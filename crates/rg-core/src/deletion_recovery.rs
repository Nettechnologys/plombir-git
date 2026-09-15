//! What a storage mutation started, and who finishes the ones a killed process left.
//!
//! Every cross-store deletion in this tree runs the same three steps, in the
//! same order, for the same reason: move the bytes out of the live namespace
//! with a rename, delete the metadata, and only then destroy the tombstone.
//! Deleting the bytes first is not compensable — a metadata failure after a
//! successful unlink leaves a live row advertising a download whose bytes are
//! gone for good — so the rename is what makes the whole thing reversible.
//!
//! It is reversible *by the process that made it*. `SIGKILL`, the OOM killer
//! and a container restart run no compensation, exactly as they run no
//! destructors, and what they leave behind is not a leaked scratch file:
//!
//! - killed **before** the metadata commit, the row is still live and still
//!   points at the live name, while the bytes sit beside it under a private
//!   one. Every read answers `404` or `500` for a file that is physically
//!   there, and nothing ever comes back for it.
//! - killed **after** the metadata commit, no row names the bytes at all. They
//!   are invisible to retention (which walks rows) and to the operator (who has
//!   no reason to look), and the space is gone until the volume fills up.
//!
//! [`crate::staging`] is the same arc for in-flight upload spools, and the
//! difference is what this module exists for: a spool holds a draft nobody has
//! accepted, so the sweep there can simply delete what it finds. A tombstone
//! holds *live bytes that may still belong to a live row*. Deleting one is
//! destroying production data, and putting one back on top of a name something
//! else has since taken is destroying it the other way round. So the pass here
//! cannot decide from the filesystem alone — it needs to know whether the
//! metadata delete this tombstone belongs to actually committed.
//!
//! That bit lives in a journal entry the deletion writes *before* it moves
//! anything, and a commit marker it writes *after* the metadata is gone. Both
//! are ordinary objects in the same [`BlobStorage`] the instance already has,
//! which is why this needs no migration and no per-family database query: the
//! recovery pass reads the journal, asks whether the marker is there, and knows
//! which of the two outcomes above it is looking at.
//!
//! Not every operation that moves live bytes aside is a deletion. A repository
//! transfer performs the same three steps for the same reason — move the bytes,
//! change the metadata, clean up — but its two outcomes are not the deletion's
//! two outcomes: once the row names the new owner, the bytes belong exactly
//! where the move already put them, and destroying them would destroy a live
//! repository. So a journal entry carries a [`Disposition`]: what the *marker*
//! means. The uncommitted branch is shared, because "the metadata never
//! changed, so put the bytes back where it still looks" is the same sentence
//! for both.
//!
//! The direction of the residual risk is deliberate. A journal entry with no
//! marker is *restored*, never destroyed — so an entry whose marker write was
//! itself interrupted costs space (bytes back in a live namespace no row names,
//! which is the ordinary orphan class) rather than data. Destroying anything
//! requires positive evidence that the metadata is gone.
//!
//! Repository and attachment creation are the inverse operations. They publish
//! a final path or blob before they can insert the row, so an interrupted create
//! leaves bytes to discard rather than bytes to restore. A marker alone cannot
//! make that decision safely: the process can die after the row commits but
//! before the marker write. Creation entries therefore carry the database key,
//! and the startup pass keeps the bytes whenever that live row exists. Only a
//! successful database read proving the row absent authorises removal.

use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::blob_storage::{BlobKey, BlobStorage, LocalBlobStorage};

/// How long a journal entry must have gone unfinished before the startup pass
/// acts on it.
///
/// The pass runs before this process serves anything, so a deletion of its own
/// can never be caught by it. The bound is for the case it cannot rule out: a
/// second process — a rolling restart with an overlap window, or a second
/// container pointed at the same storage root — with a deletion genuinely in
/// flight. An hour is far above any deletion that is still progressing (its
/// longest step is one metadata transaction, and the removals after it are
/// unlinks) and far below the "forever" that a tombstone gets today.
pub const INTERRUPTED_DELETION_AGE: Duration = Duration::from_secs(60 * 60);

/// The first key segment every deletion staging namespace already shares.
///
/// Shared with [`crate::deletion_inventory`], which walks the same tree for the
/// tombstones this journal never recorded: the one name for the staging area
/// has to be the one name, or the report and the pass would be reading two
/// different directories that happen to be spelled alike.
pub(crate) const DELETED: &str = "_deleted";
/// Where a deletion records what it is about to move.
pub(crate) const JOURNAL: &str = "journal";
/// Where a deletion records that its metadata delete committed.
pub(crate) const COMMITTED: &str = "committed";

/// The journal entry of one deletion.
fn journal_key(deletion_id: &str) -> anyhow::Result<BlobKey> {
    Ok(BlobKey::from_segments([
        DELETED,
        JOURNAL,
        validated_id(deletion_id)?,
    ])?)
}

/// The commit marker of one deletion.
fn committed_key(deletion_id: &str) -> anyhow::Result<BlobKey> {
    Ok(BlobKey::from_segments([
        DELETED,
        COMMITTED,
        validated_id(deletion_id)?,
    ])?)
}

/// Refuse a deletion id that is not the shape every producer builds.
///
/// Every call site spells its id `Uuid::new_v4().simple()`, and this is what
/// keeps that true: the id becomes a key segment, and a segment assembled from
/// anything user-controlled is how a private namespace stops being private.
/// `from_segments` would percent-encode a stray separator rather than let it
/// through, so this is not the only guard — it is the one that fails loudly
/// instead of silently storing a journal entry under a name the recovery pass
/// would then read back as a different id.
fn validated_id(deletion_id: &str) -> anyhow::Result<&str> {
    let plausible = !deletion_id.is_empty()
        && deletion_id.len() <= 64
        && deletion_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
    anyhow::ensure!(
        plausible,
        "deletion id {deletion_id:?} is not a plausible identifier for a deletion journal entry"
    );
    Ok(deletion_id)
}

/// One representation a deletion moved out of the live namespace.
///
/// The two variants are the two stores a deletion in this tree ever touches: a
/// [`BlobStorage`] prefix moved with [`BlobStorage::move_prefix`], and a path
/// renamed beside itself because it predates the blob store or belongs to a
/// tree no `BlobKey` names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StagedBytes {
    /// A blob-storage prefix: `live` is where the surviving metadata looks.
    BlobPrefix { live: String, staged: String },
    /// A filesystem path — a file or a whole directory.
    Path { live: String, staged: String },
    /// A final repository path claimed before its row was inserted.
    RepositoryCreation {
        path: String,
        owner_id: i64,
        org_id: Option<i64>,
        name: String,
    },
    /// A final attachment blob published before its metadata row was inserted.
    AttachmentCreation { blob_key: String },
    /// A final CI artifact blob published before its metadata row was inserted.
    ArtifactCreation { blob_key: String },
    /// A final CI cache archive published before its metadata row was upserted.
    CacheCreation { path: String, repo_id: i64 },
    /// A final package blob published before its package-file row was inserted.
    PackageFileCreation { blob_key: String },
}

impl StagedBytes {
    /// A prefix a deletion is about to move within the blob store.
    pub fn blob_prefix(live: &BlobKey, staged: &BlobKey) -> Self {
        Self::BlobPrefix {
            live: live.to_string(),
            staged: staged.to_string(),
        }
    }

    /// A path a deletion is about to rename beside itself.
    ///
    /// A path the platform cannot spell as UTF-8 is refused rather than
    /// lossily encoded: the journal is what a later process renames *back*, and
    /// a lossy round-trip would point that rename at a path that is not the one
    /// the bytes came from.
    pub fn path(live: &Path, staged: &Path) -> anyhow::Result<Self> {
        let spell = |path: &Path| -> anyhow::Result<String> {
            path.to_str().map(str::to_owned).ok_or_else(|| {
                anyhow::anyhow!(
                    "path {} cannot be recorded in a deletion journal entry because it is not \
                     valid UTF-8",
                    path.display()
                )
            })
        };
        Ok(Self::Path {
            live: spell(live)?,
            staged: spell(staged)?,
        })
    }

    fn repository_creation(
        path: &Path,
        owner_id: i64,
        org_id: Option<i64>,
        name: &str,
    ) -> anyhow::Result<Self> {
        let path = path.to_str().map(str::to_owned).ok_or_else(|| {
            anyhow::anyhow!(
                "repository path {} cannot be recorded in a recovery journal entry because it \
                 is not valid UTF-8",
                path.display()
            )
        })?;
        Ok(Self::RepositoryCreation {
            path,
            owner_id,
            org_id,
            name: name.to_string(),
        })
    }

    fn attachment_creation(blob_key: &BlobKey) -> Self {
        Self::AttachmentCreation {
            blob_key: blob_key.to_string(),
        }
    }

    fn artifact_creation(blob_key: &BlobKey) -> Self {
        Self::ArtifactCreation {
            blob_key: blob_key.to_string(),
        }
    }

    fn cache_creation(path: &Path, repo_id: i64) -> anyhow::Result<Self> {
        let path = path.to_str().map(str::to_owned).ok_or_else(|| {
            anyhow::anyhow!(
                "CI cache archive path {} cannot be recorded in a recovery journal entry because \
                 it is not valid UTF-8",
                path.display()
            )
        })?;
        Ok(Self::CacheCreation { path, repo_id })
    }

    fn package_file_creation(blob_key: &BlobKey) -> Self {
        Self::PackageFileCreation {
            blob_key: blob_key.to_string(),
        }
    }
}

/// What the commit marker of one journal entry authorizes.
///
/// The uncommitted outcome is the same for every producer — the metadata never
/// changed, so the bytes go back to the name it still looks at. This is the
/// other outcome, and it is the only thing a move and a deletion disagree
/// about.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Disposition {
    /// A deletion: no row names the staged bytes any more, so destroy them.
    ///
    /// The default because it is what every entry written before this field
    /// existed meant, and an entry from such a build must keep meaning it.
    #[default]
    Destroy,
    /// A move: the committed metadata names the staged location, so the bytes
    /// are already where they belong and the pass only forgets the entry.
    Keep,
    /// A create: keep the final path iff its database row exists.
    RepositoryCreation,
    /// An attachment upload: keep the final blob iff its database row exists.
    AttachmentCreation,
    /// A CI artifact upload: keep the final blob iff its database row exists.
    ArtifactCreation,
    /// A CI cache upload: keep the final archive iff its database row names it.
    CacheCreation,
    /// A package upload: keep the final blob iff its package-file row names it.
    PackageFileCreation,
}

/// What one deletion declared it was about to move, before it moved it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DeletionJournalEntry {
    /// The id every staged name of this deletion is built from.
    pub(crate) deletion_id: String,
    /// What was being deleted, in the words the recovery log line uses.
    pub(crate) what: String,
    /// When the deletion declared itself — the age bound reads this rather than
    /// the object's mtime, so a storage backend that rewrites timestamps cannot
    /// make an interrupted deletion look fresh forever.
    pub(crate) staged_at: DateTime<Utc>,
    /// Every representation, in the order it is moved. Recovery restores them
    /// in reverse, the way each producer's own compensation does.
    pub(crate) staged: Vec<StagedBytes>,
    /// What a commit marker on this entry authorizes. Defaulted rather than
    /// required so an entry a previous build left behind is still readable, and
    /// reads as the deletion it was.
    #[serde(default)]
    pub(crate) disposition: Disposition,
}

/// Declare what a deletion is about to move, before it moves it.
///
/// Ordering is the whole point: an entry written after the first rename would
/// leave exactly the window this module exists to close. A failure here is
/// therefore a failed deletion, not a warning — refusing to move bytes we
/// cannot record is the same rule as refusing to delete metadata whose bytes
/// we could not stage.
pub async fn open(
    storage: &dyn BlobStorage,
    deletion_id: &str,
    what: &str,
    staged: Vec<StagedBytes>,
) -> anyhow::Result<()> {
    declare(storage, deletion_id, what, staged, Disposition::Destroy).await
}

/// Declare what a *move* is about to relocate, before it relocates it.
///
/// Same ordering rule and same failure rule as [`open`]; the difference is what
/// a later commit marker authorizes. A repository transfer moves the same live
/// bytes with the same renames, but its committed outcome is "the row now names
/// the destination", so the recovery pass has to leave the bytes at the
/// destination rather than destroy them as it would a tombstone.
///
/// `live` in every [`StagedBytes`] is the *source* name — where the metadata
/// still looks while it has not moved — and `staged` is the destination. That
/// is what makes the uncommitted branch identical for both producers.
pub async fn open_move(
    storage: &dyn BlobStorage,
    move_id: &str,
    what: &str,
    staged: Vec<StagedBytes>,
) -> anyhow::Result<()> {
    declare(storage, move_id, what, staged, Disposition::Keep).await
}

/// Declare a repository path immediately before a create or fork claims it.
///
/// Unlike a deletion or move, recovery must consult the database: a missing
/// marker can mean either "the process died before the insert" or "the insert
/// committed and the process died before writing the marker". The namespace
/// tuple is the same unique identity used by the create path itself.
pub async fn open_repository_creation(
    storage: &dyn BlobStorage,
    creation_id: &str,
    what: &str,
    path: &Path,
    owner_id: i64,
    org_id: Option<i64>,
    name: &str,
) -> anyhow::Result<()> {
    declare(
        storage,
        creation_id,
        what,
        vec![StagedBytes::repository_creation(
            path, owner_id, org_id, name,
        )?],
        Disposition::RepositoryCreation,
    )
    .await
}

/// Declare a final attachment blob immediately before publishing it.
///
/// Like repository creation, recovery cannot decide from a commit marker
/// alone: the process can die after the attachment row commits but before the
/// marker write. The exact blob key is therefore recorded so startup can ask
/// the indexed metadata column before deleting anything.
pub async fn open_attachment_creation(
    storage: &dyn BlobStorage,
    creation_id: &str,
    blob_key: &BlobKey,
) -> anyhow::Result<()> {
    declare(
        storage,
        creation_id,
        "attachment publication",
        vec![StagedBytes::attachment_creation(blob_key)],
        Disposition::AttachmentCreation,
    )
    .await
}

/// Declare a final CI artifact blob immediately before publishing it.
///
/// A missing marker is not proof that the artifact insert failed: the process
/// can die after that insert commits. Recovery records the exact request-private
/// key and asks the database before it removes any bytes.
pub async fn open_artifact_creation(
    storage: &dyn BlobStorage,
    creation_id: &str,
    blob_key: &BlobKey,
) -> anyhow::Result<()> {
    declare(
        storage,
        creation_id,
        "CI artifact publication",
        vec![StagedBytes::artifact_creation(blob_key)],
        Disposition::ArtifactCreation,
    )
    .await
}

/// Declare a final CI cache archive immediately before publishing it.
///
/// Cache rows are mutable: the ownership proof is the exact `(repo_id,
/// file_path)` pair, not merely the cache key whose row a later publication may
/// already have replaced. Recovery may remove this request-private path only
/// after that exact lookup succeeds and says no.
pub async fn open_cache_creation(
    storage: &dyn BlobStorage,
    publication_id: &str,
    repo_id: i64,
    path: &Path,
) -> anyhow::Result<()> {
    declare(
        storage,
        publication_id,
        "CI cache publication",
        vec![StagedBytes::cache_creation(path, repo_id)?],
        Disposition::CacheCreation,
    )
    .await
}

/// Declare a final package blob immediately before publishing it.
///
/// Package object keys are request-private, so the exact `package_file.storage_path`
/// is the ownership boundary. Recovery may delete the blob only after the
/// database successfully proves that no row names this key.
pub async fn open_package_file_creation(
    storage: &dyn BlobStorage,
    publication_id: &str,
    blob_key: &BlobKey,
) -> anyhow::Result<()> {
    declare(
        storage,
        publication_id,
        "package file publication",
        vec![StagedBytes::package_file_creation(blob_key)],
        Disposition::PackageFileCreation,
    )
    .await
}

async fn declare(
    storage: &dyn BlobStorage,
    deletion_id: &str,
    what: &str,
    staged: Vec<StagedBytes>,
    disposition: Disposition,
) -> anyhow::Result<()> {
    let entry = DeletionJournalEntry {
        deletion_id: deletion_id.to_string(),
        what: what.to_string(),
        staged_at: Utc::now(),
        staged,
        disposition,
    };
    let body = serde_json::to_vec(&entry).map_err(|error| {
        anyhow::anyhow!("failed to serialize a deletion journal entry: {error}")
    })?;
    storage
        .put(&journal_key(deletion_id)?, &body)
        .await
        .map_err(|error| {
            anyhow::Error::new(error).context(format!(
                "failed to record the deletion journal entry of {what} — its bytes were left in \
                 the live namespace rather than moved aside unrecorded"
            ))
        })?;
    Ok(())
}

/// Record that the metadata change committed.
///
/// This is what stops a later recovery pass from putting the bytes back: with
/// the marker in hand it destroys the tombstone of a deletion, and leaves a
/// move's bytes at the destination its committed row now names. It runs after
/// the commit and before the first cleanup step, so the window it cannot cover
/// is one object write wide.
pub async fn mark_committed(storage: &dyn BlobStorage, deletion_id: &str) -> anyhow::Result<()> {
    storage
        .put(&committed_key(deletion_id)?, &[])
        .await
        .map_err(|error| {
            anyhow::Error::new(error).context(format!(
                "failed to mark deletion {deletion_id} committed — a recovery pass would put its \
                 staged bytes back into the live namespace instead of destroying them"
            ))
        })?;
    Ok(())
}

/// Forget a deletion that has finished, whichever way it finished.
///
/// Best-effort by construction: the deletion is over, and a journal entry that
/// outlives it costs one restore attempt on some later startup that finds
/// nothing left to move. The entry goes first so the leftover — if this is
/// interrupted too — is the inert half.
pub async fn close(storage: &dyn BlobStorage, deletion_id: &str) {
    for key in [journal_key(deletion_id), committed_key(deletion_id)] {
        let key = match key {
            Ok(key) => key,
            Err(error) => {
                tracing::warn!(deletion_id, %error, "failed to name a deletion journal key");
                continue;
            }
        };
        if let Err(error) = storage.delete(&key).await {
            tracing::warn!(
                deletion_id,
                %key,
                %error,
                "failed to drop a finished deletion's journal entry — a later startup pass will \
                 read it, find nothing staged, and drop it then"
            );
        }
    }
}

/// What one recovery pass did, so the caller can say it in a single log line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Deletions that never committed: their bytes went back to the live names.
    pub restored: usize,
    /// Deletions that committed: their tombstones were destroyed.
    pub destroyed: usize,
    /// Moves that committed: their bytes were left at the destination the
    /// committed metadata names, and only the journal entry was dropped.
    pub kept: usize,
    /// Repository creates that never committed: their final paths were removed.
    pub discarded_creations: usize,
    /// Attachment uploads that never committed: their final blobs were removed.
    pub discarded_publications: usize,
    /// Entries young enough to still belong to a deletion in flight.
    pub retained: usize,
    /// Entries the pass could not read, could not decide, or could not finish.
    pub failed: usize,
}

/// The journal of the instance whose storage root is `repo_root`.
///
/// A deletion path that does not already hold a [`BlobStorage`] handle — the
/// mirror clone directory is one — still has to record itself in the same
/// journal the recovery pass reads. This is the one place that says which
/// backend that is, so a second caller cannot quietly file its entries into a
/// store nothing reads.
pub fn journal_at(repo_root: &Path) -> LocalBlobStorage {
    LocalBlobStorage::new(repo_root.to_path_buf())
}

/// The deletion ids the journal still holds an entry for.
///
/// The other reader of this journal is [`crate::deletion_inventory`], which
/// lists what an interrupted deletion left on disk *without* deciding anything
/// about it — and an entry here is precisely the case where a decision is
/// already owned: the startup pass above finishes those. Leaving them out is
/// what keeps the operator's list a list of things only a person can settle.
pub async fn journalled_deletion_ids(
    storage: &dyn BlobStorage,
) -> anyhow::Result<std::collections::BTreeSet<String>> {
    let prefix = BlobKey::from_segments([DELETED, JOURNAL])?;
    let entries = storage.list(Some(&prefix)).await?;
    Ok(entries
        .into_iter()
        .filter_map(|object| object.key.as_str().rsplit('/').next().map(str::to_owned))
        .collect())
}

/// Finish the deletions a previous run did not survive, under `repo_root`.
///
/// The local backend is rooted at `repo_root`, the same way the server builds
/// it, so the caller needs to know a storage root and nothing else.
#[cfg(test)]
pub(crate) async fn recover_interrupted_deletions_at(
    repo_root: &Path,
    older_than: Duration,
) -> RecoveryReport {
    let storage = journal_at(repo_root);
    recover_interrupted_deletions(&storage, older_than, None).await
}

/// Finish every journalled storage mutation, including repository creations.
///
/// Creation recovery is intentionally unavailable before the database is
/// ready: absence of a commit marker is not proof that the row is absent.
pub async fn recover_interrupted_storage_at(
    db: &rg_db::DatabaseConnection,
    repo_root: &Path,
    older_than: Duration,
) -> RecoveryReport {
    let storage = journal_at(repo_root);
    recover_interrupted_deletions(&storage, older_than, Some(db)).await
}

/// What the pass decided about one journal entry, and whether it went through.
#[derive(Clone, Copy)]
enum Outcome {
    Restored(bool),
    Destroyed(bool),
    /// A committed move needs no filesystem work at all, so it cannot half-fail.
    Kept,
    /// A repository path whose row never committed was removed.
    DiscardedCreation(bool),
    /// An attachment blob whose row never committed was removed.
    DiscardedPublication(bool),
}

/// Finish the deletions a previous run did not survive.
///
/// Never fatal. An entry that cannot be read or cannot be acted on is counted
/// and logged with the names an operator needs, and the pass moves on: refusing
/// to serve because one tombstone is stuck is strictly worse than serving with
/// it still there.
async fn recover_interrupted_deletions(
    storage: &dyn BlobStorage,
    older_than: Duration,
    db: Option<&rg_db::DatabaseConnection>,
) -> RecoveryReport {
    let mut report = RecoveryReport::default();

    let prefix = match BlobKey::from_segments([DELETED, JOURNAL]) {
        Ok(prefix) => prefix,
        Err(error) => {
            tracing::warn!(%error, "failed to name the deletion journal prefix");
            report.failed += 1;
            return report;
        }
    };
    let entries = match storage.list(Some(&prefix)).await {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                %error,
                "failed to read the deletion journal; deletions interrupted by a previous stop \
                 were not finished"
            );
            report.failed += 1;
            return report;
        }
    };

    for object in entries {
        let Some(deletion_id) = object.key.as_str().rsplit('/').next().map(str::to_owned) else {
            continue;
        };
        let entry = match read_entry(storage, &object.key).await {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(
                    key = %object.key,
                    error = %format!("{error:#}"),
                    "failed to read a deletion journal entry; the bytes it names were left exactly \
                     where the interrupted deletion put them"
                );
                report.failed += 1;
                continue;
            }
        };

        // A future timestamp (clock skew, a restored volume) reads as "not
        // provably old" and keeps the entry, for the same reason the staging
        // sweep keeps a spool it cannot age: this pass exists to repair an
        // inconsistency, not to become one.
        let age = Utc::now().signed_duration_since(entry.staged_at);
        let old_enough = age.to_std().is_ok_and(|age| age >= older_than);
        if !old_enough {
            report.retained += 1;
            continue;
        }

        if matches!(
            entry.disposition,
            Disposition::RepositoryCreation
                | Disposition::AttachmentCreation
                | Disposition::ArtifactCreation
                | Disposition::CacheCreation
                | Disposition::PackageFileCreation
        ) {
            let Some(db) = db else {
                tracing::warn!(
                    deletion_id,
                    what = entry.what,
                    "storage publication recovery needs the database; its bytes were left in \
                     place rather than guessed about"
                );
                report.failed += 1;
                continue;
            };
            let owner_exists = match entry.disposition {
                Disposition::RepositoryCreation => repository_creation_exists(db, &entry).await,
                Disposition::AttachmentCreation => attachment_creation_exists(db, &entry).await,
                Disposition::ArtifactCreation => artifact_creation_exists(db, &entry).await,
                Disposition::CacheCreation => cache_creation_exists(db, &entry).await,
                Disposition::PackageFileCreation => package_file_creation_exists(db, &entry).await,
                Disposition::Destroy | Disposition::Keep => unreachable!("matched above"),
            };
            let outcome = match owner_exists {
                Ok(true) => Outcome::Kept,
                Ok(false) if entry.disposition == Disposition::RepositoryCreation => {
                    Outcome::DiscardedCreation(
                        discard_uncommitted_repository_creation(&entry).await,
                    )
                }
                Ok(false) => Outcome::DiscardedPublication(
                    discard_uncommitted_publication(storage, &entry).await,
                ),
                Err(error) => {
                    tracing::warn!(
                        deletion_id,
                        what = entry.what,
                        error = %format!("{error:#}"),
                        "failed to check whether an interrupted storage publication committed; \
                         its bytes were left in place"
                    );
                    report.failed += 1;
                    continue;
                }
            };
            if matches!(
                outcome,
                Outcome::DiscardedCreation(false) | Outcome::DiscardedPublication(false)
            ) {
                report.failed += 1;
                continue;
            }
            if matches!(outcome, Outcome::Kept) {
                tracing::info!(
                    deletion_id,
                    what = entry.what,
                    "left published bytes where their committed metadata names them"
                );
                report.kept += 1;
            } else {
                match outcome {
                    Outcome::DiscardedCreation(_) => report.discarded_creations += 1,
                    Outcome::DiscardedPublication(_) => report.discarded_publications += 1,
                    _ => unreachable!("creation outcome handled above"),
                }
            }
            close(storage, &deletion_id).await;
            continue;
        }

        let committed = match committed_key(&deletion_id) {
            Ok(key) => match storage.exists(&key).await {
                Ok(committed) => committed,
                Err(error) => {
                    tracing::warn!(
                        deletion_id,
                        what = entry.what,
                        %error,
                        "failed to read the commit marker of an interrupted deletion; its bytes \
                         were left staged rather than guessed about"
                    );
                    report.failed += 1;
                    continue;
                }
            },
            Err(error) => {
                tracing::warn!(deletion_id, %error, "failed to name a deletion commit marker");
                report.failed += 1;
                continue;
            }
        };

        // The three outcomes, and the only place the disposition is read: an
        // uncommitted entry always goes back (whatever wrote it), a committed
        // deletion is finished, and a committed move is already finished —
        // destroying its bytes here would destroy what the new row points at.
        let outcome = match (committed, entry.disposition) {
            (false, Disposition::Destroy | Disposition::Keep) => {
                Outcome::Restored(restore(storage, &entry).await)
            }
            (true, Disposition::Destroy) => Outcome::Destroyed(destroy(storage, &entry).await),
            (true, Disposition::Keep) => Outcome::Kept,
            (
                _,
                Disposition::RepositoryCreation
                | Disposition::AttachmentCreation
                | Disposition::ArtifactCreation
                | Disposition::CacheCreation
                | Disposition::PackageFileCreation,
            ) => {
                unreachable!("handled above")
            }
        };
        match outcome {
            Outcome::Restored(false)
            | Outcome::Destroyed(false)
            | Outcome::DiscardedCreation(false)
            | Outcome::DiscardedPublication(false) => {
                report.failed += 1;
                continue;
            }
            Outcome::Kept => tracing::info!(
                deletion_id,
                what = entry.what,
                "left the bytes of an interrupted move where its committed metadata already \
                 names them"
            ),
            Outcome::Restored(true)
            | Outcome::Destroyed(true)
            | Outcome::DiscardedCreation(true)
            | Outcome::DiscardedPublication(true) => {}
        }
        close(storage, &deletion_id).await;
        match outcome {
            Outcome::Restored(_) => report.restored += 1,
            Outcome::Destroyed(_) => report.destroyed += 1,
            Outcome::Kept => report.kept += 1,
            Outcome::DiscardedCreation(_) => report.discarded_creations += 1,
            Outcome::DiscardedPublication(_) => report.discarded_publications += 1,
        }
    }

    report.failed += discard_orphan_markers(storage, older_than).await;
    report
}

async fn repository_creation_exists(
    db: &rg_db::DatabaseConnection,
    entry: &DeletionJournalEntry,
) -> anyhow::Result<bool> {
    let [StagedBytes::RepositoryCreation {
        owner_id,
        org_id,
        name,
        ..
    }] = entry.staged.as_slice()
    else {
        anyhow::bail!("repository creation journal entry has an invalid payload");
    };
    let repository = match org_id {
        Some(org_id) => rg_db::ops::repo_ops::find_by_org_and_name(db, *org_id, name).await?,
        None => rg_db::ops::repo_ops::find_personal_by_owner_and_name(db, *owner_id, name).await?,
    };
    Ok(repository.is_some())
}

async fn attachment_creation_exists(
    db: &rg_db::DatabaseConnection,
    entry: &DeletionJournalEntry,
) -> anyhow::Result<bool> {
    let [StagedBytes::AttachmentCreation { blob_key }] = entry.staged.as_slice() else {
        anyhow::bail!("attachment creation journal entry has an invalid payload");
    };
    rg_db::ops::attachment_ops::exists_by_blob_key(db, blob_key).await
}

async fn artifact_creation_exists(
    db: &rg_db::DatabaseConnection,
    entry: &DeletionJournalEntry,
) -> anyhow::Result<bool> {
    let [StagedBytes::ArtifactCreation { blob_key }] = entry.staged.as_slice() else {
        anyhow::bail!("CI artifact creation journal entry has an invalid payload");
    };
    rg_db::ops::artifact_ops::exists_by_file_path(db, blob_key).await
}

async fn cache_creation_exists(
    db: &rg_db::DatabaseConnection,
    entry: &DeletionJournalEntry,
) -> anyhow::Result<bool> {
    let [StagedBytes::CacheCreation { path, repo_id }] = entry.staged.as_slice() else {
        anyhow::bail!("CI cache creation journal entry has an invalid payload");
    };
    rg_db::ops::ci_retention_ops::exists_by_file_path(db, *repo_id, path).await
}

async fn package_file_creation_exists(
    db: &rg_db::DatabaseConnection,
    entry: &DeletionJournalEntry,
) -> anyhow::Result<bool> {
    let [StagedBytes::PackageFileCreation { blob_key }] = entry.staged.as_slice() else {
        anyhow::bail!("package file creation journal entry has an invalid payload");
    };
    rg_db::ops::package_file_ops::exists_by_storage_path(db, blob_key).await
}

async fn discard_uncommitted_repository_creation(entry: &DeletionJournalEntry) -> bool {
    let [StagedBytes::RepositoryCreation { path, .. }] = entry.staged.as_slice() else {
        return false;
    };
    let path = Path::new(path);
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(error) => {
            tracing::warn!(
                deletion_id = entry.deletion_id,
                what = entry.what,
                path = %path.display(),
                %error,
                "failed to inspect the path of an interrupted repository creation"
            );
            return false;
        }
    };
    if !metadata.is_dir() {
        tracing::warn!(
            deletion_id = entry.deletion_id,
            what = entry.what,
            path = %path.display(),
            "refused to discard an interrupted repository creation path that is not a directory"
        );
        return false;
    }
    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => {
            tracing::info!(
                deletion_id = entry.deletion_id,
                what = entry.what,
                path = %path.display(),
                "discarded a repository path whose interrupted creation never committed"
            );
            true
        }
        Err(error) => {
            tracing::warn!(
                deletion_id = entry.deletion_id,
                what = entry.what,
                path = %path.display(),
                %error,
                "failed to discard a repository path whose interrupted creation never committed"
            );
            false
        }
    }
}

async fn discard_uncommitted_publication(
    storage: &dyn BlobStorage,
    entry: &DeletionJournalEntry,
) -> bool {
    if let [StagedBytes::CacheCreation { path, .. }] = entry.staged.as_slice() {
        return match tokio::fs::remove_file(path).await {
            Ok(()) => {
                tracing::info!(
                    deletion_id = entry.deletion_id,
                    what = entry.what,
                    path,
                    "discarded a cache archive whose interrupted publication never committed"
                );
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(error) => {
                tracing::warn!(
                    deletion_id = entry.deletion_id,
                    what = entry.what,
                    path,
                    %error,
                    "failed to discard a cache archive whose interrupted publication never \
                     committed"
                );
                false
            }
        };
    }

    let blob_key = match entry.staged.as_slice() {
        [StagedBytes::AttachmentCreation { blob_key }]
        | [StagedBytes::ArtifactCreation { blob_key }]
        | [StagedBytes::PackageFileCreation { blob_key }] => blob_key,
        _ => return false,
    };
    let key = match BlobKey::new(blob_key) {
        Ok(key) => key,
        Err(error) => {
            tracing::warn!(
                deletion_id = entry.deletion_id,
                what = entry.what,
                blob_key,
                %error,
                "a blob publication journal entry names a key this build cannot parse"
            );
            return false;
        }
    };
    match storage.delete(&key).await {
        Ok(_) => {
            tracing::info!(
                deletion_id = entry.deletion_id,
                what = entry.what,
                blob_key,
                "discarded a blob whose interrupted publication never committed"
            );
            true
        }
        Err(error) => {
            tracing::warn!(
                deletion_id = entry.deletion_id,
                what = entry.what,
                blob_key,
                %error,
                "failed to discard a blob whose interrupted publication never \
                 committed"
            );
            false
        }
    }
}

async fn read_entry(
    storage: &dyn BlobStorage,
    key: &BlobKey,
) -> anyhow::Result<DeletionJournalEntry> {
    let body = storage.get(key).await?;
    Ok(serde_json::from_slice(&body)?)
}

/// Put every representation back where the surviving metadata expects it.
///
/// Answers whether the entry is fully accounted for. A representation that was
/// never moved (the deletion died before reaching it, or there was nothing
/// there) is accounted for and is not a restore.
///
/// Reverse order, mirroring every producer's own compensation, so a deletion
/// that staged a prefix and then a legacy file undoes the file first.
async fn restore(storage: &dyn BlobStorage, entry: &DeletionJournalEntry) -> bool {
    let mut finished = true;
    for representation in entry.staged.iter().rev() {
        match representation {
            StagedBytes::BlobPrefix { live, staged } => {
                let (Ok(live_key), Ok(staged_key)) = (BlobKey::new(live), BlobKey::new(staged))
                else {
                    tracing::warn!(
                        deletion_id = entry.deletion_id,
                        what = entry.what,
                        live,
                        staged,
                        "a deletion journal entry names a blob prefix this build cannot parse; \
                         its bytes were left staged"
                    );
                    finished = false;
                    continue;
                };
                // `move_prefix` refuses a destination that already exists, so a
                // live name something else has taken in the meantime is an
                // error here rather than a silent overwrite. That refusal is
                // the point: the operator gets both names and decides.
                match storage.move_prefix(&staged_key, &live_key).await {
                    Ok(true) => tracing::info!(
                        deletion_id = entry.deletion_id,
                        what = entry.what,
                        staged_prefix = staged,
                        live_prefix = live,
                        "restored the blob prefix of a deletion a previous run did not survive"
                    ),
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!(
                            deletion_id = entry.deletion_id,
                            what = entry.what,
                            staged_prefix = staged,
                            live_prefix = live,
                            %error,
                            "failed to restore the blob prefix of an interrupted deletion — live \
                             metadata points at missing bytes until the prefix is moved back by hand"
                        );
                        finished = false;
                    }
                }
            }
            StagedBytes::Path { live, staged } => {
                if !restore_path(entry, Path::new(live), Path::new(staged)).await {
                    finished = false;
                }
            }
            StagedBytes::RepositoryCreation { .. } => {
                tracing::warn!(
                    deletion_id = entry.deletion_id,
                    what = entry.what,
                    "repository creation payload appeared in a restore disposition"
                );
                finished = false;
            }
            StagedBytes::AttachmentCreation { .. }
            | StagedBytes::ArtifactCreation { .. }
            | StagedBytes::CacheCreation { .. }
            | StagedBytes::PackageFileCreation { .. } => {
                tracing::warn!(
                    deletion_id = entry.deletion_id,
                    what = entry.what,
                    "blob creation payload appeared in a restore disposition"
                );
                finished = false;
            }
        }
    }
    finished
}

async fn restore_path(entry: &DeletionJournalEntry, live: &Path, staged: &Path) -> bool {
    match tokio::fs::symlink_metadata(staged).await {
        // Never moved, or already put back.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(error) => {
            tracing::warn!(
                deletion_id = entry.deletion_id,
                what = entry.what,
                staged_at = %staged.display(),
                %error,
                "failed to stat the staged bytes of an interrupted deletion"
            );
            return false;
        }
        Ok(_) => {}
    }

    // `rename` replaces a destination file without a word, so the check is what
    // stands between this pass and whatever took the live name since. A rename
    // is not atomic against a concurrent creator either, but this pass runs
    // before the process serves anything, and the age bound is what covers a
    // second process.
    match tokio::fs::symlink_metadata(live).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(
                deletion_id = entry.deletion_id,
                what = entry.what,
                belongs_at = %live.display(),
                %error,
                "failed to stat the live name of an interrupted deletion"
            );
            return false;
        }
        Ok(_) => {
            tracing::warn!(
                deletion_id = entry.deletion_id,
                what = entry.what,
                staged_at = %staged.display(),
                belongs_at = %live.display(),
                "refused to restore the bytes of an interrupted deletion: something else already \
                 holds the live name, so the two have to be reconciled by hand"
            );
            return false;
        }
    }

    // The producer creates the destination's parent before its own rename, and
    // this is the same step in reverse. It matters for a move rather than a
    // deletion: a transfer's source directory can be the last thing a namespace
    // held, and `rename` into a parent that is no longer there fails with a
    // `NotFound` that reads as if the bytes were the missing half.
    if let Some(parent) = live.parent() {
        if let Err(error) = tokio::fs::create_dir_all(parent).await {
            tracing::warn!(
                deletion_id = entry.deletion_id,
                what = entry.what,
                staged_at = %staged.display(),
                belongs_at = %live.display(),
                %error,
                "failed to recreate the directory an interrupted operation's bytes belong in"
            );
            return false;
        }
    }

    if let Err(error) = tokio::fs::rename(staged, live).await {
        tracing::warn!(
            deletion_id = entry.deletion_id,
            what = entry.what,
            staged_at = %staged.display(),
            belongs_at = %live.display(),
            %error,
            "failed to restore the bytes of an interrupted deletion — live metadata points at \
             missing bytes until they are moved back by hand"
        );
        return false;
    }
    tracing::info!(
        deletion_id = entry.deletion_id,
        what = entry.what,
        staged_at = %staged.display(),
        belongs_at = %live.display(),
        "restored the bytes of a deletion a previous run did not survive"
    );
    true
}

/// Destroy the tombstone of a deletion whose metadata is already gone.
///
/// Only ever reached with a commit marker in hand — the marker is what says no
/// row names these bytes any more.
async fn destroy(storage: &dyn BlobStorage, entry: &DeletionJournalEntry) -> bool {
    let mut finished = true;
    for representation in &entry.staged {
        match representation {
            StagedBytes::BlobPrefix { staged, .. } => {
                let Ok(staged_key) = BlobKey::new(staged) else {
                    tracing::warn!(
                        deletion_id = entry.deletion_id,
                        what = entry.what,
                        staged,
                        "a deletion journal entry names a staged blob prefix this build cannot \
                         parse; it was left in place"
                    );
                    finished = false;
                    continue;
                };
                match storage.delete_prefix(&staged_key).await {
                    Ok(true) => tracing::info!(
                        deletion_id = entry.deletion_id,
                        what = entry.what,
                        staged_prefix = staged,
                        "destroyed the tombstone of a committed deletion a previous run did not \
                         finish"
                    ),
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!(
                            deletion_id = entry.deletion_id,
                            what = entry.what,
                            staged_prefix = staged,
                            %error,
                            "failed to destroy the tombstone of a committed deletion; it still \
                             occupies space no metadata accounts for"
                        );
                        finished = false;
                    }
                }
            }
            StagedBytes::Path { staged, .. } => {
                if !destroy_path(entry, Path::new(staged)).await {
                    finished = false;
                }
            }
            StagedBytes::RepositoryCreation { .. } => {
                tracing::warn!(
                    deletion_id = entry.deletion_id,
                    what = entry.what,
                    "repository creation payload appeared in a deletion disposition"
                );
                finished = false;
            }
            StagedBytes::AttachmentCreation { .. }
            | StagedBytes::ArtifactCreation { .. }
            | StagedBytes::CacheCreation { .. }
            | StagedBytes::PackageFileCreation { .. } => {
                tracing::warn!(
                    deletion_id = entry.deletion_id,
                    what = entry.what,
                    "blob creation payload appeared in a deletion disposition"
                );
                finished = false;
            }
        }
    }
    finished
}

async fn destroy_path(entry: &DeletionJournalEntry, staged: &Path) -> bool {
    let metadata = match tokio::fs::symlink_metadata(staged).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
        Err(error) => {
            tracing::warn!(
                deletion_id = entry.deletion_id,
                what = entry.what,
                staged_at = %staged.display(),
                %error,
                "failed to stat the tombstone of a committed deletion"
            );
            return false;
        }
    };

    // A symlink is neither followed nor removed, the same refusal the staging
    // sweep makes: nothing here creates one, so its presence means something
    // this pass does not understand put it there.
    let removed = if metadata.is_dir() {
        tokio::fs::remove_dir_all(staged).await
    } else if metadata.is_file() {
        tokio::fs::remove_file(staged).await
    } else {
        tracing::warn!(
            deletion_id = entry.deletion_id,
            what = entry.what,
            staged_at = %staged.display(),
            "refused to destroy a deletion tombstone that is neither a file nor a directory"
        );
        return false;
    };
    match removed {
        Ok(()) => {
            tracing::info!(
                deletion_id = entry.deletion_id,
                what = entry.what,
                staged_at = %staged.display(),
                "destroyed the tombstone of a committed deletion a previous run did not finish"
            );
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => {
            tracing::warn!(
                deletion_id = entry.deletion_id,
                what = entry.what,
                staged_at = %staged.display(),
                %error,
                "failed to destroy the tombstone of a committed deletion; it still occupies space \
                 no metadata accounts for"
            );
            false
        }
    }
}

/// Drop commit markers whose journal entry is already gone.
///
/// A marker outlives its entry when [`close`] is interrupted between the two
/// deletes. It is inert — nothing reads a marker without an entry — but it is
/// also a key that would accumulate one per interrupted cleanup forever.
async fn discard_orphan_markers(storage: &dyn BlobStorage, older_than: Duration) -> usize {
    let Ok(prefix) = BlobKey::from_segments([DELETED, COMMITTED]) else {
        return 0;
    };
    let markers = match storage.list(Some(&prefix)).await {
        Ok(markers) => markers,
        Err(error) => {
            tracing::warn!(%error, "failed to read the deletion commit markers");
            return 1;
        }
    };

    let mut failed = 0;
    for marker in markers {
        // Young markers belong to a deletion that may still be running its
        // cleanup in another process; the age bound is the same one the entries
        // above are held to.
        let old_enough = marker
            .modified
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= older_than);
        if !old_enough {
            continue;
        }
        let Some(deletion_id) = marker.key.as_str().rsplit('/').next() else {
            continue;
        };
        let Ok(entry_key) = journal_key(deletion_id) else {
            continue;
        };
        match storage.exists(&entry_key).await {
            Ok(true) => continue,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(deletion_id, %error, "failed to look up a deletion journal entry");
                failed += 1;
                continue;
            }
        }
        if let Err(error) = storage.delete(&marker.key).await {
            tracing::warn!(
                deletion_id,
                %error,
                "failed to drop the commit marker of a deletion whose journal entry is already gone"
            );
            failed += 1;
        }
    }
    failed
}

/// Where the local backend keeps one key, for a caller that has to reach it as
/// a path rather than through the trait.
///
/// Test-only, and used by the fixtures that fake an interrupted deletion.
#[cfg(test)]
fn local_key_path(root: &Path, key: &BlobKey) -> std::path::PathBuf {
    let mut path = root.to_path_buf();
    for segment in key.as_str().split('/') {
        path.push(segment);
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob_storage::BlobMetadata;
    use futures::future::BoxFuture;
    use sea_orm::ActiveValue::Set;
    use std::io::Write;

    fn storage(root: &Path) -> LocalBlobStorage {
        LocalBlobStorage::new(root.to_path_buf())
    }

    /// A portable-backend stand-in: it exposes only the object API and
    /// deliberately leaves `local_path` at the trait's `None` default.
    struct OpaqueStorage(LocalBlobStorage);

    impl OpaqueStorage {
        fn new(root: &Path) -> Self {
            Self(storage(root))
        }
    }

    impl BlobStorage for OpaqueStorage {
        fn backend_name(&self) -> &'static str {
            "opaque-test"
        }

        fn put<'a>(
            &'a self,
            key: &'a BlobKey,
            data: &'a [u8],
        ) -> BoxFuture<'a, crate::blob_storage::Result<BlobMetadata>> {
            self.0.put(key, data)
        }

        fn put_file<'a>(
            &'a self,
            key: &'a BlobKey,
            source: &'a Path,
        ) -> BoxFuture<'a, crate::blob_storage::Result<BlobMetadata>> {
            self.0.put_file(key, source)
        }

        fn get<'a>(
            &'a self,
            key: &'a BlobKey,
        ) -> BoxFuture<'a, crate::blob_storage::Result<Vec<u8>>> {
            self.0.get(key)
        }

        fn metadata<'a>(
            &'a self,
            key: &'a BlobKey,
        ) -> BoxFuture<'a, crate::blob_storage::Result<BlobMetadata>> {
            self.0.metadata(key)
        }

        fn exists<'a>(
            &'a self,
            key: &'a BlobKey,
        ) -> BoxFuture<'a, crate::blob_storage::Result<bool>> {
            self.0.exists(key)
        }

        fn delete<'a>(
            &'a self,
            key: &'a BlobKey,
        ) -> BoxFuture<'a, crate::blob_storage::Result<bool>> {
            self.0.delete(key)
        }

        fn list<'a>(
            &'a self,
            prefix: Option<&'a BlobKey>,
        ) -> BoxFuture<'a, crate::blob_storage::Result<Vec<BlobMetadata>>> {
            self.0.list(prefix)
        }
    }

    fn write_file(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = std::fs::File::create(path).unwrap();
        file.write_all(contents.as_bytes()).unwrap();
    }

    /// Age a journal entry past the bound without waiting for it.
    ///
    /// The timestamp the pass reads is the one in the body, so an "interrupted
    /// an hour ago" deletion is faked by writing the body, not by touching
    /// mtimes — which is also what makes the fixture independent of the
    /// filesystem's timestamp granularity.
    async fn open_aged(
        storage: &dyn BlobStorage,
        deletion_id: &str,
        what: &str,
        staged: Vec<StagedBytes>,
        age: Duration,
    ) {
        open_aged_with(
            storage,
            deletion_id,
            what,
            staged,
            age,
            Disposition::Destroy,
        )
        .await
    }

    async fn open_aged_with(
        storage: &dyn BlobStorage,
        deletion_id: &str,
        what: &str,
        staged: Vec<StagedBytes>,
        age: Duration,
        disposition: Disposition,
    ) {
        let entry = DeletionJournalEntry {
            deletion_id: deletion_id.to_string(),
            what: what.to_string(),
            staged_at: Utc::now() - chrono::Duration::from_std(age).unwrap(),
            staged,
            disposition,
        };
        storage
            .put(
                &journal_key(deletion_id).unwrap(),
                &serde_json::to_vec(&entry).unwrap(),
            )
            .await
            .unwrap();
    }

    const AN_HOUR_AND_A_HALF: Duration = Duration::from_secs(90 * 60);

    #[tokio::test]
    async fn a_deletion_killed_before_its_commit_gets_its_bytes_back() {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path());

        // The state a `SIGKILL` between the rename and the metadata delete
        // leaves: the row is still live and still points at `live`, and the
        // bytes are beside it under a name only the journal records.
        let live = root.path().join("_ci_cache/7/archive.tar.zst");
        let staged = root.path().join("_ci_cache/7/archive.tar.zst.deleted-abc");
        write_file(&staged, "cache bytes");
        open_aged(
            &storage,
            "0123456789abcdef0123456789abcdef",
            "CI cache archive",
            vec![StagedBytes::path(&live, &staged).unwrap()],
            AN_HOUR_AND_A_HALF,
        )
        .await;

        let report = recover_interrupted_deletions(&storage, INTERRUPTED_DELETION_AGE, None).await;

        assert_eq!(
            report,
            RecoveryReport {
                restored: 1,
                ..RecoveryReport::default()
            },
            "an uncommitted deletion must be restored, not destroyed"
        );
        assert_eq!(
            std::fs::read_to_string(&live).unwrap(),
            "cache bytes",
            "the live name the surviving row points at must hold the bytes again"
        );
        assert!(!staged.exists(), "the tombstone must not be left behind");
        assert!(
            !local_key_path(
                root.path(),
                &journal_key("0123456789abcdef0123456789abcdef").unwrap()
            )
            .exists(),
            "a finished deletion must not leave its journal entry"
        );
    }

    #[tokio::test]
    async fn a_deletion_killed_after_its_commit_loses_its_bytes() {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path());

        // The other side of the same kill: the metadata is already gone, so no
        // row names these bytes and putting them back would be a leak.
        let live = root.path().join("_artifacts/jobs/9/build.log");
        let staged = root.path().join("_artifacts/jobs/9/build.log.deleted-abc");
        write_file(&staged, "artifact bytes");
        open_aged(
            &storage,
            "fedcba9876543210fedcba9876543210",
            "legacy CI artifact",
            vec![StagedBytes::path(&live, &staged).unwrap()],
            AN_HOUR_AND_A_HALF,
        )
        .await;
        mark_committed(&storage, "fedcba9876543210fedcba9876543210")
            .await
            .unwrap();

        let report = recover_interrupted_deletions(&storage, INTERRUPTED_DELETION_AGE, None).await;

        assert_eq!(
            report,
            RecoveryReport {
                destroyed: 1,
                ..RecoveryReport::default()
            },
            "a committed deletion must be finished, not undone"
        );
        assert!(!staged.exists(), "the tombstone must be destroyed");
        assert!(
            !live.exists(),
            "bytes whose metadata is gone must not reappear under a live name"
        );
        assert!(
            !local_key_path(
                root.path(),
                &committed_key("fedcba9876543210fedcba9876543210").unwrap()
            )
            .exists(),
            "a finished deletion must not leave its commit marker"
        );
    }

    #[tokio::test]
    async fn a_blob_prefix_travels_both_ways() {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path());

        let live = BlobKey::new("packages/alice/demo/generic/sample/1.0.0").unwrap();
        let staged =
            BlobKey::new("_deleted/package-deletions/alice/demo/generic/sample/1.0.0/aaa").unwrap();
        storage
            .put(
                &BlobKey::new(format!("{staged}/sample.tgz")).unwrap(),
                b"package bytes",
            )
            .await
            .unwrap();
        open_aged(
            &storage,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "package version",
            vec![StagedBytes::blob_prefix(&live, &staged)],
            AN_HOUR_AND_A_HALF,
        )
        .await;

        let report = recover_interrupted_deletions(&storage, INTERRUPTED_DELETION_AGE, None).await;

        assert_eq!(report.restored, 1, "{report:?}");
        assert_eq!(
            storage
                .get(&BlobKey::new("packages/alice/demo/generic/sample/1.0.0/sample.tgz").unwrap())
                .await
                .unwrap(),
            b"package bytes",
            "the surviving version must reach its objects under the live prefix again"
        );
    }

    #[tokio::test]
    async fn a_deletion_still_in_flight_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path());

        let live = root.path().join("_ci_cache/7/archive.tar.zst");
        let staged = root.path().join("_ci_cache/7/archive.tar.zst.deleted-abc");
        write_file(&staged, "cache bytes");
        // Written now, so it belongs to a deletion that may still be running in
        // another process.
        open(
            &storage,
            "11111111111111111111111111111111",
            "CI cache archive",
            vec![StagedBytes::path(&live, &staged).unwrap()],
        )
        .await
        .unwrap();

        let report = recover_interrupted_deletions(&storage, INTERRUPTED_DELETION_AGE, None).await;

        assert_eq!(
            report,
            RecoveryReport {
                retained: 1,
                ..RecoveryReport::default()
            },
            "a fresh journal entry must not be acted on"
        );
        assert!(
            staged.exists() && !live.exists(),
            "a deletion in flight must be left exactly as its own process left it"
        );
    }

    #[tokio::test]
    async fn an_entry_that_does_not_parse_is_reported_and_not_guessed_about() {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path());

        let staged = root.path().join("_ci_cache/7/archive.tar.zst.deleted-abc");
        write_file(&staged, "cache bytes");
        storage
            .put(
                &journal_key("22222222222222222222222222222222").unwrap(),
                b"not a journal entry",
            )
            .await
            .unwrap();

        let report = recover_interrupted_deletions(&storage, INTERRUPTED_DELETION_AGE, None).await;

        assert_eq!(
            report,
            RecoveryReport {
                failed: 1,
                ..RecoveryReport::default()
            },
            "an unreadable entry is a reported failure, never a decision"
        );
        assert!(
            staged.exists(),
            "bytes named by an entry the pass cannot read must be left where they are"
        );
    }

    #[tokio::test]
    async fn a_live_name_something_else_holds_is_never_overwritten() {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path());

        let live = root.path().join("_ci_cache/7/archive.tar.zst");
        let staged = root.path().join("_ci_cache/7/archive.tar.zst.deleted-abc");
        write_file(&staged, "old bytes");
        write_file(&live, "bytes written since");
        open_aged(
            &storage,
            "33333333333333333333333333333333",
            "CI cache archive",
            vec![StagedBytes::path(&live, &staged).unwrap()],
            AN_HOUR_AND_A_HALF,
        )
        .await;

        let report = recover_interrupted_deletions(&storage, INTERRUPTED_DELETION_AGE, None).await;

        assert_eq!(report.failed, 1, "{report:?}");
        assert_eq!(
            std::fs::read_to_string(&live).unwrap(),
            "bytes written since",
            "restoring must never replace whatever took the live name"
        );
        assert!(
            staged.exists(),
            "the tombstone stays until an operator reconciles the two"
        );
    }

    /// card_2c447a670cb5: a transfer moved the bytes and the row moved with
    /// them. The marker means the opposite of what it means for a deletion —
    /// destroying here would destroy the storage of a perfectly live
    /// repository under its new owner.
    #[tokio::test]
    async fn a_committed_move_keeps_its_bytes_where_the_new_row_names_them() {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path());

        let source = root.path().join("alice/demo.git");
        let destination = root.path().join("bob/demo.git");
        write_file(&destination.join("HEAD"), "ref: refs/heads/main");
        open_aged_with(
            &storage,
            "44444444444444444444444444444444",
            "repository transfer",
            vec![StagedBytes::path(&source, &destination).unwrap()],
            AN_HOUR_AND_A_HALF,
            Disposition::Keep,
        )
        .await;
        mark_committed(&storage, "44444444444444444444444444444444")
            .await
            .unwrap();

        let report = recover_interrupted_deletions(&storage, INTERRUPTED_DELETION_AGE, None).await;

        assert_eq!(
            report,
            RecoveryReport {
                kept: 1,
                ..RecoveryReport::default()
            },
            "a committed move must be left alone, not destroyed and not undone"
        );
        assert_eq!(
            std::fs::read_to_string(destination.join("HEAD")).unwrap(),
            "ref: refs/heads/main",
            "the bytes the committed row now names must still be there"
        );
        assert!(
            !source.exists(),
            "a committed move must not put its bytes back under the name it left"
        );
        assert!(
            !local_key_path(
                root.path(),
                &journal_key("44444444444444444444444444444444").unwrap()
            )
            .exists(),
            "a finished move must not leave its journal entry"
        );
    }

    /// The other side of the same kill: the row never moved, so the bytes have
    /// to come back to the name it still points at. This branch is shared with
    /// a deletion, and that is the point — only the marker's meaning differs.
    #[tokio::test]
    async fn an_uncommitted_move_puts_its_bytes_back_under_the_source() {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path());

        let source = root.path().join("alice/demo.git");
        let destination = root.path().join("bob/demo.git");
        write_file(&destination.join("HEAD"), "ref: refs/heads/main");
        open_aged_with(
            &storage,
            "55555555555555555555555555555555",
            "repository transfer",
            vec![StagedBytes::path(&source, &destination).unwrap()],
            AN_HOUR_AND_A_HALF,
            Disposition::Keep,
        )
        .await;

        let report = recover_interrupted_deletions(&storage, INTERRUPTED_DELETION_AGE, None).await;

        assert_eq!(
            report,
            RecoveryReport {
                restored: 1,
                ..RecoveryReport::default()
            },
            "a move whose metadata never committed must be undone"
        );
        assert_eq!(
            std::fs::read_to_string(source.join("HEAD")).unwrap(),
            "ref: refs/heads/main",
            "the source name the surviving row still points at must hold the bytes again"
        );
        assert!(
            !destination.exists(),
            "a move that was undone must not leave a copy at the destination"
        );
    }

    async fn repository_db() -> (rg_db::DatabaseConnection, i64) {
        let db = rg_db::connect_with_pool(
            "sqlite::memory:",
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let owner = rg_db::ops::user_ops::create_user(
            &db,
            "creation-recovery",
            "creation-recovery@example.invalid",
            "",
            "Creation Recovery",
        )
        .await
        .unwrap();
        (db, owner.id)
    }

    /// The exact crash window from card_5f5f349e92ec: intent is durable and
    /// the final blob is visible, but no attachment row was committed. The
    /// storage intentionally has no `local_path`, so recovery cannot quietly
    /// depend on filesystem access instead of the backend-neutral object API.
    #[tokio::test]
    async fn an_interrupted_attachment_publication_without_a_row_discards_its_blob() {
        let root = tempfile::tempdir().unwrap();
        let storage = OpaqueStorage::new(root.path());
        let (db, _) = repository_db().await;
        let key = BlobKey::new("attachments/7/11111111-1111-4111-8111-111111111111/evidence.txt")
            .unwrap();
        open_aged_with(
            &storage,
            "88888888888888888888888888888888",
            "attachment publication",
            vec![StagedBytes::attachment_creation(&key)],
            AN_HOUR_AND_A_HALF,
            Disposition::AttachmentCreation,
        )
        .await;
        storage.put(&key, b"unowned attachment").await.unwrap();

        let report =
            recover_interrupted_deletions(&storage, INTERRUPTED_DELETION_AGE, Some(&db)).await;

        assert_eq!(
            report,
            RecoveryReport {
                discarded_publications: 1,
                ..RecoveryReport::default()
            }
        );
        assert!(
            !storage.exists(&key).await.unwrap(),
            "a final blob with no owning attachment row must not survive forever"
        );
        assert!(
            storage
                .list(Some(&BlobKey::from_segments([DELETED, JOURNAL]).unwrap()))
                .await
                .unwrap()
                .is_empty(),
            "a recovered attachment publication must not leave its intent behind"
        );
    }

    /// The DB read is the destructive-action guard, not the marker. Removing
    /// or inverting `exists_by_blob_key` makes this test delete both live blobs.
    #[tokio::test]
    async fn live_attachment_rows_protect_blobs_with_or_without_a_commit_marker() {
        let root = tempfile::tempdir().unwrap();
        let storage = OpaqueStorage::new(root.path());
        let (db, owner_id) = repository_db().await;
        let now = Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                owner_id: Set(owner_id),
                name: Set("attachment-recovery".to_string()),
                is_private: Set(false),
                default_branch: Set("main".to_string()),
                stars_count: Set(0),
                forks_count: Set(0),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut keys = Vec::new();

        for (suffix, committed) in [("aaaaaaaa", false), ("bbbbbbbb", true)] {
            let uuid = format!("{suffix}-1111-4111-8111-111111111111");
            let key = BlobKey::from_segments([
                "attachments",
                repo.id.to_string().as_str(),
                uuid.as_str(),
                "evidence.txt",
            ])
            .unwrap();
            let publication_id = format!("{suffix}{suffix}{suffix}{suffix}");
            open_aged_with(
                &storage,
                &publication_id,
                "attachment publication",
                vec![StagedBytes::attachment_creation(&key)],
                AN_HOUR_AND_A_HALF,
                Disposition::AttachmentCreation,
            )
            .await;
            storage.put(&key, b"live attachment").await.unwrap();
            rg_db::ops::attachment_ops::create(
                &db,
                rg_db::entities::attachment::ActiveModel {
                    uuid: Set(uuid),
                    repo_id: Set(repo.id),
                    uploader_id: Set(Some(owner_id)),
                    issue_id: Set(None),
                    pull_request_id: Set(None),
                    issue_comment_id: Set(None),
                    review_comment_id: Set(None),
                    filename: Set("evidence.txt".to_string()),
                    blob_key: Set(key.to_string()),
                    content_type: Set("text/plain".to_string()),
                    size: Set(15),
                    download_count: Set(0),
                    created_at: Set(now),
                    sha256: Set(None),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            if committed {
                mark_committed(&storage, &publication_id).await.unwrap();
            }
            keys.push(key);
        }

        let report =
            recover_interrupted_deletions(&storage, INTERRUPTED_DELETION_AGE, Some(&db)).await;

        assert_eq!(
            report,
            RecoveryReport {
                kept: 2,
                ..RecoveryReport::default()
            }
        );
        for key in keys {
            assert!(
                storage.exists(&key).await.unwrap(),
                "a live attachment row must protect {key} regardless of marker state"
            );
        }
    }

    /// The artifact variant of the same crash window: the final UUID blob is
    /// visible, but the process died before `artifacts.file_path` was durable.
    #[tokio::test]
    async fn an_interrupted_artifact_publication_without_a_row_discards_its_blob() {
        let root = tempfile::tempdir().unwrap();
        let storage = OpaqueStorage::new(root.path());
        let (db, _) = repository_db().await;
        let key = BlobKey::new("artifacts/jobs/42/11111111-1111-4111-8111-111111111111-report.tar")
            .unwrap();
        open_aged_with(
            &storage,
            "99999999999999999999999999999999",
            "CI artifact publication",
            vec![StagedBytes::artifact_creation(&key)],
            AN_HOUR_AND_A_HALF,
            Disposition::ArtifactCreation,
        )
        .await;
        storage.put(&key, b"unowned artifact").await.unwrap();

        let report =
            recover_interrupted_deletions(&storage, INTERRUPTED_DELETION_AGE, Some(&db)).await;

        assert_eq!(
            report,
            RecoveryReport {
                discarded_publications: 1,
                ..RecoveryReport::default()
            }
        );
        assert!(
            !storage.exists(&key).await.unwrap(),
            "a final blob with no owning artifact row must not survive forever"
        );
    }

    /// The row lookup, not the optional marker, is the destructive-action
    /// guard. Mutating or bypassing `exists_by_file_path` deletes these live
    /// artifacts and fails on their bytes, not merely on a report counter.
    #[tokio::test]
    async fn live_artifact_rows_protect_blobs_with_or_without_a_commit_marker() {
        let root = tempfile::tempdir().unwrap();
        let storage = OpaqueStorage::new(root.path());
        let (db, _) = repository_db().await;
        let mut keys = Vec::new();

        for (suffix, committed) in [("cccccccc", false), ("dddddddd", true)] {
            let key = BlobKey::new(format!(
                "artifacts/jobs/42/{suffix}-1111-4111-8111-111111111111-report.tar"
            ))
            .unwrap();
            let publication_id = format!("{suffix}{suffix}{suffix}{suffix}");
            open_aged_with(
                &storage,
                &publication_id,
                "CI artifact publication",
                vec![StagedBytes::artifact_creation(&key)],
                AN_HOUR_AND_A_HALF,
                Disposition::ArtifactCreation,
            )
            .await;
            storage.put(&key, b"live artifact").await.unwrap();
            rg_db::ops::artifact_ops::create_artifact(
                &db,
                42,
                "report.tar",
                key.as_str(),
                13,
                None,
                None,
            )
            .await
            .unwrap();
            if committed {
                mark_committed(&storage, &publication_id).await.unwrap();
            }
            keys.push(key);
        }

        let report =
            recover_interrupted_deletions(&storage, INTERRUPTED_DELETION_AGE, Some(&db)).await;

        assert_eq!(
            report,
            RecoveryReport {
                kept: 2,
                ..RecoveryReport::default()
            }
        );
        for key in keys {
            assert!(
                storage.exists(&key).await.unwrap(),
                "a live artifact row must protect {key} regardless of marker state"
            );
        }
    }

    async fn package_version_for_recovery(
        db: &rg_db::DatabaseConnection,
        owner_id: i64,
    ) -> rg_db::entities::package_version::Model {
        let now = Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            db,
            rg_db::entities::repository::ActiveModel {
                owner_id: Set(owner_id),
                name: Set("package-recovery".to_string()),
                is_private: Set(false),
                default_branch: Set("main".to_string()),
                stars_count: Set(0),
                forks_count: Set(0),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let registry = rg_db::ops::package_registry_ops::create(db, repo.id, "generic")
            .await
            .unwrap();
        let package = rg_db::ops::package_ops::create(
            db,
            registry.id,
            owner_id,
            "recovery-package",
            None,
            None,
            None,
        )
        .await
        .unwrap();
        rg_db::ops::package_version_ops::create(
            db,
            package.id,
            "1.0.0",
            None,
            None,
            None,
            0,
            None,
            Some(owner_id),
        )
        .await
        .unwrap()
    }

    /// The package variant of the same crash window: a request-private object
    /// is visible, but no `package_file.storage_path` owns it yet.
    #[tokio::test]
    async fn an_interrupted_package_publication_without_a_row_discards_its_blob() {
        let root = tempfile::tempdir().unwrap();
        let storage = OpaqueStorage::new(root.path());
        let (db, _) = repository_db().await;
        let key = BlobKey::new(
            "packages/alice/demo/generic/pkg/1.0.0/objects/11111111111111111111111111111111/a.bin",
        )
        .unwrap();
        open_aged_with(
            &storage,
            "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
            "package file publication",
            vec![StagedBytes::package_file_creation(&key)],
            AN_HOUR_AND_A_HALF,
            Disposition::PackageFileCreation,
        )
        .await;
        storage.put(&key, b"unowned package file").await.unwrap();

        let report =
            recover_interrupted_deletions(&storage, INTERRUPTED_DELETION_AGE, Some(&db)).await;

        assert_eq!(
            report,
            RecoveryReport {
                discarded_publications: 1,
                ..RecoveryReport::default()
            }
        );
        assert!(
            !storage.exists(&key).await.unwrap(),
            "a final blob with no owning package-file row must not survive forever"
        );
    }

    /// The exact storage-path lookup, not the optional marker, is the
    /// destructive-action guard. Removing or inverting it deletes both blobs.
    #[tokio::test]
    async fn live_package_file_rows_protect_blobs_with_or_without_a_commit_marker() {
        let root = tempfile::tempdir().unwrap();
        let storage = OpaqueStorage::new(root.path());
        let (db, owner_id) = repository_db().await;
        let version = package_version_for_recovery(&db, owner_id).await;
        let mut keys = Vec::new();

        for (suffix, committed) in [("ffffffff", false), ("abababab", true)] {
            let filename = format!("{suffix}.bin");
            let key = BlobKey::new(format!(
                "packages/alice/demo/generic/pkg/1.0.0/objects/{suffix}{suffix}{suffix}{suffix}/{filename}"
            ))
            .unwrap();
            let publication_id = suffix.repeat(4);
            open_aged_with(
                &storage,
                &publication_id,
                "package file publication",
                vec![StagedBytes::package_file_creation(&key)],
                AN_HOUR_AND_A_HALF,
                Disposition::PackageFileCreation,
            )
            .await;
            storage.put(&key, b"live package file").await.unwrap();
            rg_db::ops::package_file_ops::create(
                &db,
                version.id,
                &filename,
                17,
                rg_db::ops::package_file_ops::FileDigests::default(),
                key.as_str(),
            )
            .await
            .unwrap();
            if committed {
                mark_committed(&storage, &publication_id).await.unwrap();
            }
            keys.push(key);
        }

        let report =
            recover_interrupted_deletions(&storage, INTERRUPTED_DELETION_AGE, Some(&db)).await;

        assert_eq!(
            report,
            RecoveryReport {
                kept: 2,
                ..RecoveryReport::default()
            }
        );
        for key in keys {
            assert!(
                storage.exists(&key).await.unwrap(),
                "a live package-file row must protect {key} regardless of marker state"
            );
        }
    }

    /// A failed ownership read authorizes nothing: both the final bytes and the
    /// durable intent remain for a later startup with a healthy database.
    #[tokio::test]
    async fn a_package_ownership_read_error_keeps_the_blob_and_intent() {
        let root = tempfile::tempdir().unwrap();
        let storage = OpaqueStorage::new(root.path());
        let db_without_schema = rg_db::connect_with_pool(
            "sqlite::memory:",
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .unwrap();
        let key = BlobKey::new(
            "packages/alice/demo/generic/pkg/1.0.0/objects/cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd/a.bin",
        )
        .unwrap();
        open_aged_with(
            &storage,
            "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd",
            "package file publication",
            vec![StagedBytes::package_file_creation(&key)],
            AN_HOUR_AND_A_HALF,
            Disposition::PackageFileCreation,
        )
        .await;
        storage.put(&key, b"uncertain package file").await.unwrap();

        let report = recover_interrupted_deletions(
            &storage,
            INTERRUPTED_DELETION_AGE,
            Some(&db_without_schema),
        )
        .await;

        assert_eq!(report.failed, 1);
        assert!(storage.exists(&key).await.unwrap());
        assert_eq!(
            storage
                .list(Some(&BlobKey::from_segments([DELETED, JOURNAL]).unwrap()))
                .await
                .unwrap()
                .len(),
            1,
            "a DB error must leave the only durable name of the blob intact"
        );
    }

    /// The cache variant of the post-write/pre-upsert crash: the final archive
    /// is not a spool any more, and row-driven retention cannot discover it.
    #[tokio::test]
    async fn an_interrupted_cache_publication_without_a_row_discards_its_archive() {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path());
        let (db, _) = repository_db().await;
        let archive = root
            .path()
            .join("_ci_cache/7/aaaaaaaa.11111111111111111111111111111111.tar");
        open_aged_with(
            &storage,
            "11111111111111111111111111111111",
            "CI cache publication",
            vec![StagedBytes::cache_creation(&archive, 7).unwrap()],
            AN_HOUR_AND_A_HALF,
            Disposition::CacheCreation,
        )
        .await;
        write_file(&archive, "unowned cache archive");

        let report = recover_interrupted_storage_at(&db, root.path(), Duration::ZERO).await;

        assert_eq!(
            report,
            RecoveryReport {
                discarded_publications: 1,
                ..RecoveryReport::default()
            }
        );
        assert!(
            !archive.exists(),
            "a final cache archive with no owning row must not survive forever"
        );
    }

    /// The exact `(repo_id, file_path)` lookup is the destructive-action guard,
    /// not the optional marker. Removing or inverting it deletes both archives.
    #[tokio::test]
    async fn live_cache_rows_protect_archives_with_or_without_a_commit_marker() {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path());
        let (db, owner_id) = repository_db().await;
        let now = Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                owner_id: Set(owner_id),
                name: Set("cache-recovery".to_string()),
                is_private: Set(false),
                default_branch: Set("main".to_string()),
                stars_count: Set(0),
                forks_count: Set(0),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let mut archives = Vec::new();

        for (suffix, committed) in [("22222222", false), ("33333333", true)] {
            let key_hash = suffix.repeat(8);
            let publication_id = suffix.repeat(4);
            let archive = root
                .path()
                .join("_ci_cache")
                .join(repo.id.to_string())
                .join(format!("{key_hash}.{publication_id}.tar"));
            open_aged_with(
                &storage,
                &publication_id,
                "CI cache publication",
                vec![StagedBytes::cache_creation(&archive, repo.id).unwrap()],
                AN_HOUR_AND_A_HALF,
                Disposition::CacheCreation,
            )
            .await;
            write_file(&archive, "live cache archive");
            rg_db::ops::ci_retention_ops::upsert_cache_entry(
                &db,
                repo.id,
                &key_hash,
                archive.to_string_lossy().as_ref(),
                18,
                None,
                7,
            )
            .await
            .unwrap();
            if committed {
                mark_committed(&storage, &publication_id).await.unwrap();
            }
            archives.push(archive);
        }

        let report = recover_interrupted_storage_at(&db, root.path(), Duration::ZERO).await;

        assert_eq!(
            report,
            RecoveryReport {
                kept: 2,
                ..RecoveryReport::default()
            }
        );
        for archive in archives {
            assert!(
                archive.is_file(),
                "a live cache row must protect {} regardless of marker state",
                archive.display()
            );
        }
    }

    #[tokio::test]
    async fn an_interrupted_repository_create_without_a_row_releases_its_name() {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path());
        let (db, owner_id) = repository_db().await;
        let path = root.path().join("creation-recovery/demo.git");
        open_repository_creation(
            &storage,
            "66666666666666666666666666666666",
            "repository creation",
            &path,
            owner_id,
            None,
            "demo",
        )
        .await
        .unwrap();
        write_file(&path.join("HEAD"), "ref: refs/heads/main");

        let report = recover_interrupted_storage_at(&db, root.path(), Duration::ZERO).await;

        assert_eq!(
            report,
            RecoveryReport {
                discarded_creations: 1,
                ..RecoveryReport::default()
            }
        );
        assert!(
            !path.exists(),
            "a final path with no repository row still occupies the name"
        );
        crate::repo::service::create_repo(&db, owner_id, "demo", None, false, root.path(), None)
            .await
            .expect("the recovered repository name must be creatable again");
        assert!(path.join("HEAD").is_file());
    }

    #[tokio::test]
    async fn a_live_repository_survives_even_without_a_creation_commit_marker() {
        let root = tempfile::tempdir().unwrap();
        let storage = storage(root.path());
        let (db, owner_id) = repository_db().await;
        crate::repo::service::create_repo(&db, owner_id, "live", None, false, root.path(), None)
            .await
            .unwrap();
        let path = root.path().join("creation-recovery/live.git");
        open_repository_creation(
            &storage,
            "77777777777777777777777777777777",
            "repository creation",
            &path,
            owner_id,
            None,
            "live",
        )
        .await
        .unwrap();

        let report = recover_interrupted_storage_at(&db, root.path(), Duration::ZERO).await;

        assert_eq!(
            report,
            RecoveryReport {
                kept: 1,
                ..RecoveryReport::default()
            }
        );
        assert!(
            path.join("HEAD").is_file(),
            "a missing marker must not let recovery delete a repository whose row exists"
        );
    }

    /// An entry a build without dispositions wrote carries no field at all, and
    /// it meant a deletion. Reading it as anything else would turn the first
    /// upgrade into a restore of bytes no row names.
    #[test]
    fn an_entry_written_before_dispositions_reads_as_a_deletion() {
        let entry: DeletionJournalEntry = serde_json::from_str(
            r#"{"deletion_id":"aa","what":"CI artifact","staged_at":"2026-01-01T00:00:00Z","staged":[]}"#,
        )
        .expect("an entry from a build without the field must still parse");
        assert_eq!(entry.disposition, Disposition::Destroy);
    }

    #[test]
    fn a_deletion_id_that_is_not_one_cannot_name_a_journal_key() {
        assert!(journal_key("../../etc").is_err());
        assert!(journal_key("").is_err());
        assert!(committed_key("id with spaces").is_err());
        assert!(journal_key("0123456789abcdef0123456789abcdef").is_ok());
    }
}
