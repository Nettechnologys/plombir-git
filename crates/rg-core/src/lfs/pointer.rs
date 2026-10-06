//! Git LFS pointer files — what a commit holds in place of a large file.
//!
//! A tracked file is committed as a few lines of text naming the object by
//! hash; the bytes live in the LFS store, outside Git. Anything that moves
//! history between repositories therefore moves the pointers and nothing they
//! point at, and has to ask this module which objects the moved history needs.
//!
//! The format is git-lfs's own (`docs/spec.md` in the git-lfs repository):
//!
//! ```text
//! version https://git-lfs.github.com/spec/v1
//! oid sha256:4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393
//! size 12345
//! ```

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};

/// git-lfs reads a blob as a candidate pointer only below this size, so a
/// larger blob is file content even when it starts with a `version` line.
pub const POINTER_SIZE_LIMIT: u64 = 1024;

/// The `version` values git-lfs accepts. The second is the pre-release name of
/// the same format, still recognised by every client.
const POINTER_VERSIONS: &[&str] = &[
    "https://git-lfs.github.com/spec/v1",
    "https://hawser.github.com/spec/v1",
];

/// One parsed pointer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LfsPointer {
    /// The object's SHA-256, lowercase hex — the oid the batch API speaks.
    pub oid: String,
    pub size: u64,
}

/// Parse `data` as a pointer file, or `None` when it is ordinary content.
///
/// `None` is the answer for anything git-lfs itself would not smudge: the
/// wrong size, no `version` line first, an oid that is not a SHA-256.
pub fn parse(data: &[u8]) -> Option<LfsPointer> {
    if data.len() as u64 >= POINTER_SIZE_LIMIT {
        return None;
    }
    let text = std::str::from_utf8(data).ok()?;
    let mut lines = text.lines();

    let version = lines.next()?.strip_prefix("version ")?;
    if !POINTER_VERSIONS.contains(&version) {
        return None;
    }

    let mut oid = None;
    let mut size = None;
    for line in lines {
        let (key, value) = line.split_once(' ')?;
        match key {
            "oid" => {
                let hex = value.strip_prefix("sha256:")?;
                if !crate::lfs::service::is_valid_oid(hex) {
                    return None;
                }
                oid = Some(hex.to_string());
            }
            "size" => size = Some(value.parse::<u64>().ok()?),
            // Extension lines (`ext-0-…`) are part of the format and carry no
            // object of their own.
            _ => {}
        }
    }

    Some(LfsPointer {
        oid: oid?,
        size: size?,
    })
}

/// Every LFS object that the history reachable from `head` and not from
/// `exclude` points at, sorted and without repeats.
///
/// That history is what a merge of `head` into `exclude` brings in, whichever
/// way it is merged: a squash or a rebase writes new commits and trees, but
/// the file contents are the same blobs. Only blobs small enough to be a
/// pointer are read, and git lists them, so a pull request of large binary
/// files costs no more than one of small text files.
pub fn objects_introduced(repo_path: &Path, head: &str, exclude: &str) -> Result<Vec<String>> {
    let gateway = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let size_filter = format!("--filter=blob:limit={POINTER_SIZE_LIMIT}");
    let output = rg_git::invocation::local(gateway).run(
        &[
            "rev-list",
            "--objects",
            "--no-object-names",
            "--filter=object:type=blob",
            &size_filter,
            // The filters apply to what the walk finds, not to the tips it
            // was given; without this the `head` commit is listed as well.
            "--filter-provided-objects",
            head,
            "--not",
            exclude,
        ],
        Some(repo_path),
    )?;
    output
        .ensure_success()
        .context("failed to list the blobs a merge would bring in")?;

    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository: {repo_path:?}"))?;
    let mut oids = BTreeSet::new();
    for line in output.stdout_str().lines() {
        let id = gix::ObjectId::from_hex(line.trim().as_bytes())
            .with_context(|| format!("git rev-list printed `{line}` as an object id"))?;
        let object = repo
            .find_object(id)
            .with_context(|| format!("failed to read blob {id}"))?;
        if object.kind != gix::object::Kind::Blob {
            continue;
        }
        if let Some(pointer) = parse(&object.data) {
            oids.insert(pointer.oid);
        }
    }
    Ok(oids.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const OID: &str = "4d7a214614ab2935c943f9e0ff69d22eadbb8f32b1258daaa5e2ca24d17e2393";

    fn pointer_text(oid: &str, size: u64) -> String {
        format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {size}\n")
    }

    #[test]
    fn a_pointer_file_names_its_object() {
        assert_eq!(
            parse(pointer_text(OID, 12345).as_bytes()),
            Some(LfsPointer {
                oid: OID.to_string(),
                size: 12345,
            })
        );
    }

    #[test]
    fn the_pre_release_version_and_extension_lines_are_still_pointers() {
        let text = format!(
            "version https://hawser.github.com/spec/v1\next-0-foo sha256:{OID}\noid sha256:{OID}\nsize 7\n"
        );
        assert_eq!(parse(text.as_bytes()).map(|p| p.oid), Some(OID.to_string()));
    }

    #[test]
    fn ordinary_content_is_not_a_pointer() {
        for text in [
            String::new(),
            "hello\n".to_string(),
            // `version` has to come first.
            format!("oid sha256:{OID}\nversion https://git-lfs.github.com/spec/v1\nsize 1\n"),
            // An unknown format version.
            pointer_text(OID, 1).replace("spec/v1", "spec/v2"),
            // Not a SHA-256, or not one in the form the batch API speaks.
            pointer_text("abc", 1),
            pointer_text(&OID.to_uppercase(), 1),
            pointer_text(OID, 1).replace("sha256:", "sha1:"),
            // A pointer with a part missing.
            pointer_text(OID, 1).replace("size 1\n", ""),
            pointer_text(OID, 1).replace("size 1", "size many"),
        ] {
            assert_eq!(parse(text.as_bytes()), None, "{text:?} parsed as a pointer");
        }
    }

    #[test]
    fn a_blob_at_the_size_limit_is_content_even_if_it_starts_like_a_pointer() {
        let mut text = pointer_text(OID, 1);
        text.push_str(&" ".repeat(POINTER_SIZE_LIMIT as usize - text.len()));
        assert_eq!(text.len() as u64, POINTER_SIZE_LIMIT);
        assert_eq!(parse(text.as_bytes()), None);
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = rg_git::cli_gateway::global_gateway()
            .as_ref()
            .expect("git gateway")
            .run(args, Some(dir))
            .expect("run git");
        output.ensure_success().expect("git command succeeds");
        output.stdout_str().trim().to_string()
    }

    #[test]
    fn only_the_pointers_the_new_history_adds_are_listed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        git(path, &["init", "-q", "-b", "main"]);
        git(path, &["config", "user.name", "pointer test"]);
        git(path, &["config", "user.email", "pointer@example.invalid"]);

        let on_base = "1".repeat(64);
        std::fs::write(path.join("old.bin"), pointer_text(&on_base, 3)).unwrap();
        git(path, &["add", "."]);
        git(path, &["commit", "-qm", "base"]);

        git(path, &["checkout", "-qb", "feature"]);
        let added = "2".repeat(64);
        let added_later = "3".repeat(64);
        std::fs::write(path.join("new.bin"), pointer_text(&added, 3)).unwrap();
        // The same pointer under a second name is one object, not two.
        std::fs::write(path.join("copy.bin"), pointer_text(&added, 3)).unwrap();
        std::fs::write(path.join("notes.txt"), "not a pointer\n").unwrap();
        // Large content that starts like a pointer is still content.
        let mut padded = pointer_text(&"4".repeat(64), 3);
        padded.push_str(&"x".repeat(2 * POINTER_SIZE_LIMIT as usize));
        std::fs::write(path.join("big.bin"), padded).unwrap();
        git(path, &["add", "."]);
        git(path, &["commit", "-qm", "first"]);
        std::fs::create_dir(path.join("nested")).unwrap();
        std::fs::write(path.join("nested/later.bin"), pointer_text(&added_later, 3)).unwrap();
        git(path, &["add", "."]);
        git(path, &["commit", "-qm", "second"]);
        let head = git(path, &["rev-parse", "HEAD"]);

        let introduced = objects_introduced(path, &head, "refs/heads/main").unwrap();
        assert_eq!(introduced, vec![added, added_later]);

        // Nothing is new relative to the head itself.
        assert!(objects_introduced(path, &head, &head).unwrap().is_empty());
    }
}
