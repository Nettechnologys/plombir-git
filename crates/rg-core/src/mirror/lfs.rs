//! LFS objects for a pull mirror.
//!
//! `git remote update` moves pointer files and nothing they point at, so a
//! mirror of a repository with LFS used to serve pointers whose objects were
//! nowhere on this server (card_f4bc7fe93859). After every pass that moved the
//! clone's refs, the objects the new history points at are fetched from the
//! upstream's batch API with the mirror's own credential, through the same
//! [`crate::lfs::fetch::LfsFetcher`] an import uses.
//!
//! ## Reading only what is new
//!
//! A mirror is refreshed every few minutes for as long as it exists, so the
//! history it already read is not read again. The clone keeps, beside its
//! refs, the commits its refs pointed at after the last pass whose objects all
//! arrived ([`SYNCED_TIPS_FILE`]). A pass whose refs still point there costs
//! no scan and no request at all; one that moved them reads only the commits
//! since. The file is rewritten only when nothing is missing, so an object the
//! upstream did not give is asked for again on the next pass, and a server that
//! stopped between the fetch of the refs and the fetch of the objects starts
//! the next pass from the old tips rather than from the new ones.
//!
//! The file lives in the clone directory, not in a ref: the clone is a
//! `--mirror` and `git remote update --prune` deletes every local ref the
//! upstream does not have.

use std::path::Path;

use anyhow::{Context, Result};
use rg_git::credentials::GitCredentials;
use sea_orm::DatabaseConnection;

use crate::lfs::fetch::{LfsFetchFailure, LfsFetchOutcome, LfsFetcher, LfsSourceGuard};

/// The commits the clone's refs pointed at after the last complete LFS pass,
/// one hex id per line.
pub(crate) const SYNCED_TIPS_FILE: &str = "plombir-lfs-synced-tips";

/// What one mirror pass did about LFS objects.
#[derive(Debug, Default)]
pub(crate) struct MirrorLfsPass {
    /// Objects downloaded and published by this pass.
    pub fetched: usize,
    /// Objects the new history points at that are still not on this server.
    pub failed: Vec<LfsFetchFailure>,
}

/// Give the mirror's repository the LFS objects that the history its clone
/// gained since the last complete pass points at.
///
/// `Err` is this server's own failure — its database, its blob store, a clone
/// it cannot read. An object the upstream does not give is not an error: it is
/// in [`MirrorLfsPass::failed`] by oid and path, and the rest still arrive.
pub(crate) async fn fetch_new_objects(
    db: &DatabaseConnection,
    repo_root: &Path,
    repo_id: i64,
    clone: &Path,
    remote_url: &str,
    credentials: Option<GitCredentials>,
    guard: &dyn LfsSourceGuard,
) -> Result<MirrorLfsPass> {
    let scan_path = clone.to_path_buf();
    let scan = crate::blocking::run_blocking_git("mirror LFS pointer scan", move || {
        new_pointers(&scan_path)
    })
    .await
    .context("failed to read the LFS pointers the mirror's new history brings")?;
    let Some((tips, pointers)) = scan else {
        return Ok(MirrorLfsPass::default());
    };

    let mut outcome = LfsFetchOutcome::default();
    if !pointers.is_empty() {
        // Read now rather than when the mirror was selected: the objects are
        // published under the repository's current `<owner>/<name>`, and a
        // pass can run for minutes after the row was read.
        let (owner, name) = crate::repo::service::repository_identity(db, repo_id).await?;
        let storage = crate::blob_storage::instance_blob_storage(repo_root);
        let mut fetcher = LfsFetcher::new(
            db,
            &storage,
            repo_root,
            crate::lfs::service::LfsRepository {
                id: repo_id,
                owner: &owner,
                name: &name,
            },
            remote_url,
            credentials,
            guard,
        )?;
        for chunk in pointers.chunks(crate::lfs::fetch::BATCH_SIZE) {
            outcome.absorb(fetcher.fetch(chunk).await?);
        }
    }

    if outcome.failed.is_empty() {
        let tips_path = clone.to_path_buf();
        crate::blocking::run_blocking_git("mirror LFS tips record", move || {
            record_synced_tips(&tips_path, &tips)
        })
        .await?;
    }
    Ok(MirrorLfsPass {
        fetched: outcome.fetched,
        failed: outcome.failed,
    })
}

/// The clone's current tips and the pointers the history since the recorded
/// tips adds, or `None` when the refs have not moved since the last complete
/// pass.
fn new_pointers(
    clone: &Path,
) -> Result<Option<(Vec<String>, Vec<crate::lfs::pointer::PointerInHistory>)>> {
    let tips = crate::lfs::pointer::commit_tips(clone)?;
    let recorded = read_synced_tips(clone);
    if recorded.as_ref() == Some(&tips) {
        return Ok(None);
    }
    let pointers = match recorded {
        Some(known) => match crate::lfs::pointer::pointers_added_since(clone, &known)? {
            Some(pointers) => pointers,
            None => crate::lfs::pointer::objects_in_history(clone)?,
        },
        None => crate::lfs::pointer::objects_in_history(clone)?,
    };
    Ok(Some((tips, pointers)))
}

/// The recorded tips, or `None` when there is no usable record — a first pass,
/// a clone from before this existed, a file someone damaged. Each of those
/// reads the whole store, which costs time and never misses an object.
fn read_synced_tips(clone: &Path) -> Option<Vec<String>> {
    let path = clone.join(SYNCED_TIPS_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                error = %error,
                "the mirror's record of LFS-synced tips could not be read; reading the whole \
                 history instead"
            );
            return None;
        }
    };
    let mut tips = Vec::new();
    for line in text.lines().filter(|line| !line.is_empty()) {
        if gix::ObjectId::from_hex(line.as_bytes()).is_err() {
            tracing::warn!(
                path = %path.display(),
                "the mirror's record of LFS-synced tips is damaged; reading the whole history \
                 instead"
            );
            return None;
        }
        tips.push(line.to_string());
    }
    tips.sort();
    tips.dedup();
    Some(tips)
}

/// Replace the record by rename, so a pass that stops halfway leaves the old
/// record or the new one and never half of either.
fn record_synced_tips(clone: &Path, tips: &[String]) -> Result<()> {
    let path = clone.join(SYNCED_TIPS_FILE);
    let staging = clone.join(format!(
        "{SYNCED_TIPS_FILE}.{}",
        uuid::Uuid::new_v4().simple()
    ));
    let mut text = tips.join("\n");
    text.push('\n');
    std::fs::write(&staging, text).map_err(|error| {
        crate::platform::fs::path_error(
            "mirror LFS tips record",
            &staging,
            &error,
            crate::platform::fs::REPO_ROOT_HINT,
        )
    })?;
    std::fs::rename(&staging, &path).map_err(|error| {
        crate::platform::fs::discard_file("mirror LFS tips record", &staging);
        crate::platform::fs::path_error(
            "mirror LFS tips record",
            &path,
            &error,
            crate::platform::fs::REPO_ROOT_HINT,
        )
    })
}

/// The sentence a pass whose refs arrived but whose objects did not leaves in
/// `last_sync_error`. Names objects by oid and path — what the owner needs to
/// find the file — and stops after a few, since a row is not a report.
pub(crate) fn describe_shortfall(failed: &[LfsFetchFailure]) -> String {
    const NAMED: usize = 5;
    let mut message = format!(
        "the refs are up to date, but {} LFS object(s) could not be fetched from the upstream: ",
        failed.len()
    );
    let named = failed
        .iter()
        .take(NAMED)
        .map(|failure| {
            let path = if failure.path.is_empty() {
                "no path in any ref"
            } else {
                failure.path.as_str()
            };
            format!("{} ({path}): {}", failure.oid, failure.reason)
        })
        .collect::<Vec<_>>()
        .join("; ");
    message.push_str(&named);
    if failed.len() > NAMED {
        message.push_str(&format!("; and {} more", failed.len() - NAMED));
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::ActiveValue::{NotSet, Set};
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const OWNER: &str = "mirror-lfs-owner";
    const NAME: &str = "assets";

    /// The upstream's LFS server: answers the batch API for the objects it has
    /// and serves their bytes, recording every oid each batch asked for.
    struct Upstream {
        address: std::net::SocketAddr,
        objects: Arc<Mutex<HashMap<String, Vec<u8>>>>,
        batches: Arc<Mutex<Vec<Vec<String>>>>,
    }

    impl Upstream {
        async fn spawn() -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind upstream");
            let address = listener.local_addr().expect("upstream address");
            let objects: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::default();
            let batches: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
            let (served, recorded) = (Arc::clone(&objects), Arc::clone(&batches));
            tokio::spawn(async move {
                while let Ok((mut stream, _)) = listener.accept().await {
                    let (path, body) = read_request(&mut stream).await;
                    let response = if path.ends_with("/info/lfs/objects/batch") {
                        let request: serde_json::Value =
                            serde_json::from_slice(&body).expect("batch body");
                        let oids = request["objects"]
                            .as_array()
                            .expect("batch objects")
                            .iter()
                            .map(|object| object["oid"].as_str().expect("oid").to_string())
                            .collect::<Vec<_>>();
                        recorded.lock().expect("batches").push(oids.clone());
                        let known = served.lock().expect("objects");
                        let answer = oids
                            .iter()
                            .map(|oid| match known.get(oid) {
                                Some(bytes) => serde_json::json!({
                                    "oid": oid,
                                    "size": bytes.len(),
                                    "actions": {"download": {
                                        "href": format!("http://{address}/objects/{oid}"),
                                    }},
                                }),
                                None => serde_json::json!({
                                    "oid": oid,
                                    "error": {"code": 404, "message": "not found"},
                                }),
                            })
                            .collect::<Vec<_>>();
                        respond(
                            "application/vnd.git-lfs+json",
                            serde_json::json!({"transfer": "basic", "objects": answer})
                                .to_string()
                                .as_bytes(),
                        )
                    } else {
                        let oid = path.rsplit('/').next().unwrap_or_default();
                        let bytes = served.lock().expect("objects").get(oid).cloned();
                        respond("application/octet-stream", &bytes.expect("served object"))
                    };
                    stream.write_all(&response).await.expect("write response");
                    stream.shutdown().await.expect("close connection");
                }
            });
            Self {
                address,
                objects,
                batches,
            }
        }

        fn give(&self, payload: &[u8]) {
            self.objects
                .lock()
                .expect("objects")
                .insert(oid_of(payload), payload.to_vec());
        }

        fn remote_url(&self) -> String {
            format!("http://{}/upstream.git", self.address)
        }

        fn batches(&self) -> Vec<Vec<String>> {
            self.batches.lock().expect("batches").clone()
        }
    }

    async fn read_request(stream: &mut tokio::net::TcpStream) -> (String, Vec<u8>) {
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
        let path = head.split(' ').nth(1).unwrap_or_default().to_string();
        (path, bytes[head_end..].to_vec())
    }

    fn respond(content_type: &str, body: &[u8]) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body);
        out
    }

    /// The loopback upstream is this test's own; the production guards refuse
    /// it, and have their own tests.
    struct Loopback;

    impl LfsSourceGuard for Loopback {
        fn client_for(&self, _url: &str) -> Result<reqwest::ClientBuilder> {
            Ok(crate::net::outbound_client_builder())
        }
    }

    fn oid_of(payload: &[u8]) -> String {
        hex::encode(Sha256::digest(payload))
    }

    fn git(dir: &Path, args: &[&str]) {
        rg_git::cli_gateway::global_gateway()
            .as_ref()
            .expect("git gateway")
            .run(args, Some(dir))
            .expect("run git")
            .ensure_success()
            .unwrap_or_else(|error| panic!("git {args:?} failed: {error:#}"));
    }

    /// Commit a pointer for each `(path, payload)` to the upstream work tree.
    fn commit_pointers(upstream: &Path, files: &[(&str, &[u8])], message: &str) {
        for (path, payload) in files {
            std::fs::write(
                upstream.join(path),
                format!(
                    "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {}\n",
                    oid_of(payload),
                    payload.len()
                ),
            )
            .expect("write pointer");
        }
        git(upstream, &["add", "."]);
        git(upstream, &["commit", "-qm", message]);
    }

    async fn repository(db: &DatabaseConnection) -> i64 {
        let owner = rg_db::ops::user_ops::create_user(
            db,
            OWNER,
            "mirror-lfs-owner@example.invalid",
            "",
            "Owner",
        )
        .await
        .expect("create owner");
        let now = chrono::Utc::now();
        rg_db::ops::repo_ops::create(
            db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(owner.id),
                name: Set(NAME.to_string()),
                description: Set(None),
                is_private: Set(false),
                default_branch: Set("main".to_string()),
                fork_id: Set(None),
                stars_count: Set(0),
                forks_count: Set(0),
                org_id: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                deleted_at: Set(None),
                origin_repo_id: Set(None),
            },
        )
        .await
        .expect("create repository")
        .id
    }

    /// What `git lfs pull` from the mirror would be served for `payload`.
    async fn served(repo_root: &Path, payload: &[u8]) -> Option<Vec<u8>> {
        let storage = crate::blob_storage::instance_blob_storage(repo_root);
        let legacy = crate::lfs::service::lfs_root(repo_root, OWNER, NAME);
        match crate::lfs::service::read_object_source(
            &storage,
            &legacy,
            OWNER,
            NAME,
            &oid_of(payload),
        )
        .await
        {
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

    fn scanned_oids(clone: &Path) -> Option<Vec<String>> {
        new_pointers(clone)
            .expect("scan the clone")
            .map(|(_, pointers)| {
                pointers
                    .into_iter()
                    .map(|entry| entry.pointer.oid)
                    .collect()
            })
    }

    /// The acceptance of card_f4bc7fe93859: an LFS file added upstream is
    /// served by the mirror after the pass that brought its pointer; a pass
    /// with nothing new asks the upstream nothing; a pass reads only the
    /// history since the last complete one; and an object the upstream would
    /// not give is named, then asked for again until it arrives.
    #[tokio::test]
    async fn a_mirror_pass_brings_the_objects_its_new_history_points_at() {
        let db = crate::test_support::migrated_memory_database().await;
        let repo_id = repository(&db).await;
        let directory = tempfile::tempdir().expect("tempdir");
        let repo_root = directory.path().join("repos");
        let upstream = directory.path().join("upstream");
        std::fs::create_dir_all(&upstream).expect("create upstream");
        git(&upstream, &["init", "-q", "-b", "main"]);
        git(&upstream, &["config", "user.name", "mirror lfs test"]);
        git(
            &upstream,
            &["config", "user.email", "mirror@example.invalid"],
        );

        let first: &[u8] = b"first binary payload";
        commit_pointers(&upstream, &[("first.bin", first)], "first");
        let clone = crate::mirror::service::mirror_clone_path(&repo_root, repo_id);
        std::fs::create_dir_all(&repo_root).expect("create repo root");
        git(
            &repo_root,
            &[
                "clone",
                "--mirror",
                "-q",
                upstream.to_str().expect("utf-8 path"),
                clone.to_str().expect("utf-8 path"),
            ],
        );

        let source = Upstream::spawn().await;
        source.give(first);
        let pass = |credentials: Option<GitCredentials>| {
            let (db, repo_root, clone, url) = (&db, &repo_root, &clone, source.remote_url());
            async move {
                fetch_new_objects(db, repo_root, repo_id, clone, &url, credentials, &Loopback)
                    .await
                    .expect("a pass that only the upstream can fail")
            }
        };

        let outcome = pass(None).await;
        assert_eq!((outcome.fetched, outcome.failed.len()), (1, 0));
        assert_eq!(served(&repo_root, first).await.as_deref(), Some(first));
        assert_eq!(source.batches(), vec![vec![oid_of(first)]]);

        // Nothing moved upstream: no scan, and not one request.
        assert_eq!(scanned_oids(&clone), None);
        let outcome = pass(None).await;
        assert_eq!((outcome.fetched, outcome.failed.len()), (0, 0));
        assert_eq!(
            source.batches().len(),
            1,
            "an unchanged pass asked the upstream"
        );

        // Two new files; the upstream has only one of them.
        let second: &[u8] = b"second binary payload";
        let missing: &[u8] = b"an object the upstream never received";
        commit_pointers(
            &upstream,
            &[("second.bin", second), ("missing.bin", missing)],
            "second",
        );
        git(&clone, &["remote", "update", "--prune"]);
        source.give(second);

        let mut expected = vec![oid_of(second), oid_of(missing)];
        expected.sort();
        assert_eq!(
            scanned_oids(&clone),
            Some(expected.clone()),
            "the pass read history the last complete pass had already read"
        );
        let outcome = pass(None).await;
        assert_eq!(outcome.fetched, 1);
        assert_eq!(
            outcome
                .failed
                .iter()
                .map(|failure| (failure.oid.clone(), failure.path.clone()))
                .collect::<Vec<_>>(),
            vec![(oid_of(missing), "missing.bin".to_string())]
        );
        assert_eq!(served(&repo_root, second).await.as_deref(), Some(second));
        assert_eq!(served(&repo_root, missing).await, None);
        let message = describe_shortfall(&outcome.failed);
        assert!(
            message.contains(&oid_of(missing)) && message.contains("missing.bin"),
            "the mirror's status must name the missing object: {message}"
        );

        // The pass fell short, so the next one starts from the same tips and
        // asks again — for the missing object only.
        assert_eq!(scanned_oids(&clone), Some(expected));
        source.give(missing);
        let outcome = pass(None).await;
        assert_eq!((outcome.fetched, outcome.failed.len()), (1, 0));
        assert_eq!(served(&repo_root, missing).await.as_deref(), Some(missing));
        assert_eq!(source.batches().last(), Some(&vec![oid_of(missing)]));
        assert_eq!(scanned_oids(&clone), None);
    }

    /// A record naming a commit the clone no longer has — a force-push
    /// upstream and a `gc` since — cannot bound the walk, and a damaged record
    /// cannot either; both read the whole store rather than miss an object.
    #[test]
    fn an_unusable_record_reads_the_whole_history() {
        let directory = tempfile::tempdir().expect("tempdir");
        let clone = directory.path();
        git(clone, &["init", "-q", "-b", "main"]);
        git(clone, &["config", "user.name", "mirror lfs test"]);
        git(clone, &["config", "user.email", "mirror@example.invalid"]);
        commit_pointers(clone, &[("old.bin", b"old")], "old");
        commit_pointers(clone, &[("new.bin", b"new")], "new");
        let mut everything = vec![oid_of(b"old"), oid_of(b"new")];
        everything.sort();

        for record in ["f".repeat(40), "not a commit id".to_string()] {
            std::fs::write(clone.join(".git").join(SYNCED_TIPS_FILE), &record)
                .expect("write record");
            assert_eq!(
                scanned_oids(&clone.join(".git")),
                Some(everything.clone()),
                "record {record:?}"
            );
        }
    }
}
