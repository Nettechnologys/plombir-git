//! What a deletion left behind before anything recorded deletions, and the
//! report that hands it to a person instead of guessing at it.
//!
//! [`crate::deletion_recovery`] finishes an interrupted deletion by reading the
//! journal entry that deletion wrote. Entries only exist because that module
//! exists: every tombstone an instance accumulated *before* it — a
//! `<name>.deleted-<id>` sibling left beside a live file, a subtree parked
//! under `_deleted/` — was made by a build that recorded nothing, so the
//! startup pass does not see it and never will.
//!
//! Walking the disk and deciding is the design that was deliberately not
//! taken. Without a journal entry, "put these bytes back or destroy them" has
//! to be re-derived from the database, one query shape per family, and for the
//! staging-tree forms the live address is not in the key at all — a pass that
//! guessed would be destroying production bytes on a guess, and the two
//! mistakes it can make are not equally expensive.
//!
//! So this reports and decides nothing. It renames nothing, deletes nothing and
//! creates nothing; it answers "what is actually sitting in the storage root,
//! how much of it is there, and what does its name say it was", and the person
//! reading it has the database open and can tell whether the row is still
//! alive.
//!
//! Two things keep the report worth acting on:
//!
//! - the name matcher is strict. A deletion id in this tree is always
//!   `Uuid::new_v4().simple()`, and anything whose id does not parse as one is
//!   not listed. An operator acts on this list, so a file that merely reads
//!   like a tombstone appearing on it is a file somebody deletes by hand.
//! - anything the journal still records is left out. Those are exactly the
//!   deletions the startup pass finishes on its own, and listing them would
//!   call a person to a place where nobody is needed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};

use crate::deletion_recovery::{self, COMMITTED, DELETED, JOURNAL};

/// The suffix every move-aside deletion appends to the live file name.
const DELETED_SUFFIX: &str = ".deleted-";

/// The deletion staging families, and where in each one's key the deletion id
/// sits — counted in segments below `_deleted/`, the id itself excluded.
///
/// Position, not just spelling, because a deletion id is 32 hex characters and
/// so is a perfectly ordinary package name: an artifact named after a content
/// hash would otherwise be read as an id, and the report would name the
/// package's whole directory where one deletion belonged. On a list somebody
/// deletes files from, that is the difference between one tombstone and every
/// version of a live package.
///
/// - `package-deletions/<owner>/<repo>/<type>/<name>/<version>/<id>` —
///   `package_registry::storage`.
/// - `release-deletions/<kind>/<item_id>/<id>` — `release::service`.
/// - `repositories/<repo_id>/<id>/…` — `repo::service`, and the OCI upload tree
///   under the same key below `_oci_uploads/`.
///
/// A family added later matches nothing here, so its bytes are reported as an
/// unrecognised shape rather than guessed at — visible, which is the failure
/// this whole module is about.
const STAGING_FAMILIES: &[(&str, usize)] = &[
    ("package-deletions", 6),
    ("release-deletions", 3),
    ("repositories", 2),
];

/// Is the entry named `file_name`, reached with `trail` below `_deleted/`, the
/// deletion id of a family this build knows?
fn names_a_staged_deletion(trail: &[String], file_name: &str) -> bool {
    is_deletion_id(file_name)
        && STAGING_FAMILIES.iter().any(|(family, depth)| {
            trail.len() == *depth && trail.first().is_some_and(|first| first == family)
        })
}

/// What the staged name says about the bytes under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TombstoneName {
    /// A live file or directory renamed beside itself.
    ///
    /// The live name is this name minus the suffix — a pure inverse of the
    /// rename that made it, so `belongs_at` is derived rather than guessed.
    Sibling {
        /// Where the bytes were before the deletion moved them.
        belongs_at: PathBuf,
        /// The repository row id, for the two producers that put one in the
        /// name (a repository's own deletion, and a mirror clone's).
        repo_id: Option<String>,
    },
    /// A subtree of the deletion staging area.
    ///
    /// The key below `_deleted/` spells which family the bytes belonged to and
    /// which row within it — but not where they lived, because the move that
    /// put them there replaced the live prefix rather than extending it. That
    /// address is in the database, and finding it is the reader's half.
    Staged {
        /// The key between `_deleted/` and the deletion id, verbatim.
        describes: String,
    },
}

/// One set of bytes some deletion moved out of the live namespace and left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tombstone {
    /// Where the bytes are now.
    pub staged_at: PathBuf,
    /// The deletion that moved them, as its name spells it.
    pub deletion_id: String,
    /// What the name says these bytes were.
    pub name: TombstoneName,
    /// Total size, summed over the tree when the tombstone is a directory.
    pub bytes: u64,
    /// When the tombstone itself was last touched — in practice, when the
    /// deletion moved it aside.
    pub modified: Option<DateTime<Utc>>,
}

/// A path the walk could not read, and the reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unreadable {
    pub path: PathBuf,
    pub error: String,
}

/// Everything one pass over a storage root found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inventory {
    /// The tombstones, ordered by path so two runs read the same.
    pub tombstones: Vec<Tombstone>,
    /// Staged names whose deletion the journal still records. Counted rather
    /// than listed: the startup pass owns them, and the count is only here so
    /// the report can say why it is shorter than the directory looks.
    pub journalled: usize,
    /// Paths under `_deleted/` whose key names no deletion id. Not tombstones
    /// as far as this build can tell, and not dropped either — a shape it does
    /// not recognise is exactly what a person should be told about.
    pub unrecognised: Vec<PathBuf>,
    /// Directories the walk could not open, so the report can say it is
    /// incomplete instead of reading as an all-clear.
    pub unreadable: Vec<Unreadable>,
}

/// Is `text` a deletion id — `Uuid::new_v4().simple()`, the one spelling every
/// producer in this tree builds?
///
/// The strictness is the whole point. `.deleted-` is a suffix a person could
/// have typed, and a report an operator deletes files from must not carry a
/// row that was never a tombstone.
fn is_deletion_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The two spellings of a staged sibling name, or `None` for a name that is
/// neither.
///
/// `<name>.deleted-<id>` is what most producers build; the repository form is
/// `<name>.deleted-<repo_id>-<id>`, which carries the row id so a directory
/// listing says which repository a bare `.git` tombstone belonged to.
///
/// The split is from the right: a tombstone of a tombstone (a live file that
/// was itself once staged and put back) keeps the earlier suffix as part of its
/// live name, and the last one is the one this deletion added.
fn staged_sibling(file_name: &str) -> Option<(String, Option<String>, &str)> {
    let (live, tail) = file_name.rsplit_once(DELETED_SUFFIX)?;
    if live.is_empty() {
        return None;
    }
    if is_deletion_id(tail) {
        return Some((live.to_string(), None, tail));
    }
    let (repo_id, deletion_id) = tail.rsplit_once('-')?;
    let plausible_row = !repo_id.is_empty() && repo_id.bytes().all(|byte| byte.is_ascii_digit());
    (plausible_row && is_deletion_id(deletion_id))
        .then(|| (live.to_string(), Some(repo_id.to_string()), deletion_id))
}

/// List what interrupted deletions left under `repo_root`, without touching it.
///
/// The root has to exist: this reads a deployment, and a root that is not there
/// is a wrong path rather than an instance with nothing staged. Creating it to
/// find out — which is what a blob-store listing would do — would answer the
/// question by making the answer true.
pub async fn inventory(repo_root: &Path) -> anyhow::Result<Inventory> {
    anyhow::ensure!(
        repo_root.try_exists().unwrap_or(false),
        "the repository storage root `{}` is not there, so there is nothing to inventory — pass \
         `--repo-root` or `--config`, or run from the data directory",
        repo_root.display()
    );

    let journalled =
        deletion_recovery::journalled_deletion_ids(&deletion_recovery::journal_at(repo_root))
            .await
            .map_err(|error| {
                error.context(
                    "read the deletion journal, to leave out the deletions the startup pass \
                     finishes on its own",
                )
            })?;

    let mut found = Inventory::default();
    let mut pending = vec![repo_root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let mut entries = match tokio::fs::read_dir(&directory).await {
            Ok(entries) => entries,
            Err(error) => {
                found.unreadable.push(Unreadable {
                    path: directory,
                    error: error.to_string(),
                });
                continue;
            }
        };
        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(error) => {
                    found.unreadable.push(Unreadable {
                        path: directory.clone(),
                        error: error.to_string(),
                    });
                    break;
                }
            };
            let path = entry.path();
            let file_name = entry.file_name().to_string_lossy().into_owned();

            if file_name == DELETED {
                collect_staging_tree(&path, &journalled, &mut found).await;
                continue;
            }

            if let Some((live, repo_id, deletion_id)) = staged_sibling(&file_name) {
                if journalled.contains(deletion_id) {
                    found.journalled += 1;
                    continue;
                }
                let name = TombstoneName::Sibling {
                    belongs_at: path.with_file_name(live),
                    repo_id,
                };
                record(&mut found, path, deletion_id.to_string(), name).await;
                // A staged directory is one tombstone, not a tree of them: what
                // is inside it is the live tree the deletion moved.
                continue;
            }

            // Symlinks are not followed. Nothing in this tree creates one, and
            // a walk that followed one could leave the storage root entirely.
            if entry
                .file_type()
                .await
                .is_ok_and(|file_type| file_type.is_dir())
            {
                pending.push(path);
            }
        }
    }

    found
        .tombstones
        .sort_by(|a, b| a.staged_at.cmp(&b.staged_at));
    found.unrecognised.sort();
    Ok(found)
}

/// Walk `_deleted/`, whose children are deletion staging families rather than
/// live data.
///
/// The journal and the commit markers live here too, and they are the one thing
/// under this root that is bookkeeping rather than bytes — skipped by name, at
/// the only level they can appear at.
async fn collect_staging_tree(
    staging_root: &Path,
    journalled: &BTreeSet<String>,
    found: &mut Inventory,
) {
    let mut pending = vec![(staging_root.to_path_buf(), Vec::<String>::new())];
    while let Some((directory, trail)) = pending.pop() {
        let mut entries = match tokio::fs::read_dir(&directory).await {
            Ok(entries) => entries,
            Err(error) => {
                found.unreadable.push(Unreadable {
                    path: directory,
                    error: error.to_string(),
                });
                continue;
            }
        };
        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(error) => {
                    found.unreadable.push(Unreadable {
                        path: directory.clone(),
                        error: error.to_string(),
                    });
                    break;
                }
            };
            let path = entry.path();
            let file_name = entry.file_name().to_string_lossy().into_owned();

            if trail.is_empty() && (file_name == JOURNAL || file_name == COMMITTED) {
                continue;
            }

            if names_a_staged_deletion(&trail, &file_name) {
                if journalled.contains(&file_name) {
                    found.journalled += 1;
                    continue;
                }
                let name = TombstoneName::Staged {
                    describes: trail.join("/"),
                };
                record(found, path, file_name, name).await;
                continue;
            }

            // Above a family's id level there is more key to read; at or below
            // it there is nothing left to find, and a directory this build
            // cannot place is what the operator is told about.
            let above_an_id = STAGING_FAMILIES.iter().any(|(family, depth)| {
                trail.len() < *depth && trail.first().is_none_or(|first| first == family)
            });
            let is_directory = entry
                .file_type()
                .await
                .is_ok_and(|file_type| file_type.is_dir());
            if is_directory && above_an_id {
                let mut deeper = trail.clone();
                deeper.push(file_name);
                pending.push((path, deeper));
                continue;
            }
            found.unrecognised.push(path);
        }
    }
}

/// Measure one tombstone and add it to the report.
///
/// A tombstone that cannot be measured is still reported — its size is what is
/// unknown, not its existence, and leaving it out would make the report quieter
/// than the disk.
async fn record(
    found: &mut Inventory,
    staged_at: PathBuf,
    deletion_id: String,
    name: TombstoneName,
) {
    let (bytes, modified) = match measure(&staged_at).await {
        Ok(measured) => measured,
        Err(error) => {
            found.unreadable.push(Unreadable {
                path: staged_at.clone(),
                error: error.to_string(),
            });
            (0, None)
        }
    };
    found.tombstones.push(Tombstone {
        staged_at,
        deletion_id,
        name,
        bytes,
        modified,
    });
}

/// Total bytes under `path`, and when `path` itself was last touched.
async fn measure(path: &Path) -> std::io::Result<(u64, Option<DateTime<Utc>>)> {
    let metadata = tokio::fs::symlink_metadata(path).await?;
    let modified = metadata.modified().ok().map(DateTime::<Utc>::from);
    if !metadata.is_dir() {
        return Ok((metadata.len(), modified));
    }

    let mut total = 0;
    let mut pending = vec![path.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let mut entries = tokio::fs::read_dir(&directory).await?;
        while let Some(entry) = entries.next_entry().await? {
            let metadata = entry.metadata().await?;
            if metadata.is_dir() {
                pending.push(entry.path());
            } else {
                total += metadata.len();
            }
        }
    }
    Ok((total, modified))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob_storage::BlobStorage;

    const ID: &str = "0123456789abcdef0123456789abcdef";
    const OTHER_ID: &str = "fedcba9876543210fedcba9876543210";

    fn write(path: &Path, body: &[u8]) {
        std::fs::create_dir_all(path.parent().expect("a parent")).expect("create parent");
        std::fs::write(path, body).expect("write fixture file");
    }

    /// Every file under `root`, with its bytes — the whole tree, byte for byte,
    /// so "the pass changed nothing" is asserted against the tree rather than
    /// against a count the pass itself reports.
    fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut files = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory).expect("read fixture directory") {
                let entry = entry.expect("a fixture entry");
                let path = entry.path();
                if entry.file_type().expect("a file type").is_dir() {
                    // A directory is part of the tree even when it holds no
                    // files: an emptied tombstone directory would otherwise
                    // read as unchanged.
                    files.push((path.clone(), Vec::new()));
                    pending.push(path);
                } else {
                    let body = std::fs::read(&path).expect("read a fixture file");
                    files.push((path, body));
                }
            }
        }
        files.sort();
        files
    }

    /// The tree a build with no journal left behind: one staged sibling per
    /// spelling, and one staging-tree family.
    fn pre_journal_tree(root: &Path) {
        write(&root.join("alice/site.git/HEAD"), b"ref: refs/heads/main");
        write(
            &root.join(format!("alice/site.git.deleted-7-{ID}/HEAD")),
            b"ref: refs/heads/main",
        );
        write(
            &root.join(format!("_artifacts/12/build.tar.deleted-{ID}")),
            b"artifact bytes",
        );
        write(
            &root.join(format!(
                "_deleted/package-deletions/alice/demo/npm/foo/1.0.0/{OTHER_ID}/foo-1.0.0.tgz"
            )),
            b"package bytes",
        );
    }

    #[tokio::test]
    async fn both_spellings_of_a_pre_journal_tombstone_are_listed() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let root = directory.path();
        pre_journal_tree(root);

        let found = inventory(root).await.expect("inventory the storage root");

        let listed: Vec<&Tombstone> = found.tombstones.iter().collect();
        assert_eq!(
            listed.len(),
            3,
            "expected both sibling spellings and the staging tree, got {listed:#?}"
        );

        let repository = listed
            .iter()
            .find(|tombstone| {
                tombstone
                    .staged_at
                    .ends_with(format!("site.git.deleted-7-{ID}"))
            })
            .expect("the repository tombstone");
        assert_eq!(repository.deletion_id, ID);
        assert_eq!(
            repository.name,
            TombstoneName::Sibling {
                belongs_at: root.join("alice/site.git"),
                repo_id: Some("7".to_string()),
            },
            "the live name is the staged one minus the suffix, and the row id is in the name"
        );
        assert!(repository.bytes > 0, "a directory tombstone is measured");
        assert!(repository.modified.is_some());

        let artifact = listed
            .iter()
            .find(|tombstone| {
                tombstone
                    .staged_at
                    .ends_with(format!("build.tar.deleted-{ID}"))
            })
            .expect("the legacy artifact tombstone");
        assert_eq!(
            artifact.name,
            TombstoneName::Sibling {
                belongs_at: root.join("_artifacts/12/build.tar"),
                repo_id: None,
            }
        );
        assert_eq!(artifact.bytes, b"artifact bytes".len() as u64);

        let package = listed
            .iter()
            .find(|tombstone| tombstone.staged_at.ends_with(OTHER_ID))
            .expect("the staged package version");
        assert_eq!(package.deletion_id, OTHER_ID);
        assert_eq!(
            package.name,
            TombstoneName::Staged {
                describes: "package-deletions/alice/demo/npm/foo/1.0.0".to_string(),
            },
            "the key says which package version the bytes were, and does not claim to say where \
             they lived"
        );
        assert_eq!(package.bytes, b"package bytes".len() as u64);

        assert!(
            found.unrecognised.is_empty() && found.unreadable.is_empty(),
            "nothing in the fixture is unrecognised or unreadable: {found:#?}"
        );
    }

    #[tokio::test]
    async fn the_pass_reports_and_leaves_the_tree_byte_for_byte_as_it_found_it() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let root = directory.path();
        pre_journal_tree(root);

        let before = snapshot(root);
        let found = inventory(root).await.expect("inventory the storage root");
        assert_eq!(found.tombstones.len(), 3, "the pass did report something");
        let after = snapshot(root);

        assert_eq!(
            before, after,
            "an inventory renames nothing, deletes nothing and creates nothing — the operator \
             decides, and this pass is what they decide from"
        );
    }

    #[tokio::test]
    async fn a_name_whose_id_is_not_a_deletion_id_is_not_a_tombstone() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let root = directory.path();
        // Every one of these ends in `.deleted-<something>`, and an operator
        // pointed at this list would delete the lot.
        write(
            &root.join("notes.txt.deleted-old"),
            b"a person's own backup",
        );
        write(&root.join(".deleted-{ID}"), b"no live name in front of it");
        write(
            &root.join(format!("half.tar.deleted-{}", &ID[..31])),
            b"one hex digit short",
        );
        write(
            &root.join(format!("wrong.tar.deleted-{}z", &ID[..31])),
            b"not hex",
        );
        write(
            &root.join(format!("row.tar.deleted-seven-{ID}")),
            b"a row id that is not a number",
        );

        let found = inventory(root).await.expect("inventory the storage root");

        assert!(
            found.tombstones.is_empty(),
            "a strict matcher is what keeps somebody else's files off a list acted on by hand: \
             {:#?}",
            found.tombstones
        );
    }

    #[tokio::test]
    async fn a_deletion_the_journal_still_records_is_left_to_the_startup_pass() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let root = directory.path();
        pre_journal_tree(root);

        // Both spellings, because the exclusion is two separate reads — one on
        // the sibling names, one on the staging tree — and a test that only
        // journalled a sibling left the other free to list a deletion the
        // startup pass already owns.
        let journal = deletion_recovery::journal_at(root);
        deletion_recovery::open(
            &journal,
            ID,
            "a repository whose deletion is recorded",
            vec![deletion_recovery::StagedBytes::path(
                &root.join("alice/site.git"),
                &root.join(format!("alice/site.git.deleted-7-{ID}")),
            )
            .expect("a journal representation")],
        )
        .await
        .expect("record the repository deletion");
        deletion_recovery::open(
            &journal,
            OTHER_ID,
            "a package version whose deletion is recorded",
            Vec::new(),
        )
        .await
        .expect("record the package deletion");

        let found = inventory(root).await.expect("inventory the storage root");

        let listed: Vec<&str> = found
            .tombstones
            .iter()
            .map(|tombstone| tombstone.deletion_id.as_str())
            .collect();
        assert!(
            !listed.contains(&ID),
            "the two staged names of {ID} belong to the startup pass, not to an operator: \
             {listed:?}"
        );
        assert!(
            !listed.contains(&OTHER_ID),
            "the staged package version of {OTHER_ID} belongs to the startup pass too: \
             {listed:?}"
        );
        assert_eq!(
            found.journalled, 3,
            "all three staged names were counted, so the report can say why it is shorter than \
             the directory looks: {found:#?}"
        );
    }

    #[tokio::test]
    async fn the_journal_and_its_markers_are_not_reported_as_tombstones() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let root = directory.path();
        std::fs::create_dir_all(root.join("alice")).expect("a live namespace");

        let journal = deletion_recovery::journal_at(root);
        deletion_recovery::open(&journal, ID, "a deletion in flight", Vec::new())
            .await
            .expect("record the deletion");
        deletion_recovery::mark_committed(&journal, OTHER_ID)
            .await
            .expect("mark a deletion committed");

        let found = inventory(root).await.expect("inventory the storage root");

        assert_eq!(
            found.tombstones,
            Vec::new(),
            "the journal is bookkeeping, not staged bytes"
        );
        assert!(
            found.unrecognised.is_empty(),
            "and it is skipped by name rather than falling through as a shape this build does \
             not know: {:#?}",
            found.unrecognised
        );
    }

    #[tokio::test]
    async fn a_staging_shape_this_build_does_not_know_is_reported_rather_than_dropped() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let root = directory.path();
        write(&root.join("_deleted/some-new-family/42/bytes"), b"bytes");

        let found = inventory(root).await.expect("inventory the storage root");

        assert!(found.tombstones.is_empty());
        assert_eq!(
            found.unrecognised,
            vec![root.join("_deleted/some-new-family/42")],
            "a family added later must show up as something to look at, not vanish from the \
             report"
        );
    }

    #[tokio::test]
    async fn a_package_named_like_a_deletion_id_is_not_mistaken_for_one() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let root = directory.path();
        // A generic package named after a content hash — 32 hex characters,
        // exactly the shape of a deletion id, three levels above where one can
        // actually sit.
        let staged = root
            .join("_deleted/package-deletions/alice/demo/generic")
            .join(ID)
            .join("1.0.0")
            .join(OTHER_ID)
            .join("payload.bin");
        write(&staged, b"package bytes");

        let found = inventory(root).await.expect("inventory the storage root");

        assert_eq!(found.tombstones.len(), 1, "{found:#?}");
        assert_eq!(
            found.tombstones[0].deletion_id, OTHER_ID,
            "the id is the segment the family's key puts it at, not the first thing that reads \
             like one"
        );
        assert_eq!(
            found.tombstones[0].name,
            TombstoneName::Staged {
                describes: format!("package-deletions/alice/demo/generic/{ID}/1.0.0"),
            },
            "naming the package directory instead would put every version of a live package on \
             a list somebody deletes files from"
        );
    }

    #[tokio::test]
    async fn a_storage_root_that_is_not_there_is_refused_rather_than_created() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let missing = directory.path().join("not-the-root");

        let error = inventory(&missing)
            .await
            .expect_err("a root that is not there is a wrong path");

        assert!(
            format!("{error:#}").contains("is not there"),
            "unexpected error: {error:#}"
        );
        assert!(
            !missing.exists(),
            "asking the question must not make the answer true"
        );
    }

    #[tokio::test]
    async fn a_journalled_id_is_read_from_the_journal_the_instance_actually_writes() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let root = directory.path();
        std::fs::create_dir_all(root).expect("the storage root");

        let journal = deletion_recovery::journal_at(root);
        deletion_recovery::open(&journal, ID, "a deletion in flight", Vec::new())
            .await
            .expect("record the deletion");

        let ids = deletion_recovery::journalled_deletion_ids(&journal)
            .await
            .expect("read the journal");
        assert!(ids.contains(ID));

        // The same store the recovery pass reads, addressed the same way: a
        // second backend would leave this report describing a journal nothing
        // else uses.
        assert!(
            journal
                .exists(
                    &crate::blob_storage::BlobKey::from_segments(["_deleted", "journal", ID])
                        .expect("a journal key")
                )
                .await
                .expect("look the entry up"),
            "the journal entry is where `_deleted/journal/<id>` says it is"
        );
    }
}
