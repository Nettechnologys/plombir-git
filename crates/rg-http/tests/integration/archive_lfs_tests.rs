//! card_aeeb6fc892f2: "Download ZIP / tar.gz" carries LFS files as the files
//! they stand for.
//!
//! `git archive` writes blobs as committed, and an LFS file is committed as a
//! pointer — so an archive of a repository with images or models in LFS handed
//! over three-line text stubs. These tests drive the real endpoint over a real
//! repository: objects uploaded the way `git lfs push` uploads them, pointers
//! committed by stock git, the archive read back by stock readers.

use std::io::Read as _;

use futures::StreamExt as _;
use sha2::{Digest, Sha256};

use crate::common::{register_full, spawn_test_app_with_db_and_repo_root};

const OWNER: &str = "lfs_archiver";

fn git(args: &[&str], cwd: Option<&std::path::Path>) -> String {
    let output = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway")
        .run(args, cwd)
        .expect("run git");
    output.ensure_success().expect("git command succeeds");
    output.stdout_str().trim().to_string()
}

fn pointer(payload: &[u8]) -> String {
    format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {}\n",
        hex::encode(Sha256::digest(payload)),
        payload.len()
    )
}

fn noise(len: usize, seed: u32) -> Vec<u8> {
    let mut x = seed | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (x & 0xff) as u8
        })
        .collect()
}

async fn create_repo(base: &str, token: &str, name: &str) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "is_private": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
}

/// Store an object the way `git lfs push` does: the batch, then the `PUT`.
async fn upload_object(base: &str, token: &str, repo: &str, payload: &[u8]) {
    let client = reqwest::Client::new();
    let batch = client
        .post(format!(
            "{base}/api/v1/repos/{OWNER}/{repo}/lfs/objects/batch"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "operation": "upload",
            "objects": [{"oid": hex::encode(Sha256::digest(payload)), "size": payload.len()}],
            "transfers": ["basic"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(batch.status(), 200, "LFS upload batch failed");
    let batch = batch.json::<serde_json::Value>().await.unwrap();
    let href = batch["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .expect("upload href")
        .to_string();
    let stored = client
        .put(href)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(stored.status(), 200, "LFS object upload failed");
}

/// Commit `files` as `main` of the bare repository, through a scratch clone.
fn commit_files(bare: &std::path::Path, files: &[(&str, Vec<u8>)]) -> String {
    let work = tempfile::tempdir().unwrap();
    let path = work.path();
    git(&["init", "-q", "--initial-branch=main"], Some(path));
    git(&["config", "user.name", "Archiver"], Some(path));
    git(
        &["config", "user.email", "archiver@example.com"],
        Some(path),
    );
    for (name, data) in files {
        let file = path.join(name);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, data).unwrap();
    }
    git(&["add", "."], Some(path));
    git(&["commit", "-q", "-m", "assets"], Some(path));
    let bare = bare.to_string_lossy();
    git(&["push", "-q", "-f", &bare, "main"], Some(path));
    git(
        &[
            "--git-dir",
            &bare,
            "symbolic-ref",
            "HEAD",
            "refs/heads/main",
        ],
        None,
    );
    git(&["rev-parse", "HEAD"], Some(path))
}

async fn download(base: &str, repo: &str, archive: &str) -> Vec<u8> {
    let response = reqwest::get(format!(
        "{base}/api/v1/repos/{OWNER}/{repo}/archive/{archive}"
    ))
    .await
    .unwrap();
    assert_eq!(response.status(), 200, "{archive}");
    response.bytes().await.unwrap().to_vec()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_archive_carries_lfs_objects_and_names_the_ones_it_cannot() {
    let (base, _db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (token, _) = register_full(&base, OWNER, &format!("{OWNER}@example.com")).await;
    create_repo(&base, &token, "assets").await;

    let model = noise(300_000, 7);
    let absent = noise(5_000, 11);
    upload_object(&base, &token, "assets", &model).await;
    let commit = commit_files(
        &repo_root.join(format!("{OWNER}/assets.git")),
        &[
            (
                ".gitattributes",
                b"*.bin filter=lfs diff=lfs merge=lfs -text\n".to_vec(),
            ),
            ("README.md", b"assets\n".to_vec()),
            ("models/model.bin", pointer(&model).into_bytes()),
            ("models/absent.bin", pointer(&absent).into_bytes()),
        ],
    );

    let zip = download(&base, "assets", "main.zip").await;
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(zip)).expect("a readable zip");
    assert_eq!(zip.comment(), commit.as_bytes(), "the zip names its commit");
    let mut entry = |name: &str| {
        let mut data = Vec::new();
        zip.by_name(name)
            .unwrap_or_else(|_| panic!("{name} is in the zip"))
            .read_to_end(&mut data)
            .unwrap();
        data
    };
    assert_eq!(
        entry("models/model.bin"),
        model,
        "the zip holds the object, not its pointer"
    );
    assert_eq!(entry("README.md"), b"assets\n");
    assert_eq!(entry("models/absent.bin"), pointer(&absent).into_bytes());
    let note = String::from_utf8(entry("LFS-OBJECTS-MISSING.txt")).unwrap();
    assert!(note.contains("models/absent.bin"), "{note}");
    assert!(!note.contains("models/model.bin"), "{note}");

    let tar_gz = download(&base, "assets", "main.tar.gz").await;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(tar_gz.as_slice()));
    let mut found = std::collections::HashMap::new();
    let mut commit_recorded = false;
    for entry in archive.entries().unwrap() {
        let mut entry = entry.unwrap();
        let mut data = Vec::new();
        entry.read_to_end(&mut data).unwrap();
        if entry.header().entry_type().is_pax_global_extensions() {
            commit_recorded |= String::from_utf8_lossy(&data).contains(&commit);
            continue;
        }
        found.insert(entry.path().unwrap().to_string_lossy().into_owned(), data);
    }
    assert!(
        commit_recorded,
        "git get-tar-commit-id must still find the commit"
    );
    assert_eq!(found["models/model.bin"], model);
    assert_eq!(found["models/absent.bin"], pointer(&absent).into_bytes());
    assert!(found.contains_key("LFS-OBJECTS-MISSING.txt"));
}

/// A repository that stores no LFS object keeps git's own encoder: the bytes
/// are exactly `git archive`'s.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_repository_without_lfs_objects_gets_git_archive_byte_for_byte() {
    let (base, _db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (token, _) = register_full(&base, OWNER, &format!("{OWNER}@example.com")).await;
    create_repo(&base, &token, "plain").await;
    let bare = repo_root.join(format!("{OWNER}/plain.git"));
    commit_files(
        &bare,
        &[
            ("README.md", b"plain\n".to_vec()),
            ("data/blob.bin", noise(70_000, 3)),
        ],
    );

    let served = download(&base, "plain", "main.zip").await;
    let output = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .unwrap()
        .run(&["archive", "--format=zip", "main"], Some(&bare))
        .unwrap();
    assert!(output.success());
    assert_eq!(
        served, output.stdout,
        "the archive of a repository without LFS changed"
    );
}

fn peak_resident_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status
        .lines()
        .find(|line| line.starts_with("VmHWM:"))?
        .strip_prefix("VmHWM:")?;
    let kib: u64 = line.split_whitespace().next()?.parse().ok()?;
    Some(kib * 1024)
}

/// The object is streamed into the archive, not held: the server's peak
/// resident memory does not grow by anything like the object's size.
#[cfg_attr(not(target_os = "linux"), ignore = "reads /proc/self/status")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_archive_of_a_large_lfs_object_does_not_grow_the_process_by_its_size() {
    const SIZE: usize = 24 * 1024 * 1024;

    let (base, _db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (token, _) = register_full(&base, OWNER, &format!("{OWNER}@example.com")).await;
    create_repo(&base, &token, "large").await;
    let model = noise(SIZE, 5);
    upload_object(&base, &token, "large", &model).await;
    let digest = Sha256::digest(&model);
    let committed_pointer = pointer(&model);
    drop(model);
    commit_files(
        &repo_root.join(format!("{OWNER}/large.git")),
        &[("model.bin", committed_pointer.into_bytes())],
    );

    // The upload above set the high-water mark at about the object's size;
    // left there, an archive that buffered the object would not raise it and
    // the assertion would hold for the wrong reason. `5` resets it.
    std::fs::write("/proc/self/clear_refs", "5").expect("reset the peak resident set");
    let before = peak_resident_bytes().expect("no /proc/self/status to measure against");
    let response = reqwest::get(format!(
        "{base}/api/v1/repos/{OWNER}/large/archive/main.tar.gz"
    ))
    .await
    .unwrap();
    assert_eq!(response.status(), 200);
    // Decode as it arrives, without keeping it: a client that collected the
    // body would be measuring itself.
    let (tx, rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(4);
    let reader = std::thread::spawn(move || {
        struct Chunks(std::sync::mpsc::Receiver<Vec<u8>>, std::io::Cursor<Vec<u8>>);
        impl std::io::Read for Chunks {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                loop {
                    let read = self.1.read(buf)?;
                    if read > 0 {
                        return Ok(read);
                    }
                    match self.0.recv() {
                        Ok(chunk) => self.1 = std::io::Cursor::new(chunk),
                        Err(_) => return Ok(0),
                    }
                }
            }
        }
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(Chunks(
            rx,
            std::io::Cursor::new(Vec::new()),
        )));
        let mut entry = archive
            .entries()
            .unwrap()
            .map(Result::unwrap)
            .find(|entry| entry.path().unwrap().to_string_lossy() == "model.bin")
            .expect("model.bin is in the archive");
        let mut hasher = Sha256::new();
        std::io::copy(&mut entry, &mut hasher).unwrap();
        hasher.finalize()
    });
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        if tx.send(chunk.unwrap().to_vec()).is_err() {
            break;
        }
    }
    drop(tx);
    assert_eq!(
        reader.join().unwrap(),
        digest,
        "the archive holds the object"
    );

    let after = peak_resident_bytes().expect("no /proc/self/status to measure against");
    let grew = after.saturating_sub(before);
    assert!(
        grew < (SIZE / 2) as u64,
        "archiving a {SIZE}-byte LFS object raised the peak resident set by {grew} bytes"
    );
}
