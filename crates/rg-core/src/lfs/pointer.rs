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

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
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

/// A pointer found in a repository, with one path its blob is committed
/// under — what a person needs to recognise the file when its object is
/// missing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PointerInHistory {
    pub pointer: LfsPointer,
    /// Empty when no commit reachable from a ref holds the blob.
    pub path: String,
}

/// Every LFS object a repository that arrived whole — a fresh import clone —
/// points at, one entry per oid, sorted by oid.
///
/// Read from the object store rather than from a `git rev-list --objects`
/// listing. That listing names every small blob of the whole history, and the
/// git gateway caps what it captures at 16 MiB: a large upstream would fail
/// the import after its clone had succeeded. Here only the blob headers are
/// read, only blobs small enough to be a pointer are opened, and memory grows
/// with the number of pointers, not with the size of the history. A fresh
/// clone holds exactly what its refs reach, so the store *is* the history; a
/// repository with unreachable objects would list pointers no ref names,
/// which costs a fetch, never a missed object.
///
/// Paths come from a walk of the trees of every ref's history, newest first,
/// that stops as soon as every pointer blob has one. An oid behind several
/// blobs keeps the first path in path order.
pub fn objects_in_history(repo_path: &Path) -> Result<Vec<PointerInHistory>> {
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository: {repo_path:?}"))?;

    let mut pointers: BTreeMap<gix::ObjectId, LfsPointer> = BTreeMap::new();
    let objects = repo
        .objects
        .iter()
        .with_context(|| format!("failed to list the objects of {repo_path:?}"))?;
    for id in objects {
        let id = id.with_context(|| format!("failed to list the objects of {repo_path:?}"))?;
        // The listing may repeat an object that is in two packs.
        if pointers.contains_key(&id) {
            continue;
        }
        let header = repo
            .find_header(id)
            .with_context(|| format!("failed to read the header of object {id}"))?;
        if header.kind() != gix::object::Kind::Blob || header.size() >= POINTER_SIZE_LIMIT {
            continue;
        }
        let blob = repo
            .find_object(id)
            .with_context(|| format!("failed to read blob {id}"))?;
        if let Some(pointer) = parse(&blob.data) {
            pointers.insert(id, pointer);
        }
    }
    if pointers.is_empty() {
        return Ok(Vec::new());
    }

    let mut paths = blob_paths(&repo, repo_path, &pointers)?;
    let mut found: BTreeMap<String, PointerInHistory> = BTreeMap::new();
    for (blob, pointer) in pointers {
        let path = paths.remove(&blob).unwrap_or_default();
        match found.entry(pointer.oid.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(PointerInHistory { pointer, path });
            }
            Entry::Occupied(mut entry) => {
                let kept = &mut entry.get_mut().path;
                if kept.is_empty() || (!path.is_empty() && path < *kept) {
                    *kept = path;
                }
            }
        }
    }
    Ok(found.into_values().collect())
}

/// One path for each of `wanted`, from the trees of every ref's history.
///
/// Each tree is read once however many commits share it, and the walk ends
/// when every blob has a path — for the usual repository, whose pointers are
/// all in its branch tips, after the first few commits.
fn blob_paths(
    repo: &gix::Repository,
    repo_path: &Path,
    wanted: &BTreeMap<gix::ObjectId, LfsPointer>,
) -> Result<HashMap<gix::ObjectId, String>> {
    let tips = ref_commit_tips(repo, repo_path)?;

    let mut paths = HashMap::new();
    let mut visited = HashSet::new();
    let walk = repo
        .rev_walk(tips)
        .all()
        .with_context(|| format!("failed to walk the history of {repo_path:?}"))?;
    for commit in walk {
        let commit =
            commit.with_context(|| format!("failed to walk the history of {repo_path:?}"))?;
        let tree = repo
            .find_commit(commit.id)
            .with_context(|| format!("failed to read commit {}", commit.id))?
            .tree_id()
            .with_context(|| format!("failed to read the tree of commit {}", commit.id))?
            .detach();
        collect_blob_paths(repo, tree, wanted, &mut paths, &mut visited)?;
        if paths.len() == wanted.len() {
            break;
        }
    }
    Ok(paths)
}

/// The commits the refs of a repository point at, each once, sorted.
///
/// A tag of a tree or a blob names no history and is left out.
fn ref_commit_tips(repo: &gix::Repository, repo_path: &Path) -> Result<Vec<gix::ObjectId>> {
    let mut tips = BTreeSet::new();
    let references = repo
        .references()
        .with_context(|| format!("failed to open references in {repo_path:?}"))?;
    for reference in references
        .all()
        .with_context(|| format!("failed to list references in {repo_path:?}"))?
    {
        let mut reference = reference
            .map_err(anyhow::Error::from_boxed)
            .with_context(|| format!("failed to read a reference in {repo_path:?}"))?;
        let id = reference
            .peel_to_id()
            .with_context(|| format!("failed to resolve `{}`", reference.name().as_bstr()))?
            .detach();
        let kind = repo
            .find_header(id)
            .with_context(|| format!("failed to read the header of object {id}"))?
            .kind();
        if kind == gix::object::Kind::Commit {
            tips.insert(id);
        }
    }
    Ok(tips.into_iter().collect())
}

/// The commits a repository's refs point at, as hex, sorted — what a later
/// [`pointers_added_since`] is told it has already seen.
pub fn commit_tips(repo_path: &Path) -> Result<Vec<String>> {
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository: {repo_path:?}"))?;
    Ok(ref_commit_tips(&repo, repo_path)?
        .into_iter()
        .map(|id| id.to_string())
        .collect())
}

/// Every LFS object that the commits reachable from the refs of `repo_path`,
/// and from none of `known`, add — one entry per oid, sorted by oid.
///
/// What a repository that is refreshed rather than cloned needs: the history
/// behind `known` was read on an earlier pass, so only what came after it is
/// read now, and a pass that brought ten commits costs ten tree diffs however
/// long the history behind them is. Each new commit is compared with its first
/// parent; the blobs a merge takes from another parent are listed again, which
/// costs a lookup of an object that is already there and never misses one.
///
/// `None` when `known` names a commit the store no longer has — a force-push
/// upstream and a `gc` since — and the walk cannot be bounded by it. The caller
/// then reads the whole store with [`objects_in_history`], which never misses
/// an object.
pub fn pointers_added_since(
    repo_path: &Path,
    known: &[String],
) -> Result<Option<Vec<PointerInHistory>>> {
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository: {repo_path:?}"))?;
    let mut hidden = Vec::with_capacity(known.len());
    for tip in known {
        let Ok(id) = gix::ObjectId::from_hex(tip.as_bytes()) else {
            return Ok(None);
        };
        match repo.try_find_header(id) {
            Ok(Some(header)) if header.kind() == gix::object::Kind::Commit => hidden.push(id),
            Ok(_) => return Ok(None),
            Err(error) => {
                return Err(anyhow::Error::from(error)
                    .context(format!("failed to read the header of object {id}")))
            }
        }
    }
    let tips = ref_commit_tips(&repo, repo_path)?;

    let mut found: BTreeMap<String, PointerInHistory> = BTreeMap::new();
    let mut seen_blobs = HashSet::new();
    let walk = repo
        .rev_walk(tips)
        .with_hidden(hidden)
        .all()
        .with_context(|| format!("failed to walk the new history of {repo_path:?}"))?;
    for info in walk {
        let info = info.with_context(|| format!("failed to walk the history of {repo_path:?}"))?;
        let commit = repo
            .find_commit(info.id)
            .with_context(|| format!("failed to read commit {}", info.id))?;
        let tree = commit
            .tree()
            .with_context(|| format!("failed to read the tree of commit {}", info.id))?;
        let parent_tree = match info.parent_ids.first() {
            Some(parent) => repo
                .find_commit(*parent)
                .with_context(|| format!("failed to read commit {parent}"))?
                .tree()
                .with_context(|| format!("failed to read the tree of commit {parent}"))?,
            None => repo.empty_tree(),
        };
        let mut options = gix::diff::Options::default();
        options.track_path().track_rewrites(None);
        let changes = repo
            .diff_tree_to_tree(Some(&parent_tree), Some(&tree), options)
            .with_context(|| format!("failed to diff commit {} with its parent", info.id))?;
        for change in changes {
            if matches!(
                change,
                gix::object::tree::diff::ChangeDetached::Deletion { .. }
            ) {
                continue;
            }
            let (mode, id) = change.entry_mode_and_id();
            if !mode.is_blob() || !seen_blobs.insert(id.to_owned()) {
                continue;
            }
            let header = repo
                .find_header(id)
                .with_context(|| format!("failed to read the header of object {id}"))?;
            if header.size() >= POINTER_SIZE_LIMIT {
                continue;
            }
            let blob = repo
                .find_object(id)
                .with_context(|| format!("failed to read blob {id}"))?;
            let Some(pointer) = parse(&blob.data) else {
                continue;
            };
            let path = change.location().to_string();
            match found.entry(pointer.oid.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert(PointerInHistory { pointer, path });
                }
                Entry::Occupied(mut entry) => {
                    let kept = &mut entry.get_mut().path;
                    if path < *kept {
                        *kept = path;
                    }
                }
            }
        }
    }
    Ok(Some(found.into_values().collect()))
}

/// Every LFS object that the history reachable from a repository's refs
/// points at — the objects the repository still needs.
///
/// Unlike [`objects_in_history`] this walks reachability rather than the
/// object store: after a force-push the commits that held a pointer stay in
/// the store until `git gc`, and an object only they name is exactly the one a
/// clean-up is allowed to drop. Every tree of every commit is read once, so
/// the cost grows with the size of the history; it runs when an administrator
/// asks, not on a hot path.
pub fn oids_reachable_from_refs(repo_path: &Path) -> Result<BTreeSet<String>> {
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository: {repo_path:?}"))?;
    let tips = ref_commit_tips(&repo, repo_path)?;
    let mut oids = BTreeSet::new();
    if tips.is_empty() {
        return Ok(oids);
    }
    let mut trees = HashSet::new();
    let mut blobs = HashSet::new();
    let walk = repo
        .rev_walk(tips)
        .all()
        .with_context(|| format!("failed to walk the history of {repo_path:?}"))?;
    for info in walk {
        let info = info.with_context(|| format!("failed to walk the history of {repo_path:?}"))?;
        let root = repo
            .find_commit(info.id)
            .with_context(|| format!("failed to read commit {}", info.id))?
            .tree_id()
            .with_context(|| format!("failed to read the tree of commit {}", info.id))?
            .detach();
        if !trees.insert(root) {
            continue;
        }
        let mut stack = vec![root];
        while let Some(tree_id) = stack.pop() {
            let tree = repo
                .find_tree(tree_id)
                .with_context(|| format!("failed to read tree {tree_id}"))?;
            for entry in tree.iter() {
                let entry =
                    entry.with_context(|| format!("failed to read an entry of tree {tree_id}"))?;
                let id = entry.oid().to_owned();
                if entry.mode().is_tree() {
                    if trees.insert(id) {
                        stack.push(id);
                    }
                } else if entry.mode().is_blob() && blobs.insert(id) {
                    let header = repo
                        .find_header(id)
                        .with_context(|| format!("failed to read the header of object {id}"))?;
                    if header.size() >= POINTER_SIZE_LIMIT {
                        continue;
                    }
                    let blob = repo
                        .find_object(id)
                        .with_context(|| format!("failed to read blob {id}"))?;
                    if let Some(pointer) = parse(&blob.data) {
                        oids.insert(pointer.oid);
                    }
                }
            }
        }
    }
    Ok(oids)
}

fn collect_blob_paths(
    repo: &gix::Repository,
    root: gix::ObjectId,
    wanted: &BTreeMap<gix::ObjectId, LfsPointer>,
    paths: &mut HashMap<gix::ObjectId, String>,
    visited: &mut HashSet<gix::ObjectId>,
) -> Result<()> {
    if !visited.insert(root) {
        return Ok(());
    }
    let mut stack = vec![(root, String::new())];
    while let Some((tree_id, prefix)) = stack.pop() {
        let tree = repo
            .find_tree(tree_id)
            .with_context(|| format!("failed to read tree {tree_id}"))?;
        for entry in tree.iter() {
            let entry =
                entry.with_context(|| format!("failed to read an entry of tree {tree_id}"))?;
            let name = String::from_utf8_lossy(entry.filename());
            let path = if prefix.is_empty() {
                name.into_owned()
            } else {
                format!("{prefix}/{name}")
            };
            let id = entry.oid().to_owned();
            if entry.mode().is_tree() {
                if visited.insert(id) {
                    stack.push((id, path));
                }
            } else if wanted.contains_key(&id) {
                paths.entry(id).or_insert(path);
            }
        }
    }
    Ok(())
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

    /// What an import asks the source for: every pointer the store holds —
    /// in the tip, only in older history, under two names — each once, with a
    /// path a person can recognise.
    #[test]
    fn every_pointer_in_the_store_is_listed_once_with_a_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path();
        git(path, &["init", "-q", "-b", "main"]);
        git(path, &["config", "user.name", "pointer test"]);
        git(path, &["config", "user.email", "pointer@example.invalid"]);

        // Replaced by a later commit: only older history holds this pointer.
        let replaced = "5".repeat(64);
        std::fs::create_dir(path.join("art")).unwrap();
        std::fs::write(path.join("art/hero.png"), pointer_text(&replaced, 9)).unwrap();
        std::fs::write(path.join("notes.txt"), "not a pointer\n").unwrap();
        git(path, &["add", "."]);
        git(path, &["commit", "-qm", "first"]);

        let current = "6".repeat(64);
        let shared = "7".repeat(64);
        std::fs::write(path.join("art/hero.png"), pointer_text(&current, 9)).unwrap();
        std::fs::write(path.join("z-copy.bin"), pointer_text(&shared, 4)).unwrap();
        std::fs::write(path.join("a-original.bin"), pointer_text(&shared, 4)).unwrap();
        let mut padded = pointer_text(&"8".repeat(64), 3);
        padded.push_str(&"x".repeat(2 * POINTER_SIZE_LIMIT as usize));
        std::fs::write(path.join("big.bin"), padded).unwrap();
        git(path, &["add", "."]);
        git(path, &["commit", "-qm", "second"]);

        // A side branch holds a pointer no `main` commit has.
        git(path, &["checkout", "-qb", "side"]);
        let side = "9".repeat(64);
        std::fs::write(path.join("side.bin"), pointer_text(&side, 2)).unwrap();
        git(path, &["add", "."]);
        git(path, &["commit", "-qm", "side"]);
        git(path, &["checkout", "-q", "main"]);

        // A tag naming a blob directly, and a pointer no commit holds.
        let notes = git(path, &["rev-parse", "HEAD:notes.txt"]);
        git(path, &["tag", "blob-tag", &notes]);
        let loose = "a".repeat(64);
        let loose_file = path.join("loose-pointer");
        std::fs::write(&loose_file, pointer_text(&loose, 1)).unwrap();
        git(path, &["hash-object", "-w", loose_file.to_str().unwrap()]);
        std::fs::remove_file(&loose_file).unwrap();

        let listed = objects_in_history(path)
            .unwrap()
            .into_iter()
            .map(|entry| (entry.pointer.oid, entry.path))
            .collect::<Vec<_>>();
        assert_eq!(
            listed,
            vec![
                (replaced, "art/hero.png".to_string()),
                (current, "art/hero.png".to_string()),
                (shared, "a-original.bin".to_string()),
                (side, "side.bin".to_string()),
                (loose, String::new()),
            ]
        );
    }
}
