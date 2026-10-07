//! Fetching LFS objects from the server a repository came from.
//!
//! `git clone --bare` copies pointer files and nothing they point at: the
//! content lives in the source's LFS store, outside Git. A repository that
//! arrives that way looks complete and is not — every LFS file in it answers
//! `404` to `git lfs pull` (card_bc7c8ddbf9b7). An import fetches once, after
//! its clone; a pull mirror after every pass that moved its refs
//! (card_f4bc7fe93859). This module asks the source's
//! batch API for each object the arrived history names, downloads it, checks it
//! against its oid and publishes it the way an upload is published.
//!
//! The endpoint is the one the git-lfs client derives from a remote URL:
//! `<remote>.git/info/lfs/objects/batch`. A committed `.lfsconfig` that names
//! another `lfs.url` is deliberately not honoured — that would let the content
//! of the repository choose where this server sends the import's credential.
//!
//! ## Where requests go
//!
//! Two addresses are involved and the source chooses one of them. The batch
//! endpoint is derived from the clone URL, which the caller has already
//! admitted. The download `href` comes from the source's answer, and is
//! treated as what it is — a URL someone else wrote: each one goes through the
//! caller's [`LfsSourceGuard`] — for an import the static check and
//! connector-owned DNS guard its API clients use (card_f958a838ef80), for a
//! mirror the same guard under the mirror's transport policy — so an `href`
//! aimed at a private address is refused before anything connects to it. One
//! on another origin must also be `https`: the plaintext opt-in an operator
//! gave the source's origin is not an opt-in for wherever it points.
//! Redirects are followed only within the origin of the request that got them.
//!
//! ## The credential
//!
//! The source's credential goes to the batch endpoint as HTTP Basic, the way `git`
//! sends it. An `href` gets it only when it is on that same origin and the
//! source did not name an `Authorization` of its own — the per-host rule the
//! git-lfs client follows. The headers the source attaches to an action go to
//! that action's `href` and nowhere else.
//!
//! ## What a failure is
//!
//! An object the source will not or cannot give is a [`LfsFetchFailure`] — the
//! oid, a path it is committed under, and a reason written here rather than
//! quoted from the source — and the remaining objects are still fetched. An
//! error returned from [`LfsFetcher::fetch`] is this server's own: a database,
//! a spool or a blob store that failed, which no other object would fare better
//! against.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::blob_storage::BlobStorage;
use crate::lfs::pointer::PointerInHistory;
use crate::lfs::service::{LfsRepository, LFS_OBJECT_MAX_BYTES};
use crate::net::HttpOrigin;
use rg_git::credentials::GitCredentials;
use sea_orm::DatabaseConnection;

/// Objects per batch request — the git-lfs client's own default
/// (`lfs.transfer.batchSize`), which every server accepts.
pub const BATCH_SIZE: usize = 100;

/// Ceiling on one batch answer. A hundred download actions with signed
/// `href`s and their headers come to well under a megabyte; anything near
/// this is not a batch answer.
const BATCH_RESPONSE_MAX_BYTES: usize = 4 * 1024 * 1024;

const LFS_MEDIA_TYPE: &str = "application/vnd.git-lfs+json";

/// How long one object may take to arrive in total. The size of an object is
/// the source's to choose up to [`LFS_OBJECT_MAX_BYTES`], so the request-wide
/// outbound timeout cannot apply; [`DOWNLOAD_IDLE_TIMEOUT`] is what catches a
/// source that has stopped sending.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(6 * 60 * 60);

/// How long a download may go without receiving a byte.
const DOWNLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Tries per request before a transient refusal — a connection that failed,
/// `429`, `5xx` — counts as the object's failure.
const ATTEMPTS: u32 = 3;
const RETRY_BACKOFF: Duration = Duration::from_millis(500);

/// Where a fetch may connect, and the only client that may connect there.
///
/// The batch endpoint and every download `href` go through it before a
/// request is built. A guard answers with a client builder rather than a
/// verdict so the address it checked and the connector that dials are one
/// value: a separate yes/no would leave the DNS answer free to change between
/// the check and the connection.
pub trait LfsSourceGuard: Sync {
    /// A client builder bound to `url`'s destination, or why `url` may not be
    /// reached from this server.
    fn client_for(&self, url: &str) -> Result<reqwest::ClientBuilder>;
}

impl LfsSourceGuard for crate::import::trust::TrustedImportOrigins {
    fn client_for(&self, url: &str) -> Result<reqwest::ClientBuilder> {
        let (_, builder) = self.api_destination(url)?.into_parts();
        Ok(builder)
    }
}

/// One object the destination still does not have, and why.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct LfsFetchFailure {
    pub oid: String,
    /// A path the object's pointer is committed under.
    pub path: String,
    /// Written here, never quoted from the source: it is stored in a row the
    /// person who started the import, or who owns the mirror, reads.
    pub reason: String,
}

/// What one [`LfsFetcher::fetch`] did.
#[derive(Debug, Default)]
pub struct LfsFetchOutcome {
    /// Objects downloaded, verified and published.
    pub fetched: usize,
    /// Objects the destination already served, left alone.
    pub already_present: usize,
    pub failed: Vec<LfsFetchFailure>,
}

impl LfsFetchOutcome {
    pub fn absorb(&mut self, other: LfsFetchOutcome) {
        self.fetched += other.fetched;
        self.already_present += other.already_present;
        self.failed.extend(other.failed);
    }
}

/// Downloads LFS objects from one source into one destination repository.
pub struct LfsFetcher<'a> {
    db: &'a DatabaseConnection,
    storage: &'a dyn BlobStorage,
    repo_root: &'a std::path::Path,
    destination: LfsRepository<'a>,
    guard: &'a dyn LfsSourceGuard,
    credentials: Option<GitCredentials>,
    endpoint: reqwest::Url,
    endpoint_origin: HttpOrigin,
    batch_client: reqwest::Client,
    /// One client per download origin, each built through the guard for that
    /// origin. A source typically serves every object from one or two hosts.
    download_clients: HashMap<HttpOrigin, reqwest::Client>,
}

/// The batch endpoint the git-lfs client derives from `remote_url`.
fn batch_endpoint(remote_url: &str) -> Result<reqwest::Url> {
    let mut url = reqwest::Url::parse(remote_url.trim())
        .with_context(|| format!("the remote URL `{remote_url}` is not a URL"))?;
    if !matches!(url.scheme(), "http" | "https") {
        anyhow::bail!("LFS objects can only be fetched over HTTP(S)");
    }
    // The login rides in the Authorization header, never in the URL, and the
    // API guard refuses a URL that carries one.
    if url.set_username("").is_err() || url.set_password(None).is_err() {
        anyhow::bail!("the remote URL cannot carry an LFS endpoint");
    }
    url.set_query(None);
    url.set_fragment(None);
    let path = url.path().trim_end_matches('/');
    let repository = if path.ends_with(".git") {
        path.to_string()
    } else {
        format!("{path}.git")
    };
    url.set_path(&format!("{repository}/info/lfs/objects/batch"));
    Ok(url)
}

impl<'a> LfsFetcher<'a> {
    /// A fetcher for the source cloned from `remote_url`.
    ///
    /// Network-free: the endpoint's address is resolved by the client's guarded
    /// connector when the first batch is sent, never ahead of it.
    pub fn new(
        db: &'a DatabaseConnection,
        storage: &'a dyn BlobStorage,
        repo_root: &'a std::path::Path,
        destination: LfsRepository<'a>,
        remote_url: &str,
        credentials: Option<GitCredentials>,
        guard: &'a dyn LfsSourceGuard,
    ) -> Result<Self> {
        let endpoint = batch_endpoint(remote_url)?;
        let endpoint_origin = HttpOrigin::from_url(&endpoint)
            .context("the LFS batch endpoint has no HTTP(S) origin")?;
        let batch_client = guard
            .client_for(endpoint.as_str())?
            .redirect(crate::net::same_origin_redirect_policy())
            .user_agent("PlombirGit/0.1")
            .build()
            .context("failed to build the LFS batch client")?;
        Ok(Self {
            db,
            storage,
            repo_root,
            destination,
            guard,
            credentials,
            endpoint,
            endpoint_origin,
            batch_client,
            download_clients: HashMap::new(),
        })
    }

    /// Give the destination every object of `pointers` it does not have yet.
    pub async fn fetch(&mut self, pointers: &[PointerInHistory]) -> Result<LfsFetchOutcome> {
        let mut outcome = LfsFetchOutcome::default();
        let mut wanted = Vec::new();
        for entry in pointers {
            let oid = &entry.pointer.oid;
            if crate::lfs::service::object_claims_upload(self.db, self.destination.id, oid).await? {
                outcome.already_present += 1;
            } else if entry.pointer.size > LFS_OBJECT_MAX_BYTES as u64 {
                outcome.failed.push(failure(
                    entry,
                    format!(
                        "the pointer declares {} bytes, above this server's {LFS_OBJECT_MAX_BYTES}-byte \
                         LFS object limit",
                        entry.pointer.size
                    ),
                ));
            } else {
                wanted.push(entry);
            }
        }

        for chunk in wanted.chunks(BATCH_SIZE) {
            let answer = match self.request_batch(chunk).await {
                Ok(answer) => answer,
                Err(reason) => {
                    outcome
                        .failed
                        .extend(chunk.iter().map(|entry| failure(entry, reason.clone())));
                    continue;
                }
            };
            for entry in chunk {
                match self
                    .fetch_one(entry, answer.get(&entry.pointer.oid))
                    .await?
                {
                    Ok(()) => outcome.fetched += 1,
                    Err(reason) => {
                        tracing::warn!(
                            oid = %entry.pointer.oid,
                            path = %entry.path,
                            repo_id = self.destination.id,
                            reason,
                            "an LFS object could not be fetched from its source"
                        );
                        outcome.failed.push(failure(entry, reason));
                    }
                }
            }
        }
        Ok(outcome)
    }

    /// Ask the source for download actions for `chunk`. `Err` is the reason
    /// the whole batch failed, for every object in it.
    async fn request_batch(
        &self,
        chunk: &[&PointerInHistory],
    ) -> std::result::Result<HashMap<String, BatchObject>, String> {
        let body = serde_json::json!({
            "operation": "download",
            "transfers": ["basic"],
            "objects": chunk
                .iter()
                .map(|entry| serde_json::json!({
                    "oid": entry.pointer.oid,
                    "size": entry.pointer.size,
                }))
                .collect::<Vec<_>>(),
        })
        .to_string();

        let response = send_with_retry(|| {
            let request = self
                .batch_client
                .post(self.endpoint.clone())
                .header(reqwest::header::ACCEPT, LFS_MEDIA_TYPE)
                .header(reqwest::header::CONTENT_TYPE, LFS_MEDIA_TYPE)
                .body(body.clone());
            match &self.credentials {
                Some(credentials) => request.basic_auth(
                    credentials.username().unwrap_or_default(),
                    Some(credentials.password()),
                ),
                None => request,
            }
        })
        .await
        .map_err(|error| {
            tracing::warn!(
                endpoint = %self.endpoint_origin_label(),
                error = %format!("{error:#}"),
                "the source's LFS batch API could not be reached"
            );
            "the source's LFS batch API could not be reached".to_string()
        })?;

        let status = response.status();
        if !status.is_success() {
            return Err(match status {
                reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN => {
                    format!(
                        "the source refused this server's credential for its LFS API ({status})"
                    )
                }
                reqwest::StatusCode::NOT_FOUND => {
                    format!("the source has no LFS batch API for this repository ({status})")
                }
                reqwest::StatusCode::TOO_MANY_REQUESTS => {
                    format!("the source rate-limited the LFS batch request ({status})")
                }
                _ => format!("the source's LFS batch API answered {status}"),
            });
        }

        let bytes = read_bounded(response, BATCH_RESPONSE_MAX_BYTES)
            .await
            .map_err(|error| {
                tracing::warn!(
                    error = %format!("{error:#}"),
                    "the source's LFS batch answer could not be read"
                );
                "the source's LFS batch answer could not be read".to_string()
            })?;
        let answer: BatchResponse = serde_json::from_slice(&bytes)
            .map_err(|_| "the source's LFS batch answer is not a batch answer".to_string())?;
        if let Some(transfer) = answer.transfer.as_deref() {
            if transfer != "basic" {
                return Err(format!(
                    "the source answered with the `{transfer}` transfer adapter, which this server \
                     does not speak"
                ));
            }
        }
        Ok(answer
            .objects
            .into_iter()
            .map(|object| (object.oid.clone(), object))
            .collect())
    }

    /// Download and publish one object. The outer `Err` is this server's; the
    /// inner one is the object's reason for not arriving.
    async fn fetch_one(
        &mut self,
        entry: &PointerInHistory,
        answer: Option<&BatchObject>,
    ) -> Result<std::result::Result<(), String>> {
        let Some(answer) = answer else {
            return Ok(Err(
                "the source's batch answer did not mention this object".to_string()
            ));
        };
        if let Some(error) = &answer.error {
            return Ok(Err(match error.code {
                404 => "the source does not have this object (404)".to_string(),
                410 => "the source has removed this object (410)".to_string(),
                422 => "the source considers this object's pointer invalid (422)".to_string(),
                code => format!("the source refused to give this object ({code})"),
            }));
        }
        if answer.size.is_some_and(|size| size != entry.pointer.size) {
            return Ok(Err(
                "the source knows this object under a different size than its pointer declares"
                    .to_string(),
            ));
        }
        let Some(action) = answer
            .actions
            .as_ref()
            .and_then(|actions| actions.download.as_ref())
        else {
            return Ok(Err(
                "the source offered no download for this object".to_string()
            ));
        };

        let request = match self.download_request(action) {
            Ok(request) => request,
            Err(reason) => return Ok(Err(reason)),
        };
        let credentials = self
            .credentials
            .as_ref()
            .filter(|_| request.may_carry_credentials);
        let response = match send_with_retry(|| request.build(credentials)).await {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(
                    oid = %entry.pointer.oid,
                    error = %format!("{error:#}"),
                    "an LFS download from the source could not be started"
                );
                return Ok(Err(
                    "the source's download address could not be reached".to_string()
                ));
            }
        };
        if response.status() != reqwest::StatusCode::OK {
            return Ok(Err(format!(
                "the source answered {} to the download",
                response.status()
            )));
        }
        if response
            .content_length()
            .is_some_and(|length| length != entry.pointer.size)
        {
            return Ok(Err(format!(
                "the source announced {} bytes; the pointer declares {}",
                response.content_length().unwrap_or_default(),
                entry.pointer.size
            )));
        }

        let spool = self.spool_path(&entry.pointer.oid);
        let staged = match stream_to_spool(response, &spool, entry.pointer.size).await? {
            Ok(staged) => staged,
            Err(reason) => return Ok(Err(reason)),
        };
        if staged.sha256 != entry.pointer.oid {
            return Ok(Err(
                "the content the source sent does not match the object's oid".to_string(),
            ));
        }

        // The spool is the publication's from here: it retires the file
        // whichever way it ends.
        let path = staged.disarm();
        crate::lfs::service::store_object_from_file(
            self.db,
            self.destination.id,
            self.storage,
            self.destination.owner,
            self.destination.name,
            &entry.pointer.oid,
            &path,
            entry.pointer.size as i64,
        )
        .await
        .with_context(|| {
            format!(
                "failed to store LFS object {} fetched for {}/{}",
                entry.pointer.oid, self.destination.owner, self.destination.name
            )
        })?;
        Ok(Ok(()))
    }

    /// Prepare the download request for `action`, or say why it may not be
    /// sent.
    fn download_request(
        &mut self,
        action: &DownloadAction,
    ) -> std::result::Result<DownloadRequest, String> {
        let href = reqwest::Url::parse(&action.href)
            .map_err(|_| "the source's download address is not a URL".to_string())?;
        let origin = HttpOrigin::from_url(&href)
            .ok_or_else(|| "the source's download address is not an HTTP(S) URL".to_string())?;
        let same_origin = origin == self.endpoint_origin;
        if !same_origin && href.scheme() != "https" {
            return Err(
                "the source sent a plaintext download address on another origin".to_string(),
            );
        }

        let client = match self.download_clients.get(&origin) {
            Some(client) => client.clone(),
            None => {
                let client = self
                    .guard
                    .client_for(href.as_str())
                    .and_then(|builder| {
                        builder
                            .redirect(crate::net::same_origin_redirect_policy())
                            .timeout(DOWNLOAD_TIMEOUT)
                            .read_timeout(DOWNLOAD_IDLE_TIMEOUT)
                            .user_agent("PlombirGit/0.1")
                            .build()
                            .context("failed to build the LFS download client")
                    })
                    .map_err(|error| {
                        tracing::warn!(
                            host = href.host_str().unwrap_or_default(),
                            error = %format!("{error:#}"),
                            "refused an LFS download address the source chose"
                        );
                        "the source's download address points where this server may not connect"
                            .to_string()
                    })?;
                self.download_clients.insert(origin, client.clone());
                client
            }
        };

        let mut headers = reqwest::header::HeaderMap::new();
        for (name, value) in &action.header {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| "the source attached an invalid header to the download".to_string())?;
            let value = reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| "the source attached an invalid header to the download".to_string())?;
            headers.insert(name, value);
        }
        let names_authorization = headers.contains_key(reqwest::header::AUTHORIZATION);
        Ok(DownloadRequest {
            client,
            href,
            headers,
            may_carry_credentials: same_origin && !names_authorization,
        })
    }

    fn spool_path(&self, oid: &str) -> std::path::PathBuf {
        crate::lfs::service::lfs_root(
            self.repo_root,
            self.destination.owner,
            self.destination.name,
        )
        .join(crate::staging::lfs_object_fetch_spool_name(
            oid,
            uuid::Uuid::new_v4(),
        ))
    }

    /// The endpoint's origin, for log lines — never the URL, whose path names
    /// the source repository and whose query a source may sign.
    fn endpoint_origin_label(&self) -> String {
        self.endpoint.origin().ascii_serialization()
    }
}

/// One download, rebuilt for every try.
struct DownloadRequest {
    client: reqwest::Client,
    href: reqwest::Url,
    headers: reqwest::header::HeaderMap,
    /// Whether the source's credential may go to this `href`.
    may_carry_credentials: bool,
}

impl DownloadRequest {
    fn build(&self, credentials: Option<&GitCredentials>) -> reqwest::RequestBuilder {
        let request = self
            .client
            .get(self.href.clone())
            .headers(self.headers.clone());
        match credentials {
            Some(credentials) => request.basic_auth(
                credentials.username().unwrap_or_default(),
                Some(credentials.password()),
            ),
            None => request,
        }
    }
}

fn failure(entry: &PointerInHistory, reason: String) -> LfsFetchFailure {
    LfsFetchFailure {
        oid: entry.pointer.oid.clone(),
        path: entry.path.clone(),
        reason,
    }
}

/// Send what `build` makes, again after a pause when the answer is one a
/// later try may not repeat.
async fn send_with_retry(
    build: impl Fn() -> reqwest::RequestBuilder,
) -> reqwest::Result<reqwest::Response> {
    let mut backoff = RETRY_BACKOFF;
    let mut attempt = 1;
    loop {
        let result = build().send().await;
        let transient = match &result {
            Ok(response) => {
                let status = response.status();
                status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
            }
            Err(error) => error.is_connect() || error.is_timeout(),
        };
        if !transient || attempt >= ATTEMPTS {
            return result;
        }
        tokio::time::sleep(backoff).await;
        backoff *= 2;
        attempt += 1;
    }
}

/// Read a whole response body, refusing one larger than `max_bytes`.
async fn read_bounded(mut response: reqwest::Response, max_bytes: usize) -> Result<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        anyhow::bail!("the response announces more than {max_bytes} bytes");
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > max_bytes {
            anyhow::bail!("the response is longer than {max_bytes} bytes");
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// A spool that holds a complete download, and is removed unless it is handed
/// on.
struct StagedDownload {
    path: std::path::PathBuf,
    sha256: String,
    armed: bool,
}

impl StagedDownload {
    fn disarm(mut self) -> std::path::PathBuf {
        self.armed = false;
        std::mem::take(&mut self.path)
    }
}

impl Drop for StagedDownload {
    fn drop(&mut self) {
        if self.armed {
            crate::platform::fs::discard_file("fetched LFS object", &self.path);
        }
    }
}

/// Stream `response` into a new file at `path`, hashing as it goes and never
/// accepting more than `expected` bytes. The outer `Err` is the local disk's;
/// the inner one the source's.
async fn stream_to_spool(
    mut response: reqwest::Response,
    path: &std::path::Path,
    expected: u64,
) -> Result<std::result::Result<StagedDownload, String>> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|error| {
            crate::platform::fs::path_error(
                "LFS object directory",
                parent,
                &error,
                crate::platform::fs::LFS_STORAGE_HINT,
            )
        })?;
    }
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .await
        .map_err(|error| {
            crate::platform::fs::path_error(
                "fetched LFS object spool",
                path,
                &error,
                crate::platform::fs::LFS_STORAGE_HINT,
            )
        })?;
    // Armed from the moment the file exists, so a cancelled fetch or a
    // refused download leaves nothing behind.
    let mut staged = StagedDownload {
        path: path.to_path_buf(),
        sha256: String::new(),
        armed: true,
    };

    let mut hasher = Sha256::new();
    let mut written: u64 = 0;
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(
                    error = %format!("{error:#}"),
                    "an LFS download from the source broke off"
                );
                return Ok(Err("the download from the source broke off".to_string()));
            }
        };
        written += chunk.len() as u64;
        if written > expected {
            return Ok(Err(format!(
                "the source sent more than the {expected} bytes the pointer declares"
            )));
        }
        file.write_all(&chunk).await.map_err(|error| {
            crate::platform::fs::path_error(
                "fetched LFS object spool",
                path,
                &error,
                crate::platform::fs::LFS_STORAGE_HINT,
            )
        })?;
        hasher.update(&chunk);
    }
    // `tokio::fs::File` hands writes to the blocking pool; without the flush
    // the publication could compress a file still missing its tail.
    file.flush().await.map_err(|error| {
        crate::platform::fs::path_error(
            "fetched LFS object spool",
            path,
            &error,
            crate::platform::fs::LFS_STORAGE_HINT,
        )
    })?;
    drop(file);

    if written != expected {
        return Ok(Err(format!(
            "the source sent {written} bytes; the pointer declares {expected}"
        )));
    }
    staged.sha256 = hex::encode(hasher.finalize());
    Ok(Ok(staged))
}

#[derive(Deserialize)]
struct BatchResponse {
    #[serde(default)]
    transfer: Option<String>,
    #[serde(default)]
    objects: Vec<BatchObject>,
}

#[derive(Deserialize)]
struct BatchObject {
    oid: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    actions: Option<BatchActions>,
    #[serde(default)]
    error: Option<BatchObjectError>,
}

#[derive(Deserialize)]
struct BatchActions {
    #[serde(default)]
    download: Option<DownloadAction>,
}

#[derive(Deserialize)]
struct DownloadAction {
    href: String,
    #[serde(default)]
    header: HashMap<String, String>,
}

#[derive(Deserialize)]
struct BatchObjectError {
    code: i64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::trust::TrustedImportOrigins;
    use crate::lfs::pointer::LfsPointer;
    use sea_orm::{ConnectOptions, Database};
    use std::sync::{Arc, Mutex};
    use tokio::io::AsyncReadExt;
    use tokio::net::{TcpListener, TcpStream};

    const OWNER: &str = "importer";
    const REPO: &str = "assets";
    const TOKEN: &str = "private-import-token";

    type Handler = Arc<dyn Fn(&Request) -> Vec<u8> + Send + Sync>;

    #[derive(Clone, Debug)]
    struct Request {
        head: String,
        body: Vec<u8>,
    }

    impl Request {
        fn path(&self) -> &str {
            self.head.split(' ').nth(1).unwrap_or_default()
        }

        /// The values of `name`, compared case-insensitively.
        ///
        /// Every value, not the first: a request may carry a header twice,
        /// and the second one is sent all the same.
        fn headers(&self, name: &str) -> Vec<&str> {
            self.head
                .lines()
                .skip(1)
                .filter_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
                })
                .collect()
        }
    }

    /// A raw-socket stand-in for a source's LFS server: one request per
    /// connection, answered by `handler`, every request recorded.
    async fn serve(handler: Handler) -> (std::net::SocketAddr, Arc<Mutex<Vec<Request>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind source");
        let address = listener.local_addr().expect("source address");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let request = read_request(&mut stream).await;
                let response = handler(&request);
                recorded.lock().expect("requests lock").push(request);
                stream.write_all(&response).await.expect("write response");
                stream.shutdown().await.expect("close connection");
            }
        });
        (address, seen)
    }

    async fn read_request(stream: &mut TcpStream) -> Request {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 4096];
        let head_end = loop {
            let read = stream.read(&mut buffer).await.expect("read request");
            assert!(read > 0, "the client closed before finishing its request");
            bytes.extend_from_slice(&buffer[..read]);
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
        let length = head
            .lines()
            .find_map(|line| {
                let (key, value) = line.split_once(':')?;
                key.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())?
            })
            .unwrap_or(0);
        while bytes.len() < head_end + length {
            let read = stream.read(&mut buffer).await.expect("read body");
            assert!(read > 0, "the client closed before sending its body");
            bytes.extend_from_slice(&buffer[..read]);
        }
        Request {
            head,
            body: bytes[head_end..].to_vec(),
        }
    }

    fn respond(status: &str, content_type: &str, body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    fn oid_of(payload: &[u8]) -> String {
        hex::encode(Sha256::digest(payload))
    }

    fn pointer(oid: &str, size: usize, path: &str) -> PointerInHistory {
        PointerInHistory {
            pointer: LfsPointer {
                oid: oid.to_string(),
                size: size as u64,
            },
            path: path.to_string(),
        }
    }

    fn basic(username: &str, password: &str) -> String {
        use base64::Engine;
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
        )
    }

    async fn setup_db() -> DatabaseConnection {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = Database::connect(options).await.expect("connect");
        rg_db::run_migrations(&db).await.expect("migrate");
        db
    }

    fn trusting(address: std::net::SocketAddr) -> TrustedImportOrigins {
        TrustedImportOrigins::parse(&[format!("http://{address}")]).expect("trusted origin")
    }

    async fn stored_content(repo_root: &std::path::Path, oid: &str) -> Option<Vec<u8>> {
        let storage = crate::blob_storage::instance_blob_storage(repo_root);
        let legacy = crate::lfs::service::lfs_root(repo_root, OWNER, REPO);
        match crate::lfs::service::read_object_source(&storage, &legacy, OWNER, REPO, oid).await {
            Ok(crate::lfs::service::LfsObjectSource::Local { path, compressed }) => {
                let bytes = std::fs::read(path).expect("read stored object");
                Some(if compressed {
                    zstd::decode_all(bytes.as_slice()).expect("decompress stored object")
                } else {
                    bytes
                })
            }
            Ok(crate::lfs::service::LfsObjectSource::Bytes { .. }) => {
                panic!("local storage answered with bytes")
            }
            Err(_) => None,
        }
    }

    /// Spools are named `.fetch_<oid>.<id>` beside the objects; a fetch that
    /// finished, however it finished, leaves none of them.
    fn leftover_spools(repo_root: &std::path::Path) -> Vec<String> {
        let root = crate::lfs::service::lfs_root(repo_root, OWNER, REPO);
        let Ok(entries) = std::fs::read_dir(&root) else {
            return Vec::new();
        };
        entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".fetch_"))
            .collect()
    }

    async fn fetch_with(
        db: &DatabaseConnection,
        repo_root: &std::path::Path,
        remote_url: &str,
        trusted: &TrustedImportOrigins,
        pointers: &[PointerInHistory],
    ) -> LfsFetchOutcome {
        let storage = crate::blob_storage::instance_blob_storage(repo_root);
        let mut fetcher = LfsFetcher::new(
            db,
            &storage,
            repo_root,
            LfsRepository {
                id: 1,
                owner: OWNER,
                name: REPO,
            },
            remote_url,
            Some(GitCredentials::token("x-access-token", TOKEN)),
            trusted,
        )
        .expect("build fetcher");
        fetcher
            .fetch(pointers)
            .await
            .expect("a source's refusals are not this server's error")
    }

    #[test]
    fn the_batch_endpoint_is_the_one_git_lfs_derives_from_the_remote() {
        for (remote, endpoint) in [
            (
                "https://github.com/team/widgets",
                "https://github.com/team/widgets.git/info/lfs/objects/batch",
            ),
            (
                "https://github.com/team/widgets.git",
                "https://github.com/team/widgets.git/info/lfs/objects/batch",
            ),
            (
                "https://gitlab.example/group/sub/widgets.git/",
                "https://gitlab.example/group/sub/widgets.git/info/lfs/objects/batch",
            ),
            // The login travels in the Authorization header, never the URL.
            (
                "https://alice@git.example:8443/widgets.git?ref=main#readme",
                "https://git.example:8443/widgets.git/info/lfs/objects/batch",
            ),
        ] {
            assert_eq!(batch_endpoint(remote).expect(remote).as_str(), endpoint);
        }
        assert!(batch_endpoint("ssh://git@git.example/widgets.git").is_err());
    }

    /// The acceptance of card_bc7c8ddbf9b7 at the protocol level: objects the
    /// source gives arrive, verified and served; one it refuses and one whose
    /// bytes do not match their oid are named by oid and path, and neither
    /// stops the other objects.
    #[tokio::test]
    async fn every_object_the_source_gives_is_stored_and_every_refusal_is_named() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_root = dir.path();
        let db = setup_db().await;

        let plain = b"texture bytes the source serves with its own grant".to_vec();
        let granted = b"model bytes the source serves behind its own Authorization".to_vec();
        let corrupt = b"what the pointer promised".to_vec();
        let (plain_oid, granted_oid, corrupt_oid) =
            (oid_of(&plain), oid_of(&granted), oid_of(&corrupt));
        let missing_oid = "d".repeat(64);

        let address = Arc::new(Mutex::new(None::<std::net::SocketAddr>));
        let handler: Handler = {
            let address = Arc::clone(&address);
            let (plain, granted) = (plain.clone(), granted.clone());
            let (plain_oid, granted_oid, corrupt_oid, missing_oid) = (
                plain_oid.clone(),
                granted_oid.clone(),
                corrupt_oid.clone(),
                missing_oid.clone(),
            );
            Arc::new(move |request: &Request| {
                let origin = format!("http://{}", address.lock().unwrap().expect("address"));
                match request.path() {
                    "/team/assets.git/info/lfs/objects/batch" => {
                        let answer = serde_json::json!({
                            "transfer": "basic",
                            "objects": [
                                {"oid": plain_oid, "size": plain.len(), "actions": {"download": {
                                    "href": format!("{origin}/media/{plain_oid}"),
                                    "header": {"X-Source-Grant": "grant-for-plain"},
                                }}},
                                {"oid": granted_oid, "size": granted.len(), "actions": {"download": {
                                    "href": format!("{origin}/media/{granted_oid}"),
                                    "header": {"Authorization": "RemoteToken source-chosen"},
                                }}},
                                {"oid": corrupt_oid, "size": 25, "actions": {"download": {
                                    "href": format!("{origin}/media/{corrupt_oid}"),
                                }}},
                                {"oid": missing_oid, "size": 7, "error": {
                                    "code": 404, "message": "Object does not exist on the server",
                                }},
                            ],
                        });
                        respond("200 OK", LFS_MEDIA_TYPE, answer.to_string().as_bytes())
                    }
                    path if path == format!("/media/{plain_oid}") => {
                        respond("200 OK", "application/octet-stream", &plain)
                    }
                    path if path == format!("/media/{granted_oid}") => {
                        respond("200 OK", "application/octet-stream", &granted)
                    }
                    // Same length as the pointer declares, different bytes.
                    path if path == format!("/media/{corrupt_oid}") => respond(
                        "200 OK",
                        "application/octet-stream",
                        b"what the source sent inst",
                    ),
                    other => panic!("unexpected request for {other}"),
                }
            })
        };
        let (source, requests) = serve(handler).await;
        *address.lock().unwrap() = Some(source);

        let pointers = vec![
            pointer(&plain_oid, plain.len(), "textures/plain.png"),
            pointer(&granted_oid, granted.len(), "models/granted.fbx"),
            pointer(&corrupt_oid, corrupt.len(), "audio/corrupt.wav"),
            pointer(&missing_oid, 7, "video/missing.mp4"),
        ];
        let outcome = fetch_with(
            &db,
            repo_root,
            &format!("http://{source}/team/assets.git"),
            &trusting(source),
            &pointers,
        )
        .await;

        assert_eq!(outcome.fetched, 2, "failed: {:?}", outcome.failed);
        let failed: HashMap<_, _> = outcome
            .failed
            .iter()
            .map(|failure| (failure.oid.as_str(), failure))
            .collect();
        assert_eq!(failed.len(), 2, "{:?}", outcome.failed);
        assert_eq!(failed[missing_oid.as_str()].path, "video/missing.mp4");
        assert!(failed[missing_oid.as_str()].reason.contains("404"));
        assert!(
            !failed[missing_oid.as_str()]
                .reason
                .contains("does not exist on the server"),
            "the source's own text must not reach the task row"
        );
        assert_eq!(failed[corrupt_oid.as_str()].path, "audio/corrupt.wav");
        assert!(failed[corrupt_oid.as_str()]
            .reason
            .contains("does not match"));

        assert_eq!(stored_content(repo_root, &plain_oid).await, Some(plain));
        assert_eq!(stored_content(repo_root, &granted_oid).await, Some(granted));
        assert_eq!(stored_content(repo_root, &corrupt_oid).await, None);
        for (oid, uploaded) in [
            (&plain_oid, true),
            (&granted_oid, true),
            (&corrupt_oid, false),
            (&missing_oid, false),
        ] {
            assert_eq!(
                crate::lfs::service::object_claims_upload(&db, 1, oid)
                    .await
                    .unwrap(),
                uploaded,
                "row of {oid}"
            );
        }
        assert!(
            leftover_spools(repo_root).is_empty(),
            "spools left behind: {:?}",
            leftover_spools(repo_root)
        );

        let requests = requests.lock().unwrap().clone();
        let batch = requests
            .iter()
            .find(|request| request.path().ends_with("/objects/batch"))
            .expect("one batch request");
        assert_eq!(
            batch.headers("authorization"),
            vec![basic("x-access-token", TOKEN).as_str()]
        );
        assert_eq!(batch.headers("accept"), vec![LFS_MEDIA_TYPE]);
        let asked: serde_json::Value = serde_json::from_slice(&batch.body).expect("batch body");
        assert_eq!(asked["operation"], "download");
        assert_eq!(asked["objects"].as_array().map(Vec::len), Some(4));

        let download = |oid: &str| {
            requests
                .iter()
                .find(|request| request.path() == format!("/media/{oid}"))
                .unwrap_or_else(|| panic!("no download of {oid}"))
                .clone()
        };
        let plain_download = download(&plain_oid);
        assert_eq!(
            plain_download.headers("x-source-grant"),
            vec!["grant-for-plain"]
        );
        assert_eq!(
            plain_download.headers("authorization"),
            vec![basic("x-access-token", TOKEN).as_str()],
            "a same-origin href with no Authorization of its own gets the import's credential"
        );
        assert_eq!(
            download(&granted_oid).headers("authorization"),
            vec!["RemoteToken source-chosen"],
            "the source's own Authorization replaces the import's credential, not joins it"
        );
    }

    /// The `href` is the source's choice. One aimed at a private address is
    /// refused by the guard before any connection is made, and a plaintext
    /// one on another origin is refused outright; the object is named and the
    /// fetch carries on.
    #[tokio::test]
    async fn an_href_the_server_may_not_reach_is_refused_before_anything_connects() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_root = dir.path();
        let db = setup_db().await;

        let sink = TcpListener::bind("127.0.0.1:0").await.expect("bind sink");
        let sink_address = sink.local_addr().expect("sink address");
        let (private_oid, plaintext_oid) = ("a".repeat(64), "b".repeat(64));
        let handler: Handler = {
            let (private_oid, plaintext_oid) = (private_oid.clone(), plaintext_oid.clone());
            Arc::new(move |request: &Request| {
                assert!(
                    request.path().ends_with("/objects/batch"),
                    "{}",
                    request.path()
                );
                let answer = serde_json::json!({"objects": [
                    {"oid": private_oid, "size": 3, "actions": {"download": {
                        "href": format!("https://{sink_address}/latest/meta-data"),
                    }}},
                    {"oid": plaintext_oid, "size": 3, "actions": {"download": {
                        "href": format!("http://{sink_address}/object"),
                    }}},
                ]});
                respond("200 OK", LFS_MEDIA_TYPE, answer.to_string().as_bytes())
            })
        };
        let (source, requests) = serve(handler).await;

        let outcome = fetch_with(
            &db,
            repo_root,
            &format!("http://{source}/team/assets"),
            &trusting(source),
            &[
                pointer(&private_oid, 3, "a.bin"),
                pointer(&plaintext_oid, 3, "b.bin"),
            ],
        )
        .await;

        assert_eq!(outcome.fetched, 0);
        let reasons: HashMap<_, _> = outcome
            .failed
            .iter()
            .map(|failure| (failure.oid.clone(), failure.reason.clone()))
            .collect();
        assert!(
            reasons[&private_oid].contains("may not connect"),
            "{}",
            reasons[&private_oid]
        );
        assert!(
            reasons[&plaintext_oid].contains("plaintext"),
            "{}",
            reasons[&plaintext_oid]
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(200), sink.accept())
                .await
                .is_err(),
            "the server connected to an address the source chose and the guard forbids"
        );
        assert_eq!(requests.lock().unwrap().len(), 1, "only the batch request");
        assert!(leftover_spools(repo_root).is_empty());
    }

    /// A batch the source refuses as a whole fails each of its objects, by
    /// name, rather than the import.
    #[tokio::test]
    async fn a_refused_batch_names_every_object_in_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db = setup_db().await;
        let handler: Handler =
            Arc::new(|_: &Request| respond("401 Unauthorized", "application/json", b"{}"));
        let (source, _) = serve(handler).await;

        let pointers = vec![
            pointer(&"1".repeat(64), 5, "one.bin"),
            pointer(&"2".repeat(64), 5, "two.bin"),
        ];
        let outcome = fetch_with(
            &db,
            dir.path(),
            &format!("http://{source}/team/assets.git"),
            &trusting(source),
            &pointers,
        )
        .await;

        assert_eq!(outcome.fetched, 0);
        assert_eq!(
            outcome
                .failed
                .iter()
                .map(|failure| failure.path.as_str())
                .collect::<Vec<_>>(),
            vec!["one.bin", "two.bin"]
        );
        assert!(outcome
            .failed
            .iter()
            .all(|failure| failure.reason.contains("refused this server's credential")));
    }
}
