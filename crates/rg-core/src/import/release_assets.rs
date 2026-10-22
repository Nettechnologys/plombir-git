//! The files of an imported release, downloaded from the source and stored the
//! way an upload stores them (card_e5197b84a524).
//!
//! The release list of a GitHub, Gitea or Forgejo source names every asset with
//! its size; the import used to deserialize that list and drop it, so a
//! release came over with its notes and none of its files. Each asset is now
//! fetched, held to the same budgets an upload is held to — the repository's
//! `[limits]` and the import's clone budget — and stored through
//! [`crate::release::service::upload_asset_to_repository_release`], which records the
//! SHA-256 of what it stores. An asset that cannot be brought over is named in
//! the task's stats with the reason, never dropped in silence.
//!
//! Where the bytes are: GitHub serves them from the API
//! (`/repos/{owner}/{repo}/releases/assets/{id}`, `Accept:
//! application/octet-stream`), answering with a redirect to a signed address on
//! another host; Gitea and Forgejo from the attachment URL on their own host.
//! The source's credential goes only to the source's own origin. Every hop is
//! followed here rather than by reqwest, so each one is checked by the same
//! guard the API client was built through, and a hop to another origin is made
//! without the credential and only over TLS.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use reqwest::header::{HeaderValue, ACCEPT, AUTHORIZATION, LOCATION};
use sea_orm::DatabaseConnection;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::import::github_client::{ApiDialect, GitHubAsset};
use crate::lfs::fetch::{send_with_retry, LfsSourceGuard};
use crate::net::HttpOrigin;

/// Redirects one download may take. GitHub takes one; a few more leave room
/// for a CDN in front of a self-hosted Gitea without letting a source loop the
/// import.
const MAX_REDIRECTS: usize = 5;

/// The repository the assets are stored for, and the account that stores them.
pub(crate) struct AssetDestination<'a> {
    pub(crate) repo_id: i64,
    pub(crate) owner: &'a str,
    pub(crate) name: &'a str,
    pub(crate) importer_id: i64,
}

/// The source API the assets are listed by, and the credential it takes.
pub(crate) struct AssetSource<'a> {
    pub(crate) dialect: ApiDialect,
    /// The API base the release list came from (`https://api.github.com`,
    /// `https://gitea.example/api/v1`).
    pub(crate) api_base_url: &'a str,
    pub(crate) owner: &'a str,
    pub(crate) repo: &'a str,
    /// The import's token; empty for a public source imported without one.
    pub(crate) token: &'a str,
}

/// One asset the import did not store, with a reason its owner can act on.
/// What the source said stays in the server log.
#[derive(Debug, serde::Serialize)]
pub struct ReleaseAssetImportFailure {
    pub tag_name: String,
    pub filename: String,
    pub reason: String,
}

pub(crate) struct ReleaseAssetFetcher<'a> {
    db: &'a DatabaseConnection,
    storage: &'a dyn crate::blob_storage::BlobStorage,
    repo_root: &'a Path,
    destination: AssetDestination<'a>,
    source: AssetSource<'a>,
    source_origin: HttpOrigin,
    authorization: Option<HeaderValue>,
    guard: &'a dyn LfsSourceGuard,
    limits: crate::storage_quota::StorageLimits,
    pass_allowance: crate::clone_budget::PassAllowance,
    clients: HashMap<HttpOrigin, reqwest::Client>,
}

impl<'a> ReleaseAssetFetcher<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        db: &'a DatabaseConnection,
        storage: &'a dyn crate::blob_storage::BlobStorage,
        repo_root: &'a Path,
        destination: AssetDestination<'a>,
        source: AssetSource<'a>,
        guard: &'a dyn LfsSourceGuard,
        limits: crate::storage_quota::StorageLimits,
        pass_allowance: crate::clone_budget::PassAllowance,
    ) -> Result<Self> {
        let source_origin = HttpOrigin::from_target(source.api_base_url)
            .context("the import's API base has no HTTP(S) origin")?;
        let authorization = (!source.token.is_empty())
            .then(|| {
                let scheme = match source.dialect {
                    ApiDialect::GitHub => "Bearer",
                    ApiDialect::Gitea => "token",
                };
                HeaderValue::from_str(&format!("{scheme} {}", source.token))
                    .context("invalid import auth token: not a valid HTTP header value")
                    .map(|mut value| {
                        value.set_sensitive(true);
                        value
                    })
            })
            .transpose()?;
        Ok(Self {
            db,
            storage,
            repo_root,
            destination,
            source,
            source_origin,
            authorization,
            guard,
            limits,
            pass_allowance,
            clients: HashMap::new(),
        })
    }

    /// Store every asset of the release `release_id` (tagged `tag_name`),
    /// counting each one in `stats`. `Err` is this server's failure — its
    /// database or blob store — and ends the import; whatever the source or
    /// a budget refused is one of `stats.release_assets_failed`.
    pub(crate) async fn import(
        &mut self,
        release_id: i64,
        tag_name: &str,
        assets: &[GitHubAsset],
        stats: &mut crate::import::service::ImportStats,
    ) -> Result<()> {
        for asset in assets {
            match self.fetch_one(release_id, asset).await? {
                Ok(()) => stats.release_assets_imported += 1,
                Err(reason) => {
                    tracing::warn!(
                        tag = %tag_name,
                        asset = %asset.name,
                        %reason,
                        "a release asset was not imported"
                    );
                    stats.release_assets_failed.push(ReleaseAssetImportFailure {
                        tag_name: tag_name.to_owned(),
                        filename: asset.name.clone(),
                        reason,
                    });
                }
            }
        }
        Ok(())
    }

    async fn fetch_one(
        &mut self,
        release_id: i64,
        asset: &GitHubAsset,
    ) -> Result<std::result::Result<(), String>> {
        let Ok(size) = u64::try_from(asset.size) else {
            return Ok(Err("the source declared a negative size".to_owned()));
        };
        if let Err(error) =
            crate::platform::path::validate_upload_filename("release asset", &asset.name)
        {
            return refused_by_rule(error);
        }
        if let Some(max_assets) = self.limits.release_assets_max_per_release {
            if rg_db::ops::release_ops::count_assets(self.db, release_id).await? >= max_assets {
                return Ok(Err(format!(
                    "the release already holds the maximum of {max_assets} asset(s) \
                     ([limits].release_assets_max_per_release)"
                )));
            }
        }
        // Checked per asset, before a byte is fetched: everything stored before
        // this one is already in the sum.
        match crate::storage_quota::check_room(
            self.db,
            self.destination.repo_id,
            size,
            &self.limits,
        )
        .await
        {
            Ok(()) => {}
            Err(crate::storage_quota::QuotaError::Exceeded(exceeded)) => {
                return Ok(Err(exceeded.message));
            }
            Err(crate::storage_quota::QuotaError::Db(error)) => return Err(error),
        }
        if let Err(reason) = self.pass_allowance.admit(size) {
            return Ok(Err(reason));
        }

        let (url, accept) = match self.source.dialect {
            ApiDialect::GitHub => (
                format!(
                    "{}/repos/{}/{}/releases/assets/{}",
                    self.source.api_base_url.trim_end_matches('/'),
                    self.source.owner,
                    self.source.repo,
                    asset.id
                ),
                "application/octet-stream",
            ),
            ApiDialect::Gitea => (asset.browser_download_url.clone(), "*/*"),
        };
        let Ok(url) = reqwest::Url::parse(&url) else {
            return Ok(Err("the source's download address is not a URL".to_owned()));
        };
        let response = match self.open(url, accept).await {
            Ok(response) => response,
            Err(reason) => return Ok(Err(reason)),
        };
        if let Some(length) = response.content_length().filter(|length| *length != size) {
            return Ok(Err(format!(
                "the source announced {length} bytes; its release lists {size}"
            )));
        }

        let spool_dir = crate::staging::StagingArea::ReleaseUploads.path_in(self.repo_root);
        tokio::fs::create_dir_all(&spool_dir)
            .await
            .with_context(|| format!("failed to create {}", spool_dir.display()))?;
        let spool = tempfile::Builder::new()
            .prefix("import-asset-")
            .tempfile_in(&spool_dir)
            .with_context(|| format!("failed to create a spool in {}", spool_dir.display()))?;
        let sha256 = match stream_to_spool(response, spool.path(), size).await? {
            Ok(sha256) => sha256,
            Err(reason) => return Ok(Err(reason)),
        };
        if let Some(expected) = asset
            .digest
            .as_deref()
            .and_then(|digest| digest.strip_prefix("sha256:"))
        {
            if !expected.eq_ignore_ascii_case(&sha256) {
                return Ok(Err(
                    "the content the source sent does not match the SHA-256 it lists".to_owned(),
                ));
            }
        }

        match crate::release::service::upload_asset_to_repository_release(
            self.db,
            self.destination.repo_id,
            release_id,
            self.storage,
            self.destination.owner,
            self.destination.name,
            &asset.name,
            crate::platform::upload_type::passive_upload_type(&asset.content_type),
            self.destination.importer_id,
            spool.path(),
            size,
        )
        .await
        {
            Ok(_) => {}
            // A name the store refuses is the asset's, not this server's.
            Err(error) if error.is::<crate::error::InvalidRequest>() => {
                return refused_by_rule(error);
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to store release asset {} imported for {}/{}",
                        asset.name, self.destination.owner, self.destination.name
                    )
                });
            }
        }
        self.pass_allowance.charge(size);
        Ok(Ok(()))
    }

    /// The source's answer to a GET of `url`, following its redirects one hop
    /// at a time, or why the download cannot be had.
    async fn open(
        &mut self,
        mut url: reqwest::Url,
        accept: &'static str,
    ) -> std::result::Result<reqwest::Response, String> {
        for _ in 0..=MAX_REDIRECTS {
            let origin = HttpOrigin::from_url(&url)
                .ok_or_else(|| "the source's download address is not an HTTP(S) URL".to_owned())?;
            let same_origin = origin == self.source_origin;
            if !same_origin && url.scheme() != "https" {
                return Err(
                    "the source sent the download to a plaintext address on another origin"
                        .to_owned(),
                );
            }
            let client = self.client_for(&url, origin)?;
            let authorization = self.authorization.clone().filter(|_| same_origin);
            let response = send_with_retry(|| {
                let request = client.get(url.clone()).header(ACCEPT, accept);
                match &authorization {
                    Some(value) => request.header(AUTHORIZATION, value.clone()),
                    None => request,
                }
            })
            .await
            .map_err(|error| {
                tracing::warn!(
                    host = url.host_str().unwrap_or_default(),
                    error = %format!("{error:#}"),
                    "a release asset download from the source could not be started"
                );
                "the source's download address could not be reached".to_owned()
            })?;

            let status = response.status();
            if status.is_redirection() {
                let next = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|location| location.to_str().ok())
                    .and_then(|location| url.join(location).ok())
                    .ok_or_else(|| {
                        format!("the source answered {status} without a usable address")
                    })?;
                url = next;
                continue;
            }
            if status != reqwest::StatusCode::OK {
                return Err(format!("the source answered {status} to the download"));
            }
            return Ok(response);
        }
        Err("the source redirected the download too many times".to_owned())
    }

    fn client_for(
        &mut self,
        url: &reqwest::Url,
        origin: HttpOrigin,
    ) -> std::result::Result<reqwest::Client, String> {
        if let Some(client) = self.clients.get(&origin) {
            return Ok(client.clone());
        }
        let client = self
            .guard
            .client_for(url.as_str())
            .and_then(|builder| {
                builder
                    .redirect(reqwest::redirect::Policy::none())
                    .timeout(crate::lfs::fetch::DOWNLOAD_TIMEOUT)
                    .read_timeout(crate::lfs::fetch::DOWNLOAD_IDLE_TIMEOUT)
                    .user_agent("PlombirGit/0.1")
                    .build()
                    .context("failed to build the release asset client")
            })
            .map_err(|error| {
                tracing::warn!(
                    host = url.host_str().unwrap_or_default(),
                    error = %format!("{error:#}"),
                    "refused a release asset address the source chose"
                );
                "the source's download address points where this server may not connect".to_owned()
            })?;
        self.clients.insert(origin, client.clone());
        Ok(client)
    }
}

/// An asset a rule of this server refuses: the rule's own fixed text is the
/// reason, never the chain around it. Anything else is this server's failure.
fn refused_by_rule(error: anyhow::Error) -> Result<std::result::Result<(), String>> {
    match error.downcast_ref::<crate::error::InvalidRequest>() {
        Some(invalid) => Ok(Err(invalid.message.clone())),
        None => Err(error),
    }
}

/// Stream `response` into `path`, hashing as it goes and never accepting more
/// than `expected` bytes. The outer `Err` is the local disk's; the inner one
/// the source's.
async fn stream_to_spool(
    mut response: reqwest::Response,
    path: &Path,
    expected: u64,
) -> Result<std::result::Result<String, String>> {
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .await
        .with_context(|| format!("failed to open release asset spool {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut written = 0_u64;
    loop {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(
                    error = %format!("{error:#}"),
                    "a release asset download from the source broke off"
                );
                return Ok(Err("the source stopped sending the file".to_owned()));
            }
        };
        written += chunk.len() as u64;
        if written > expected {
            return Ok(Err(format!(
                "the source sent more than the {expected} bytes its release lists"
            )));
        }
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .with_context(|| format!("failed to write release asset spool {}", path.display()))?;
    }
    file.flush()
        .await
        .with_context(|| format!("failed to write release asset spool {}", path.display()))?;
    if written != expected {
        return Ok(Err(format!(
            "the source sent {written} of the {expected} bytes its release lists"
        )));
    }
    Ok(Ok(hex::encode(hasher.finalize())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use sea_orm::{ConnectionTrait, Database, Statement};
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    type Handler = Arc<dyn Fn(&str) -> Vec<u8> + Send + Sync>;

    /// A raw-socket stand-in for a source forge: one request per connection,
    /// answered by `handler` from the request head, every head recorded.
    async fn serve(handler: Handler) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind source");
        let origin = format!("http://{}", listener.local_addr().expect("address"));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut bytes = Vec::new();
                let mut buffer = [0_u8; 4096];
                while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    let read = stream.read(&mut buffer).await.expect("read request");
                    if read == 0 {
                        break;
                    }
                    bytes.extend_from_slice(&buffer[..read]);
                }
                let head = String::from_utf8_lossy(&bytes).into_owned();
                let response = handler(&head);
                recorded.lock().expect("requests").push(head);
                stream.write_all(&response).await.expect("write response");
                stream.shutdown().await.expect("close");
            }
        });
        (origin, seen)
    }

    fn ok(body: &[u8], length: usize) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\n\
             Content-Length: {length}\r\nConnection: close\r\n\r\n"
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    fn status(line: &str, extra: &str) -> Vec<u8> {
        format!("HTTP/1.1 {line}\r\n{extra}Content-Length: 0\r\nConnection: close\r\n\r\n")
            .into_bytes()
    }

    fn path_of(head: &str) -> &str {
        head.split_whitespace().nth(1).unwrap_or_default()
    }

    fn header<'h>(head: &'h str, name: &str) -> Option<&'h str> {
        head.lines().skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
        })
    }

    fn sha256_hex(payload: &[u8]) -> String {
        hex::encode(Sha256::digest(payload))
    }

    fn asset(id: i64, name: &str, payload: &[u8], url: String) -> GitHubAsset {
        GitHubAsset {
            id,
            name: name.to_owned(),
            content_type: "application/gzip".to_owned(),
            size: payload.len() as i64,
            download_count: 0,
            browser_download_url: url,
            created_at: "2024-01-01T00:00:00Z".to_owned(),
            digest: None,
        }
    }

    struct Fixture {
        db: DatabaseConnection,
        root: tempfile::TempDir,
        release_id: i64,
    }

    async fn fixture() -> Fixture {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        for statement in [
            "INSERT INTO users(id, username, email, password_hash, is_admin, is_active, created_at, updated_at) \
             VALUES(2, 'importer', 'importer@example.invalid', 'x', 0, 1, '2024-01-01', '2024-01-01')",
            "INSERT INTO repositories(id, owner_id, name, is_private, default_branch, stars_count, forks_count, created_at, updated_at) \
             VALUES(1, 2, 'imported', 0, 'main', 0, 0, '2024-01-01', '2024-01-01')",
        ] {
            db.execute(Statement::from_string(
                sea_orm::DatabaseBackend::Sqlite,
                statement.to_owned(),
            ))
            .await
            .unwrap();
        }
        let root = tempfile::tempdir().unwrap();
        let release = crate::release::service::create_release(
            &db,
            1,
            2,
            "v1",
            "v1",
            None,
            "v1",
            false,
            false,
            root.path(),
        )
        .await
        .unwrap();
        Fixture {
            db,
            root,
            release_id: release.id,
        }
    }

    async fn import(
        fixture: &Fixture,
        dialect: ApiDialect,
        source: &str,
        trusted: &[String],
        assets: &[GitHubAsset],
    ) -> crate::import::service::ImportStats {
        let storage = crate::blob_storage::instance_blob_storage(fixture.root.path());
        let guard = crate::import::trust::TrustedImportOrigins::parse(trusted).unwrap();
        let api_base_url = match dialect {
            ApiDialect::GitHub => source.to_owned(),
            ApiDialect::Gitea => format!("{source}/api/v1"),
        };
        let mut fetcher = ReleaseAssetFetcher::new(
            &fixture.db,
            &storage,
            fixture.root.path(),
            AssetDestination {
                repo_id: 1,
                owner: "importer",
                name: "imported",
                importer_id: 2,
            },
            AssetSource {
                dialect,
                api_base_url: &api_base_url,
                owner: "octo",
                repo: "widgets",
                token: "tok",
            },
            &guard,
            crate::storage_quota::StorageLimits::default(),
            crate::clone_budget::PassAllowance::unlimited(),
        )
        .unwrap();
        let mut stats = crate::import::service::ImportStats::default();
        fetcher
            .import(fixture.release_id, "v1", assets, &mut stats)
            .await
            .unwrap();
        stats
    }

    /// What `GET /releases/assets/{id}` would serve for each stored asset:
    /// its name, the digest recorded on the row, and the bytes the store
    /// holds under it.
    async fn stored(fixture: &Fixture) -> Vec<(String, Option<String>, Vec<u8>)> {
        let storage = crate::blob_storage::instance_blob_storage(fixture.root.path());
        let mut out = Vec::new();
        for asset in crate::release::service::list_assets(&fixture.db, fixture.release_id)
            .await
            .unwrap()
        {
            let (asset, source) = crate::release::service::download_asset(
                &fixture.db,
                asset.id,
                &storage,
                fixture.root.path(),
                "importer",
                "imported",
            )
            .await
            .unwrap();
            let bytes = match source {
                crate::release::service::AssetSource::LocalFile { path, .. } => {
                    std::fs::read(path).unwrap()
                }
                crate::release::service::AssetSource::Buffered(bytes) => bytes,
            };
            out.push((asset.filename, asset.sha256, bytes));
        }
        out.sort();
        out
    }

    /// card_e5197b84a524: both files of a GitHub release arrive — one behind
    /// the redirect GitHub answers the asset API with — under the SHA-256 of
    /// what the source holds, and the source's credential is what fetched them.
    #[tokio::test]
    async fn a_github_releases_files_arrive_with_the_sources_digests() {
        let fixture = fixture().await;
        let first = b"first release file".to_vec();
        let second = b"the second, a little longer, release file".to_vec();
        let (one, two) = (first.clone(), second.clone());
        let (source, seen) = serve(Arc::new(move |head: &str| match path_of(head) {
            "/repos/octo/widgets/releases/assets/1" => {
                status("302 Found", "Location: /signed/1\r\n")
            }
            "/signed/1" => ok(&one, one.len()),
            "/repos/octo/widgets/releases/assets/2" => ok(&two, two.len()),
            _ => status("404 Not Found", ""),
        }))
        .await;
        let mut listed_second = asset(2, "widgets-linux.tar.gz", &second, String::new());
        listed_second.digest = Some(format!("sha256:{}", sha256_hex(&second)));

        let stats = import(
            &fixture,
            ApiDialect::GitHub,
            &source,
            std::slice::from_ref(&source),
            &[
                asset(1, "widgets.tgz", &first, String::new()),
                listed_second,
            ],
        )
        .await;

        assert!(
            stats.release_assets_failed.is_empty(),
            "{:?}",
            stats.release_assets_failed
        );
        assert_eq!(stats.release_assets_imported, 2);
        assert_eq!(
            stored(&fixture).await,
            vec![
                (
                    "widgets-linux.tar.gz".to_owned(),
                    Some(sha256_hex(&second)),
                    second
                ),
                ("widgets.tgz".to_owned(), Some(sha256_hex(&first)), first),
            ]
        );
        let seen = seen.lock().unwrap();
        for head in seen.iter() {
            assert_eq!(header(head, "authorization"), Some("Bearer tok"), "{head}");
        }
        assert!(seen.iter().any(
            |head| path_of(head) == "/repos/octo/widgets/releases/assets/1"
                && header(head, "accept") == Some("application/octet-stream")
        ));
    }

    /// The release id the fetcher is handed must be one of the destination
    /// repository's: a release of another repository is refused before a byte
    /// of the source is asked for.
    #[tokio::test]
    async fn a_release_of_another_repository_is_not_written_to() {
        let fixture = fixture().await;
        let storage = crate::blob_storage::instance_blob_storage(fixture.root.path());
        let spool = fixture.root.path().join("payload");
        std::fs::write(&spool, b"abc").unwrap();

        let error = crate::release::service::upload_asset_to_repository_release(
            &fixture.db,
            2,
            fixture.release_id,
            &storage,
            "importer",
            "imported",
            "widgets.tgz",
            "application/gzip",
            2,
            &spool,
            3,
        )
        .await
        .unwrap_err();

        assert!(error.is::<crate::error::NotFound>(), "{error:#}");
        assert!(stored(&fixture).await.is_empty());
    }

    /// Gitea and Forgejo serve the file at the attachment URL on their own
    /// host, and take the token as `token …`.
    #[tokio::test]
    async fn a_gitea_releases_file_is_fetched_from_its_attachment_url() {
        let fixture = fixture().await;
        let payload = b"gitea attachment".to_vec();
        let body = payload.clone();
        let (source, seen) = serve(Arc::new(move |head: &str| match path_of(head) {
            "/attachments/0b1c" => ok(&body, body.len()),
            _ => status("404 Not Found", ""),
        }))
        .await;

        let stats = import(
            &fixture,
            ApiDialect::Gitea,
            &source,
            std::slice::from_ref(&source),
            &[asset(
                7,
                "widgets.zip",
                &payload,
                format!("{source}/attachments/0b1c"),
            )],
        )
        .await;

        assert!(
            stats.release_assets_failed.is_empty(),
            "{:?}",
            stats.release_assets_failed
        );
        assert_eq!(
            stored(&fixture).await,
            vec![(
                "widgets.zip".to_owned(),
                Some(sha256_hex(&payload)),
                payload
            )]
        );
        assert_eq!(
            header(&seen.lock().unwrap()[0], "authorization"),
            Some("token tok")
        );
    }

    /// Every asset the import cannot bring is named with its reason, nothing
    /// half-written is left as an asset, and the ones it can bring still arrive.
    #[tokio::test]
    async fn an_asset_the_source_cannot_give_is_named_and_the_rest_arrive() {
        let fixture = fixture().await;
        let (elsewhere, elsewhere_seen) = serve(Arc::new(|_: &str| status("200 OK", ""))).await;
        let good = b"the good one".to_vec();
        let tampered = b"not what the listing hashed".to_vec();
        let short = b"short".to_vec();
        let (good_body, tampered_body, short_body) =
            (good.clone(), tampered.clone(), short.clone());
        let redirect = format!("Location: {elsewhere}/stolen\r\n");
        let (source, _) = serve(Arc::new(move |head: &str| match path_of(head) {
            "/repos/octo/widgets/releases/assets/1" => ok(&good_body, good_body.len()),
            "/repos/octo/widgets/releases/assets/2" => ok(&tampered_body, tampered_body.len()),
            "/repos/octo/widgets/releases/assets/3" => ok(&short_body, short_body.len()),
            "/repos/octo/widgets/releases/assets/4" => status("302 Found", &redirect),
            _ => status("404 Not Found", ""),
        }))
        .await;
        let mut listed_tampered = asset(2, "tampered.bin", &tampered, String::new());
        listed_tampered.digest = Some(format!("sha256:{}", sha256_hex(b"something else")));
        let mut listed_short = asset(3, "short.bin", &short, String::new());
        listed_short.size = 1000;

        let stats = import(
            &fixture,
            ApiDialect::GitHub,
            &source,
            &[source.clone(), elsewhere.clone()],
            &[
                asset(1, "good.bin", &good, String::new()),
                listed_tampered,
                listed_short,
                asset(4, "moved.bin", b"moved", String::new()),
                asset(5, "gone.bin", b"gone", String::new()),
                asset(6, "../escape.bin", b"escape", String::new()),
            ],
        )
        .await;

        assert_eq!(stats.release_assets_imported, 1);
        let failed: Vec<(&str, &str)> = stats
            .release_assets_failed
            .iter()
            .map(|failure| (failure.filename.as_str(), failure.reason.as_str()))
            .collect();
        assert_eq!(
            failed,
            [
                (
                    "tampered.bin",
                    "the content the source sent does not match the SHA-256 it lists"
                ),
                (
                    "short.bin",
                    "the source announced 5 bytes; its release lists 1000"
                ),
                (
                    "moved.bin",
                    "the source sent the download to a plaintext address on another origin"
                ),
                (
                    "gone.bin",
                    "the source answered 404 Not Found to the download"
                ),
                (
                    "../escape.bin",
                    "release asset filename must not contain a path"
                ),
            ]
        );
        assert!(stats
            .release_assets_failed
            .iter()
            .all(|failure| failure.tag_name == "v1"));
        assert!(
            elsewhere_seen.lock().unwrap().is_empty(),
            "a plaintext hop to another origin must not be made at all"
        );
        assert_eq!(
            stored(&fixture).await,
            vec![("good.bin".to_owned(), Some(sha256_hex(&good)), good)]
        );
    }
}
