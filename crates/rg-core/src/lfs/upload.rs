//! Uploading LFS objects to another server — what a push mirror sends beside
//! its refs.
//!
//! `git push` moves pointer files and nothing they point at, so a push mirror
//! of a repository with LFS used to leave the remote with pointers whose
//! objects were nowhere there: `git lfs pull` from the mirror answered `404`
//! (card_c5bd09ee37e9). This module is the opposite of [`crate::lfs::fetch`]:
//! it asks the remote's batch API for `upload` actions, sends this server's
//! copy of every object the remote does not have yet, and confirms each one
//! through `verify` when the remote asks for it — what the git-lfs client's
//! pre-push hook does.
//!
//! ## Where requests go, and with what
//!
//! The rules of [`crate::lfs::fetch`], through the same
//! [`LfsBatchClient`]: the batch endpoint is derived from the mirror's URL, an
//! `href` the remote answers with goes through the caller's
//! [`LfsSourceGuard`] before anything connects to it, one on another origin
//! must be `https`, and the mirror's credential goes to an `href` only on the
//! endpoint's own origin when the action names no `Authorization` of its own.
//!
//! ## What a failure is
//!
//! An object that did not reach the remote — this server has no copy of it,
//! the remote refused it, the transfer broke off — is a [`LfsFetchFailure`]
//! with the oid, a path it is committed under and a reason written here, and
//! the remaining objects are still sent. An error returned from
//! [`LfsUploader::upload`] is this server's own: a blob store that could not be
//! read.

use std::path::Path;

use anyhow::Result;
use rg_git::credentials::GitCredentials;

use crate::blob_storage::BlobStorage;
use crate::lfs::fetch::{
    failure, BatchObject, LfsBatchClient, LfsFetchFailure, LfsSourceGuard, ATTEMPTS, BATCH_SIZE,
    LFS_MEDIA_TYPE, RETRY_BACKOFF,
};
use crate::lfs::pointer::PointerInHistory;
use crate::lfs::service::{LfsObjectSource, LfsRepository};

/// What one [`LfsUploader::upload`] did.
#[derive(Debug, Default)]
pub struct LfsUploadOutcome {
    /// Objects sent to the remote (and confirmed, when it asked for that).
    pub uploaded: usize,
    /// Objects the remote said it already had.
    pub already_present: usize,
    pub failed: Vec<LfsFetchFailure>,
}

/// Sends the LFS objects of one repository on this server to one remote.
pub struct LfsUploader<'a> {
    storage: &'a dyn BlobStorage,
    repo_root: &'a Path,
    source: LfsRepository<'a>,
    batch: LfsBatchClient<'a>,
}

/// How one object ended up on the remote.
enum Sent {
    Uploaded,
    AlreadyThere,
}

impl<'a> LfsUploader<'a> {
    /// An uploader from `source` to the repository at `remote_url`.
    /// Network-free, like [`LfsBatchClient::new`].
    pub fn new(
        storage: &'a dyn BlobStorage,
        repo_root: &'a Path,
        source: LfsRepository<'a>,
        remote_url: &str,
        credentials: Option<GitCredentials>,
        guard: &'a dyn LfsSourceGuard,
    ) -> Result<Self> {
        Ok(Self {
            storage,
            repo_root,
            source,
            batch: LfsBatchClient::new("the push mirror", remote_url, credentials, guard)?,
        })
    }

    /// Give the remote every object of `pointers` it does not have yet.
    pub async fn upload(&mut self, pointers: &[PointerInHistory]) -> Result<LfsUploadOutcome> {
        let mut outcome = LfsUploadOutcome::default();
        for chunk in pointers.chunks(BATCH_SIZE) {
            let entries = chunk.iter().collect::<Vec<_>>();
            let answer = match self.batch.request("upload", &entries).await {
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
                    .upload_one(entry, answer.get(&entry.pointer.oid))
                    .await?
                {
                    Ok(Sent::Uploaded) => outcome.uploaded += 1,
                    Ok(Sent::AlreadyThere) => outcome.already_present += 1,
                    Err(reason) => {
                        tracing::warn!(
                            oid = %entry.pointer.oid,
                            path = %entry.path,
                            repo_id = self.source.id,
                            reason,
                            "an LFS object could not be sent to a push mirror"
                        );
                        outcome.failed.push(failure(entry, reason));
                    }
                }
            }
        }
        Ok(outcome)
    }

    /// Send one object. The outer `Err` is this server's; the inner one is
    /// the object's reason for not arriving.
    async fn upload_one(
        &mut self,
        entry: &PointerInHistory,
        answer: Option<&BatchObject>,
    ) -> Result<std::result::Result<Sent, String>> {
        let Some(answer) = answer else {
            return Ok(Err(
                "the push mirror's batch answer did not mention this object".to_string(),
            ));
        };
        if let Some(error) = &answer.error {
            return Ok(Err(match error.code {
                413 => "the push mirror refused this object as too large (413)".to_string(),
                422 => "the push mirror considers this object's pointer invalid (422)".to_string(),
                code => format!("the push mirror refused to take this object ({code})"),
            }));
        }
        let actions = answer.actions.as_ref();
        // No `upload` action is how the batch API says "I have it".
        let Some(upload) = actions.and_then(|actions| actions.upload.as_ref()) else {
            return Ok(Ok(Sent::AlreadyThere));
        };
        let request = match self.batch.prepare(upload, "upload") {
            Ok(request) => request,
            Err(reason) => return Ok(Err(reason)),
        };

        let mut backoff = RETRY_BACKOFF;
        let mut attempt = 1;
        let response = loop {
            // Built again for every try: a body is a stream, and a stream that
            // went out once is spent.
            let body = match self.object_body(entry).await? {
                Ok(body) => body,
                Err(reason) => return Ok(Err(reason)),
            };
            let result = request
                .build(reqwest::Method::PUT, self.batch.credentials())
                .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
                // A length rather than a chunked body: object stores behind a
                // signed `href` refuse an upload that does not announce one.
                .header(reqwest::header::CONTENT_LENGTH, entry.pointer.size)
                .body(body)
                .send()
                .await;
            let transient = match &result {
                Ok(response) => {
                    let status = response.status();
                    status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
                }
                Err(error) => error.is_connect() || error.is_timeout(),
            };
            if !transient || attempt >= ATTEMPTS {
                break result;
            }
            tokio::time::sleep(backoff).await;
            backoff *= 2;
            attempt += 1;
        };
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                tracing::warn!(
                    oid = %entry.pointer.oid,
                    error = %format!("{error:#}"),
                    "an LFS upload to a push mirror did not complete"
                );
                return Ok(Err(
                    "the upload to the push mirror did not complete".to_string()
                ));
            }
        };
        if !response.status().is_success() {
            return Ok(Err(format!(
                "the push mirror answered {} to the upload",
                response.status()
            )));
        }

        let Some(verify) = actions.and_then(|actions| actions.verify.as_ref()) else {
            return Ok(Ok(Sent::Uploaded));
        };
        let request = match self.batch.prepare(verify, "verify") {
            Ok(request) => request,
            Err(reason) => return Ok(Err(reason)),
        };
        let body = serde_json::json!({
            "oid": entry.pointer.oid,
            "size": entry.pointer.size,
        })
        .to_string();
        let response = crate::lfs::fetch::send_with_retry(|| {
            request
                .build(reqwest::Method::POST, self.batch.credentials())
                .header(reqwest::header::ACCEPT, LFS_MEDIA_TYPE)
                .header(reqwest::header::CONTENT_TYPE, LFS_MEDIA_TYPE)
                .body(body.clone())
        })
        .await;
        match response {
            Ok(response) if response.status().is_success() => Ok(Ok(Sent::Uploaded)),
            Ok(response) => Ok(Err(format!(
                "the push mirror did not confirm the upload ({})",
                response.status()
            ))),
            Err(error) => {
                tracing::warn!(
                    oid = %entry.pointer.oid,
                    error = %format!("{error:#}"),
                    "a push mirror's LFS verify address could not be reached"
                );
                Ok(Err(
                    "the push mirror's verify address could not be reached".to_string()
                ))
            }
        }
    }

    /// This server's copy of `entry`, as a request body that streams it. The
    /// outer `Err` is the blob store's; the inner one says this server has no
    /// usable copy.
    async fn object_body(
        &self,
        entry: &PointerInHistory,
    ) -> Result<std::result::Result<reqwest::Body, String>> {
        let oid = &entry.pointer.oid;
        let expected = entry.pointer.size;
        let legacy =
            crate::lfs::service::lfs_root(self.repo_root, self.source.owner, self.source.name);
        let source = match crate::lfs::service::read_object_source(
            self.storage,
            &legacy,
            self.source.owner,
            self.source.name,
            oid,
        )
        .await
        {
            Ok(source) => source,
            Err(error) if is_absent(&error) => {
                return Ok(Err("this server does not have this object".to_string()))
            }
            Err(error) => return Err(error),
        };
        let mismatch = |found: u64| {
            format!("this server's copy is {found} bytes; the pointer declares {expected}")
        };
        match source {
            LfsObjectSource::Local {
                path,
                compressed: false,
            } => {
                let file = tokio::fs::File::open(&path).await.map_err(|error| {
                    crate::platform::fs::path_error(
                        "LFS object file",
                        &path,
                        &error,
                        crate::platform::fs::LFS_STORAGE_HINT,
                    )
                })?;
                let length = file
                    .metadata()
                    .await
                    .map_err(|error| {
                        crate::platform::fs::path_error(
                            "LFS object file",
                            &path,
                            &error,
                            crate::platform::fs::LFS_STORAGE_HINT,
                        )
                    })?
                    .len();
                if length != expected {
                    return Ok(Err(mismatch(length)));
                }
                Ok(Ok(reqwest::Body::wrap_stream(
                    tokio_util::io::ReaderStream::new(file),
                )))
            }
            LfsObjectSource::Local {
                path,
                compressed: true,
            } => {
                // Opened here, so that a file that cannot be read is reported
                // as this server's failure rather than as a broken transfer.
                let file = tokio::fs::File::open(&path)
                    .await
                    .map_err(|error| {
                        crate::platform::fs::path_error(
                            "LFS object file",
                            &path,
                            &error,
                            crate::platform::fs::LFS_STORAGE_HINT,
                        )
                    })?
                    .into_std()
                    .await;
                Ok(Ok(decompressing_body(file, path)))
            }
            LfsObjectSource::Bytes { data, compressed } => {
                let data = if compressed {
                    tokio::task::spawn_blocking(move || zstd::decode_all(data.as_slice()))
                        .await
                        .map_err(|error| anyhow::anyhow!("LFS object decompression task: {error}"))?
                        .map_err(|error| {
                            anyhow::anyhow!("failed to decompress LFS object {oid}: {error}")
                        })?
                } else {
                    data
                };
                if data.len() as u64 != expected {
                    return Ok(Err(mismatch(data.len() as u64)));
                }
                Ok(Ok(reqwest::Body::from(data)))
            }
        }
    }
}

/// Whether `error` says the object is not in the store — as opposed to a
/// store that could not be asked.
fn is_absent(error: &anyhow::Error) -> bool {
    error.downcast_ref::<crate::error::NotFound>().is_some()
        || matches!(
            error.downcast_ref::<crate::blob_storage::BlobStorageError>(),
            Some(crate::blob_storage::BlobStorageError::NotFound(_))
        )
}

/// A body that decompresses a zstd object on a blocking thread as the request
/// reads it, a chunk at a time: an object may be gigabytes.
fn decompressing_body(file: std::fs::File, path: std::path::PathBuf) -> reqwest::Body {
    let (sender, receiver) = tokio::sync::mpsc::channel::<std::io::Result<Vec<u8>>>(8);
    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        // A body error reaches the uploader only as a broken transfer, so
        // this is the one place the file can still be named.
        let aborted = |error: &std::io::Error| {
            let message = crate::platform::fs::describe_path_error(
                "LFS object file",
                &path,
                error,
                crate::platform::fs::LFS_STORAGE_HINT,
            );
            tracing::error!(error = %message, "an LFS object could not be read for a push mirror");
            std::io::Error::other(message)
        };
        let mut decoder = match zstd::stream::Decoder::new(file) {
            Ok(decoder) => decoder,
            Err(error) => {
                // A receiver that is gone is an upload that already ended.
                drop(sender.blocking_send(Err(aborted(&error))));
                return;
            }
        };
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            match decoder.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    if sender.blocking_send(Ok(buffer[..read].to_vec())).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    drop(sender.blocking_send(Err(aborted(&error))));
                    break;
                }
            }
        }
    });
    reqwest::Body::wrap_stream(futures::stream::unfold(
        receiver,
        |mut receiver| async move { receiver.recv().await.map(|item| (item, receiver)) },
    ))
}
