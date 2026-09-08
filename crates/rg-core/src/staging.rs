//! Where an in-flight request spools to disk, and who retires what it left.
//!
//! Four handlers stream a request body into a private file under
//! `<repo_root>/.tmp/` before they know whether the request will succeed:
//! package publishes, release assets, git request bodies and issue attachments.
//! Each of them relies on `tempfile::TempPath` — or an explicit `remove_file` —
//! to retire the spool on every path out of the handler, which covers every
//! outcome the *process* survives: success, refusal, transport error, panic.
//!
//! It does not cover the process not surviving. `SIGKILL`, the OOM killer and a
//! container restart run no destructors, so a spool caught mid-write is simply
//! left on the volume — and until this module existed nothing ever read those
//! directories again, so a 512 MiB package upload interrupted by a restart was
//! a permanent 512 MiB. The operator learns about it when `.tmp` has eaten the
//! partition the repositories themselves live on.
//!
//! [`sweep_stale_spools`] is the reverse arc: one pass at startup that retires
//! spools no longer plausibly attached to a live request. It is deliberately
//! ONE pass over a list, not a fourth copy of a cleanup loop — the directories
//! were spelled in three different modules, and a fifth staging area added
//! tomorrow has to be swept without anybody remembering to extend a sweep.
//! [`StagingArea::ALL`] is that single declaration, and
//! `staging_registry_is_the_only_producer_of_tmp_paths` is what keeps a new
//! area from being spelled past it.
//!
//! Not every spool can live under `.tmp/`. Five more producers write theirs
//! *beside* the destination — an LFS object, a CI cache archive, any blob a
//! local backend writes, an audit archive, the rollback copy of an attachment
//! being deleted. For the first four the reason is that publishing is a rename,
//! and a rename that stays inside one directory cannot fail across a device
//! boundary the way a move out of `<repo_root>/.tmp/` onto a bind-mounted
//! volume can. That is a deliberate and correct choice, so [`StagingArea`] must
//! not swallow them: their roots are per-repository (`<owner>.lfs/<repo>`,
//! `_ci_cache/<repo_id>`) or configured somewhere else entirely. They get
//! [`SiblingSpool`] instead — the same pair of halves, a namer the producer
//! calls and a matcher the sweep asks, so the two cannot drift apart.
//!
//! The fifth is there for the opposite reason: it is never renamed anywhere,
//! and it used to be written into the system temp directory — a tree under no
//! ForgeKeep root, which nothing here can sweep and which is a tmpfs on a
//! typical deployment. Putting it beside the blob it protects is what brings it
//! inside the walk.
//!
//! Two more producers stage neither a file nor a rename but a whole *tree*: an
//! import clones the upstream — and, separately, its wiki — into a directory
//! beside the target before moving it into place. That clone is the longest
//! step an import has, minutes on a large upstream, so it is by a wide margin
//! the likeliest thing to be interrupted; and what it leaves behind is a
//! partial bare repository the size of what it had transferred, named by no
//! row and reachable from no other pass. The file half of this sweep can
//! neither delete one nor even see it, because it retires regular files only.
//! [`SiblingSpoolTree`] is the same pair of halves for those — a namer the
//! producer calls, a matcher the sweep asks — with `remove_dir_all` in place of
//! `remove_file`, and with the walk stopping at the tree rather than descending
//! into an object directory it has no reason to read.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a spool must have gone untouched before the startup sweep retires
/// it.
///
/// The sweep runs before this process serves anything, so its own requests
/// cannot be caught by it. The age bound is for the case it cannot rule out:
/// a second process — a rolling restart with an overlap window, or a second
/// container pointed at the same `repo_root` — with a genuinely live upload in
/// flight. Twelve hours is far above any plausible single request (the largest
/// ceiling any of these areas carries is 512 MiB) and far below "forever",
/// which is what the retention was before.
pub const STALE_SPOOL_AGE: Duration = Duration::from_secs(12 * 60 * 60);

/// One staging area: a directory under `<repo_root>/.tmp/` that a handler
/// spools an in-flight request body into.
///
/// Adding a variant is what registers a new area with the startup sweep. The
/// guard test in this module fails if a handler builds a `.tmp` path any other
/// way, so the two cannot drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StagingArea {
    /// `POST` of a package to any of the registries — up to
    /// `[server].package_upload_max_bytes` per file.
    PackageUploads,
    /// A release asset upload — up to `[server].package_upload_max_bytes`.
    ReleaseUploads,
    /// A git request body (`git-receive-pack` push, `git-upload-pack`
    /// negotiation) staged before it is handed to rg-git.
    GitRequests,
    /// An issue / pull-request / comment attachment, staged before it reaches
    /// the blob store.
    Attachments,
}

impl StagingArea {
    /// Every staging area this binary spools into.
    ///
    /// The startup sweep iterates exactly this slice; nothing else enumerates
    /// staging directories.
    pub const ALL: &'static [StagingArea] = &[
        StagingArea::PackageUploads,
        StagingArea::ReleaseUploads,
        StagingArea::GitRequests,
        StagingArea::Attachments,
    ];

    /// The directory name under `<repo_root>/.tmp/`.
    pub const fn directory_name(self) -> &'static str {
        match self {
            StagingArea::PackageUploads => "package-uploads",
            StagingArea::ReleaseUploads => "release-uploads",
            StagingArea::GitRequests => "git-requests",
            StagingArea::Attachments => "attachments",
        }
    }

    /// Where this area lives under `repo_root`.
    ///
    /// The `.tmp` literal is spelled here and only here on purpose: the guard
    /// test anchors on it, so a handler that builds its own `.tmp` path is a
    /// red test rather than a directory nobody sweeps.
    pub fn path_in(self, repo_root: &Path) -> PathBuf {
        repo_root.join(".tmp").join(self.directory_name())
    }
}

// ── Spools written beside their destination ────────────────────────────────

/// The `tempfile::Builder` prefix of a CI cache upload spool.
///
/// This family is the one whose name is not built here: `tempfile` composes it
/// from a prefix, its own random middle and a suffix, so the two halves are
/// constants the producer hands over rather than a `format!` this module owns.
pub const CI_CACHE_SPOOL_PREFIX: &str = "cache-";
/// The `tempfile::Builder` suffix of a CI cache upload spool.
pub const CI_CACHE_SPOOL_SUFFIX: &str = ".upload";

/// The spool `upload_object` streams an LFS object into before it has been
/// verified against the oid the client claimed.
///
/// Derived from the oid rather than random, so a repeat of the same upload
/// reuses one file instead of adding a second — which is why this family leaks
/// per *distinct* oid rather than per request.
pub fn lfs_object_spool_name(oid: &str) -> String {
    format!(".tmp_{oid}")
}

/// The spool a local blob write publishes with a same-directory rename.
pub fn blob_write_spool_name(destination: &str, write_id: uuid::Uuid) -> String {
    format!(".{destination}.{write_id}.tmp")
}

/// The spool one audit-archive run compresses into.
pub fn audit_archive_spool_name(archive_id: uuid::Uuid) -> String {
    format!(".audit-{archive_id}.tmp")
}

/// The copy `attachment::delete_attachment` keeps of a blob while it deletes
/// it, so a metadata delete that fails can put the bytes back.
///
/// Written beside the blob it protects rather than in the system temp
/// directory, which is where it used to go. `TMPDIR` is under no ForgeKeep
/// root, so nothing swept it — and on a typical deployment `/tmp` is a tmpfs,
/// which makes a leaked 100 MiB copy memory rather than disk. Beside the blob
/// it is inside the tree [`sweep_stale_sibling_spools`] walks, and it inherits
/// the storage directory's permissions instead of a directory every other
/// process on the host can read.
pub fn attachment_backup_spool_name(backup_id: uuid::Uuid) -> String {
    format!(".attachment-backup-{backup_id}.tmp")
}

/// One family of spool written next to its destination instead of under
/// `.tmp/`.
///
/// A variant is registered by appearing in [`SiblingSpool::ALL`], and it is
/// worth being clear about what that registry can and cannot promise. It is not
/// a list of directories — these have no fixed directory — it is the list of
/// *names* the sweep recognises. So the compile-time half is what carries the
/// weight: every producer builds its spool name through the namer beside its
/// variant, which means a producer that changes the shape of its name changes
/// the matcher with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SiblingSpool {
    /// `.tmp_<oid>` beside one repository's LFS objects, up to
    /// `LFS_OBJECT_MAX_BYTES` — by a wide margin the most expensive of the four.
    LfsObject,
    /// `cache-<random>.upload` in `_ci_cache/<repo_id>/`. Retention walks cache
    /// *rows*, and a spool is named by no row, so nothing else can find one.
    CiCacheArchive,
    /// `.<destination>.<uuid>.tmp` beside any blob a local backend writes.
    BlobWrite,
    /// `.audit-<uuid>.tmp` in the audit archive directory.
    AuditArchive,
    /// `.attachment-backup-<uuid>.tmp` beside an attachment blob being deleted,
    /// up to `attachment::MAX_ATTACHMENT_SIZE`. The only family here that is
    /// never renamed into place: it is a rollback copy, restored through the
    /// storage backend and otherwise dropped.
    AttachmentBackup,
}

impl SiblingSpool {
    /// Every spool family written beside its destination.
    pub const ALL: &'static [SiblingSpool] = &[
        SiblingSpool::LfsObject,
        SiblingSpool::CiCacheArchive,
        SiblingSpool::BlobWrite,
        SiblingSpool::AuditArchive,
        SiblingSpool::AttachmentBackup,
    ];

    /// Whether `name` is a spool of this family.
    ///
    /// Strict on purpose — every variable part has to parse back — because this
    /// predicate is the whole of what stands between the sweep and somebody
    /// else's file. `backup::is_temp_snapshot` is the same shape for the same
    /// reason: these spools share their directory with the live objects, so a
    /// loose predicate here does not waste space, it destroys data.
    pub fn matches(self, name: &str) -> bool {
        match self {
            SiblingSpool::LfsObject => name
                .strip_prefix(".tmp_")
                .is_some_and(crate::lfs::service::is_valid_oid),
            SiblingSpool::CiCacheArchive => name
                .strip_prefix(CI_CACHE_SPOOL_PREFIX)
                .and_then(|rest| rest.strip_suffix(CI_CACHE_SPOOL_SUFFIX))
                .is_some_and(|random| {
                    !random.is_empty() && random.bytes().all(|byte| byte.is_ascii_alphanumeric())
                }),
            // The destination name may itself contain dots, so the id is taken
            // from the right — `.pkg.tar.gz.<uuid>.tmp` is one spool of
            // `pkg.tar.gz`, not a malformed anything.
            SiblingSpool::BlobWrite => name
                .strip_prefix('.')
                .and_then(|rest| rest.strip_suffix(".tmp"))
                .and_then(|rest| rest.rsplit_once('.'))
                .is_some_and(|(destination, write_id)| {
                    !destination.is_empty() && uuid::Uuid::parse_str(write_id).is_ok()
                }),
            SiblingSpool::AuditArchive => name
                .strip_prefix(".audit-")
                .and_then(|rest| rest.strip_suffix(".tmp"))
                .is_some_and(|archive_id| uuid::Uuid::parse_str(archive_id).is_ok()),
            SiblingSpool::AttachmentBackup => name
                .strip_prefix(".attachment-backup-")
                .and_then(|rest| rest.strip_suffix(".tmp"))
                .is_some_and(|backup_id| uuid::Uuid::parse_str(backup_id).is_ok()),
        }
    }
}

/// Whether `name` is a spool of any family in [`SiblingSpool::ALL`].
///
/// Private: the sweep below is the only thing that has a reason to ask, and the
/// producers ask the namers instead.
fn is_sibling_spool(name: &str) -> bool {
    SiblingSpool::ALL.iter().any(|spool| spool.matches(name))
}

// ── Whole trees staged beside their destination ────────────────────────────

/// The directory an import clones an upstream repository into before moving it
/// onto `<owner>/<name>.git`.
///
/// `token` is the import pass's own id, so two passes over one repository —
/// and a pass racing another import of the same name — cannot share a tree.
/// It is also the part that has to parse back: that is the whole of what
/// stands between the sweep and a directory belonging to somebody else.
pub fn import_clone_staging_name(name: &str, token: uuid::Uuid) -> String {
    format!(".{name}.git.importing-{}", token.simple())
}

/// The directory an import clones a source *wiki* into.
///
/// A wiki is a second repository one path suffix away, cloned the same way and
/// read for its pages rather than installed, so it is staged beside the
/// repository it belongs to under the same shape of name.
pub fn import_wiki_clone_staging_name(name: &str, token: uuid::Uuid) -> String {
    format!(".{name}.wiki.git.importing-{}", token.simple())
}

/// One family of *tree* staged beside its destination, as opposed to the single
/// files [`SiblingSpool`] covers.
///
/// Both members are an import's clone of an upstream. They are a registry of
/// their own rather than two more [`SiblingSpool`] variants because retiring
/// one is `remove_dir_all` rather than `remove_file`, and because the sweep
/// must not *walk* one: a partial bare repository holds exactly the loose
/// object and pack directories [`is_skipped_directory`] exists to keep a
/// startup pass out of. Recognised by name and retired or kept whole, a leaked
/// clone costs one `stat`; walked, it costs a full traversal of everything the
/// interrupted transfer had written, on every start, for as long as the leak
/// lasts.
///
/// What is deliberately NOT here: `.<name>.git.replaced-<token>`, the skeleton
/// an install moves aside on its way to putting the clone on the target path.
/// That directory is declared in the deletion-recovery journal *before* it
/// exists and is finished — put back, or destroyed — by the startup pass that
/// reads the journal. That pass runs after this sweep, so retiring the
/// skeleton here on age alone would delete bytes it is about to restore.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SiblingSpoolTree {
    /// `.<name>.git.importing-<token>` beside `<owner>/<name>.git` — a bare
    /// clone of the upstream, as large as the upstream is.
    ImportClone,
    /// `.<name>.wiki.git.importing-<token>` beside the same repository — the
    /// source wiki, cloned to be read.
    ImportWikiClone,
}

impl SiblingSpoolTree {
    /// Every tree staged beside its destination.
    pub const ALL: &'static [SiblingSpoolTree] = &[
        SiblingSpoolTree::ImportClone,
        SiblingSpoolTree::ImportWikiClone,
    ];

    /// Whether `name` is a staged tree of this family.
    ///
    /// Strict for the reason [`SiblingSpool::matches`] is strict, and then some:
    /// what a match authorises here is `remove_dir_all` on a directory sitting
    /// among live bare repositories. Every variable part has to parse back —
    /// the leading dot, a non-empty repository name, and a token that is a
    /// real uuid.
    pub fn matches(self, name: &str) -> bool {
        let Some((stem, token)) = name
            .strip_prefix('.')
            .and_then(|rest| rest.rsplit_once(".importing-"))
        else {
            return false;
        };
        if uuid::Uuid::parse_str(token).is_err() {
            return false;
        }
        match self {
            // A repository genuinely named `x.wiki` stages its own clone under
            // a name the wiki family claims. Both are an import's clone and
            // both are retired identically, so the overlap costs nothing —
            // what matters is that exactly one family owns each name.
            SiblingSpoolTree::ImportClone => stem
                .strip_suffix(".git")
                .is_some_and(|repo| !repo.is_empty() && !repo.ends_with(".wiki")),
            SiblingSpoolTree::ImportWikiClone => stem
                .strip_suffix(".wiki.git")
                .is_some_and(|repo| !repo.is_empty()),
        }
    }
}

/// Whether `name` is a staged tree of any family in [`SiblingSpoolTree::ALL`].
fn is_sibling_spool_tree(name: &str) -> bool {
    SiblingSpoolTree::ALL.iter().any(|tree| tree.matches(name))
}

/// What one sweep did, so the caller can say it in a single log line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Spools older than the bound that were deleted — a file, or a whole
    /// staged tree.
    pub removed: usize,
    /// Spools young enough to still belong to a live request or a live import.
    pub retained: usize,
    /// Entries the sweep could not read or could not delete.
    pub failed: usize,
}

impl SweepReport {
    fn merge(&mut self, other: SweepReport) {
        self.removed += other.removed;
        self.retained += other.retained;
        self.failed += other.failed;
    }
}

/// Retire spools left behind by a stop that ran no destructors.
///
/// Best-effort by construction: a directory that does not exist yet is not a
/// problem (no request of that kind has run), and a single unreadable or
/// undeletable entry is counted and logged rather than allowed to hold up
/// startup. Serving with an unswept spool is strictly better than not serving.
///
/// Only regular files are touched. A symlink is never followed and never
/// removed — nothing in these directories creates one, so its presence means
/// something the sweep does not understand put it there.
pub async fn sweep_stale_spools(repo_root: &Path, older_than: Duration) -> SweepReport {
    let mut report = SweepReport::default();
    for area in StagingArea::ALL {
        report.merge(sweep_one_area(&area.path_in(repo_root), older_than).await);
    }
    report.merge(sweep_stale_sibling_spools(repo_root, older_than).await);
    report
}

/// Retire [`SiblingSpool`] files anywhere under `root`.
///
/// One walk rather than one pass per family, because there is no list of
/// directories to pass over: two of the families are rooted per repository
/// (`<owner>.lfs/<repo>`, `_ci_cache/<repo_id>`) and a blob write spools beside
/// whatever key it is publishing, at whatever depth that key has. Enumerating
/// those roots would mean re-deriving, here, a layout that lives in three other
/// modules — and getting it subtly wrong is invisible, because the failure is a
/// pass that finds nothing. The walk asks the filesystem instead, and
/// [`is_sibling_spool`] is what decides.
///
/// Split out from [`sweep_stale_spools`] so the audit archive directory — which
/// is configured separately and is by default a *sibling* of `repo_root`, not
/// inside it — can be swept by whoever knows where it is.
pub async fn sweep_stale_sibling_spools(root: &Path, older_than: Duration) -> SweepReport {
    let mut report = SweepReport::default();
    let mut pending = vec![root.to_path_buf()];

    while let Some(directory) = pending.pop() {
        let mut entries = match tokio::fs::read_dir(&directory).await {
            Ok(entries) => entries,
            // Nothing of this kind exists on this instance yet.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                tracing::warn!(
                    path = %directory.display(),
                    %error,
                    "failed to read a storage directory; leftover spools under it were not swept"
                );
                report.failed += 1;
                continue;
            }
        };

        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(
                        path = %directory.display(),
                        %error,
                        "failed to walk a storage directory; the remaining spools were not swept"
                    );
                    report.failed += 1;
                    break;
                }
            };

            let path = entry.path();
            let metadata = match tokio::fs::symlink_metadata(&path).await {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    tracing::warn!(path = %path.display(), %error, "failed to stat a stored file");
                    report.failed += 1;
                    continue;
                }
            };

            // A symlink reports neither `is_dir` nor `is_file` here, so it is
            // neither descended into nor deleted — the same refusal to follow
            // one the staging areas make, and for the same reason.
            if metadata.is_dir() {
                // Asked before `is_skipped_directory`, and that order is the
                // point: a staged clone is a bare repository under a name that
                // does not end in `.git`, so the skip below would not recognise
                // one and the walk would descend into its object tree — the
                // exact cost that skip exists to avoid, paid on every start for
                // as long as the leak lasts. Recognised here it is retired or
                // kept, and either way not entered.
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(is_sibling_spool_tree)
                {
                    retire_tree_if_stale(&path, &metadata, older_than, &mut report).await;
                    continue;
                }
                if !is_skipped_directory(&path).await {
                    pending.push(path);
                }
                continue;
            }
            if !metadata.is_file() {
                continue;
            }

            let name = entry.file_name();
            let is_spool = name.to_str().is_some_and(is_sibling_spool);
            if is_spool {
                retire_if_stale(&path, &metadata, older_than, &mut report).await;
            }
        }
    }

    report
}

/// Directories the sibling walk does not enter.
///
/// `.tmp` belongs to [`StagingArea`] and has already been swept by name, so
/// walking it again would only double-count what it holds.
///
/// A bare git repository is skipped because it is the one tree under the
/// storage root large enough to turn a startup pass into minutes of `stat`
/// calls, and no spool of any family can be inside one. The `HEAD` probe is
/// what keeps that skip from resting on the name alone: a blob whose key
/// happens to end in `.git` is still walked.
async fn is_skipped_directory(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if name == ".tmp" {
        return true;
    }
    name.ends_with(".git") && tokio::fs::symlink_metadata(path.join("HEAD")).await.is_ok()
}

async fn sweep_one_area(directory: &Path, older_than: Duration) -> SweepReport {
    let mut report = SweepReport::default();

    let mut entries = match tokio::fs::read_dir(directory).await {
        Ok(entries) => entries,
        // Nothing of this kind has been staged on this instance yet.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return report,
        Err(error) => {
            tracing::warn!(
                path = %directory.display(),
                %error,
                "failed to read a staging directory; leftover spools were not swept"
            );
            report.failed += 1;
            return report;
        }
    };

    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(error) => {
                tracing::warn!(
                    path = %directory.display(),
                    %error,
                    "failed to walk a staging directory; the remaining spools were not swept"
                );
                report.failed += 1;
                break;
            }
        };

        let path = entry.path();
        let metadata = match tokio::fs::symlink_metadata(&path).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "failed to stat a staged spool");
                report.failed += 1;
                continue;
            }
        };
        if !metadata.is_file() {
            continue;
        }

        retire_if_stale(&path, &metadata, older_than, &mut report).await;
    }

    report
}

/// Delete one spool if it is provably too old to belong to a live request, and
/// record which of the two happened.
///
/// Shared by both passes so the age bound — the half of the sweep that stops it
/// destroying a live 10 GiB upload — is decided in exactly one place.
async fn retire_if_stale(
    path: &Path,
    metadata: &std::fs::Metadata,
    older_than: Duration,
    report: &mut SweepReport,
) {
    // A modification time the platform cannot give, or one in the future
    // (clock skew, a restored volume), reads as "not provably stale" and
    // keeps the file. The sweep exists to reclaim space, not to be the
    // reason a live upload disappears.
    let stale = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .is_some_and(|age| age >= older_than);
    if !stale {
        report.retained += 1;
        return;
    }

    match tokio::fs::remove_file(path).await {
        Ok(()) => {
            tracing::info!(
                path = %path.display(),
                bytes = metadata.len(),
                "removed a staged upload left behind by a previous run"
            );
            report.removed += 1;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "failed to remove a staged upload left behind by a previous run"
            );
            report.failed += 1;
        }
    }
}

/// Delete one staged tree if it is provably too old to belong to a live
/// import, and record which of the two happened.
///
/// The twin of [`retire_if_stale`] for a directory: `remove_dir_all` rather
/// than `remove_file`, and an age asked of the tree rather than read off the
/// directory's own metadata — [`staging_tree_age`] is where those two stop
/// being the same number.
async fn retire_tree_if_stale(
    path: &Path,
    metadata: &std::fs::Metadata,
    older_than: Duration,
    report: &mut SweepReport,
) {
    let stale = staging_tree_age(path, metadata)
        .await
        .is_some_and(|age| age >= older_than);
    if !stale {
        report.retained += 1;
        return;
    }

    match tokio::fs::remove_dir_all(path).await {
        Ok(()) => {
            tracing::info!(
                path = %path.display(),
                "removed a partial import clone left behind by a previous run"
            );
            report.removed += 1;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(
                path = %path.display(),
                %error,
                "failed to remove a partial import clone left behind by a previous run"
            );
            report.failed += 1;
        }
    }
}

/// How long ago the clone staged under `path` last received a byte, or `None`
/// when that cannot be established.
///
/// Not the directory's own mtime, which is a different and much older number.
/// `git clone --bare` writes the repository skeleton into the top level in its
/// first moments and then does not touch it again for the whole of the
/// transfer — the part that takes minutes. Dating the tree by its top would
/// therefore date it from when the clone *started*, and the age bound would be
/// guarding against the wrong thing: the case it exists for is a second process
/// with a genuinely live import, which is precisely the case where the tree is
/// old at the top and being written to underneath.
///
/// The incoming pack is what advances. Git streams it into a temporary file in
/// `objects/pack/` and renames it into place at the end, so the newest mtime in
/// that one directory is the last byte this clone received. It is a single
/// `read_dir` of a directory holding a handful of entries — not the traversal
/// of the object tree that recognising the family by name is what avoids.
///
/// `None` — an mtime the platform cannot give, one in the future, or a pack
/// directory that cannot be read to the end — reads as "not provably stale" and
/// keeps the tree, the same fail-safe direction [`retire_if_stale`] takes.
async fn staging_tree_age(path: &Path, metadata: &std::fs::Metadata) -> Option<Duration> {
    let mut newest = metadata.modified().ok()?;

    match tokio::fs::read_dir(path.join("objects").join("pack")).await {
        Ok(mut entries) => loop {
            match entries.next_entry().await {
                Ok(Some(entry)) => {
                    if let Ok(modified) = tokio::fs::symlink_metadata(entry.path())
                        .await
                        .and_then(|metadata| metadata.modified())
                    {
                        newest = newest.max(modified);
                    }
                }
                Ok(None) => break,
                // A directory that cannot be read to the end is one whose
                // freshest write is unknown, and "unknown" resolving to "old"
                // is how a live clone gets deleted.
                Err(_) => return None,
            }
        },
        // The clone has not written a pack directory yet, so the tree's own
        // mtime is the whole story.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return None,
    }

    newest.elapsed().ok()
}

#[cfg(test)]
mod tests {
    use super::{
        attachment_backup_spool_name, audit_archive_spool_name, blob_write_spool_name,
        import_clone_staging_name, import_wiki_clone_staging_name, is_sibling_spool,
        is_sibling_spool_tree, lfs_object_spool_name, sweep_stale_spools, SiblingSpool,
        SiblingSpoolTree, StagingArea, SweepReport, CI_CACHE_SPOOL_PREFIX, CI_CACHE_SPOOL_SUFFIX,
        STALE_SPOOL_AGE,
    };
    use std::time::{Duration, SystemTime};

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    fn age_file(path: &std::path::Path, by: Duration) {
        let when = SystemTime::now() - by;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open the spool to backdate it");
        file.set_times(std::fs::FileTimes::new().set_modified(when))
            .expect("backdate the spool");
    }

    /// Backdating a directory takes a read-only handle: a directory cannot be
    /// opened for writing, and the timestamps are set on the inode rather than
    /// through the handle anyway.
    fn age_directory(path: &std::path::Path, by: Duration) {
        let when = SystemTime::now() - by;
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .open(path)
            .expect("open the staged tree to backdate it");
        directory
            .set_times(std::fs::FileTimes::new().set_modified(when))
            .expect("backdate the staged tree");
    }

    /// A bare repository as an interrupted `git clone --bare` leaves one: the
    /// skeleton at the top, and the pack it was still receiving underneath.
    fn write_partial_clone(path: &std::path::Path) {
        let pack = path.join("objects").join("pack");
        std::fs::create_dir_all(&pack).expect("clone objects");
        std::fs::create_dir_all(path.join("refs")).expect("clone refs");
        std::fs::write(path.join("HEAD"), b"ref: refs/heads/main\n").expect("clone HEAD");
        std::fs::write(pack.join("tmp_pack_incoming"), b"a pack in flight").expect("incoming pack");
    }

    /// Backdate a whole staged clone — the tree's own mtime *and* the incoming
    /// pack's, which is the newer of the two while a transfer is running and
    /// therefore the one the sweep dates the tree by.
    fn age_tree(path: &std::path::Path, by: Duration) {
        let pack = path.join("objects").join("pack");
        if let Ok(entries) = std::fs::read_dir(&pack) {
            for entry in entries.flatten() {
                age_file(&entry.path(), by);
            }
        }
        age_directory(path, by);
    }

    /// The whole point of the sweep, stated per area rather than in aggregate:
    /// a spool that outlived its request goes, and — the half without which
    /// this proves nothing — a spool that may still belong to one stays. A
    /// sweep that deleted everything it found would pass the first assertion
    /// and destroy a live 512 MiB upload.
    #[tokio::test]
    async fn every_area_loses_its_stale_spools_and_keeps_its_fresh_ones() {
        let root = tempfile::tempdir().expect("repo root");

        for area in StagingArea::ALL {
            let directory = area.path_in(root.path());
            std::fs::create_dir_all(&directory).expect("staging directory");
            let stale = directory.join("abandoned.upload");
            std::fs::write(&stale, b"left behind by a SIGKILL").expect("stale spool");
            age_file(&stale, STALE_SPOOL_AGE + Duration::from_secs(60));
            std::fs::write(directory.join("in-flight.upload"), b"a live request")
                .expect("fresh spool");
        }

        let report = sweep_stale_spools(root.path(), STALE_SPOOL_AGE).await;

        assert_eq!(
            report,
            SweepReport {
                removed: StagingArea::ALL.len(),
                retained: StagingArea::ALL.len(),
                failed: 0,
            },
            "one stale spool per area had to go and one fresh spool per area had to stay"
        );
        for area in StagingArea::ALL {
            let directory = area.path_in(root.path());
            assert!(
                !directory.join("abandoned.upload").exists(),
                "{} still holds the spool of a request that ended with the process",
                directory.display()
            );
            assert!(
                directory.join("in-flight.upload").exists(),
                "{} lost a spool young enough to belong to a live request",
                directory.display()
            );
        }
    }

    /// A first boot, and every boot on an instance that has never used one of
    /// the four areas: the directories do not exist, and that is not a failure
    /// anybody should have to read a warning about.
    #[tokio::test]
    async fn a_repo_root_without_any_staging_directory_sweeps_clean() {
        let root = tempfile::tempdir().expect("repo root");

        let report = sweep_stale_spools(root.path(), STALE_SPOOL_AGE).await;

        assert_eq!(report, SweepReport::default());
    }

    /// Nothing in these directories creates a symlink, so one is something the
    /// sweep does not understand. It is left alone rather than followed —
    /// following it would delete whatever it points at, outside `.tmp`
    /// entirely.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_symlink_is_neither_followed_nor_removed() {
        let root = tempfile::tempdir().expect("repo root");
        let outside = root.path().join("precious.db");
        std::fs::write(&outside, b"not a spool").expect("target");
        age_file(&outside, STALE_SPOOL_AGE + Duration::from_secs(60));

        let directory = StagingArea::Attachments.path_in(root.path());
        std::fs::create_dir_all(&directory).expect("staging directory");
        let link = directory.join("pointer.upload");
        std::os::unix::fs::symlink(&outside, &link).expect("symlink");

        let report = sweep_stale_spools(root.path(), STALE_SPOOL_AGE).await;

        assert_eq!(report, SweepReport::default(), "a symlink is not a spool");
        assert!(outside.exists(), "the sweep followed a symlink out of .tmp");
        assert!(
            link.symlink_metadata().is_ok(),
            "the sweep removed a symlink"
        );
    }

    /// Every production `.tmp` path in the workspace has to come out of
    /// [`StagingArea::path_in`], because [`StagingArea::ALL`] is what the
    /// startup sweep iterates: a fifth staging directory spelled directly in a
    /// handler is a directory nothing ever reads again.
    ///
    /// The census is a REPORTING sweep, so it reads the production view of each
    /// file — `#[cfg(test)]` items blanked, comments and literals kept — locates
    /// `join(` in the code-only twin, and decodes the argument literal out of
    /// the string-bearing one at the same byte offset. A commented-out or
    /// test-only `join(".tmp")` therefore contributes nothing, and neither does
    /// `strip_suffix(".tmp")`, which is a suffix test rather than a path built
    /// under the staging root.
    #[test]
    fn staging_registry_is_the_only_producer_of_tmp_paths() {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let home = workspace
            .join("crates")
            .join("rg-core")
            .join("src")
            .join("staging.rs");

        let mut here = 0_usize;
        let mut elsewhere = Vec::new();
        for file in production_rust_files(&workspace.join("crates")) {
            let text = std::fs::read_to_string(&file).expect("read a workspace source file");
            for line in staging_root_joins(&text) {
                if file == home {
                    here += 1;
                } else {
                    elsewhere.push(format!("{}:{line}", file.display()));
                }
            }
        }

        assert!(
            here > 0,
            "no production `join(\".tmp\"…)` was found in {} — the census went blind and \
             would now stay green over any new staging directory",
            home.display()
        );
        assert!(
            elsewhere.is_empty(),
            "these build a `.tmp` path without going through `StagingArea::path_in`, so \
             `sweep_stale_spools` will never read what they leave behind: {}",
            elsewhere.join(", ")
        );
    }

    /// 1-based lines of the production `join(…)` calls whose argument literal
    /// names the staging root.
    ///
    /// Split out of the census loop so the census reads one file's bytes
    /// through one named view, rather than deciding what a `.tmp` path is
    /// inline over a directory walk.
    fn staging_root_joins(text: &str) -> Vec<usize> {
        let source = rust_source::production_rust_source(text);
        rust_source::production_call_sites(text, &["join"])
            .into_iter()
            .filter(|call| {
                literal_argument(&source, call.open_paren)
                    .is_some_and(|value| value == ".tmp" || value.starts_with(".tmp/"))
            })
            .map(|call| call.line)
            .collect()
    }

    /// The string literal opening a call's argument list, or `None` when the
    /// first argument is not one (a variable, a `format!`, a constant).
    fn literal_argument(source: &str, open_paren: usize) -> Option<String> {
        let rest = source.get(open_paren + 1..)?.trim_start();
        let body = rest.strip_prefix('"')?;
        let end = body.find('"')?;
        // An escape would make the bytes up to the first quote the wrong value;
        // no staging path needs one, so decline rather than guess.
        let value = &body[..end];
        if value.contains('\\') {
            return None;
        }
        Some(value.to_owned())
    }

    /// One CI cache spool name as `tempfile::Builder` actually renders it, so
    /// the matcher is tested against the producer's output rather than against
    /// a guess at its shape.
    fn cache_spool_in(directory: &std::path::Path) -> std::path::PathBuf {
        let staged = tempfile::Builder::new()
            .prefix(CI_CACHE_SPOOL_PREFIX)
            .suffix(CI_CACHE_SPOOL_SUFFIX)
            .tempfile_in(directory)
            .expect("cache spool");
        staged
            .into_temp_path()
            .keep()
            .expect("keep the cache spool")
    }

    /// Every namer's output is recognised by its own matcher, and by no other.
    ///
    /// This is the join that makes the registry mean anything: the producers
    /// build their names through these functions, so a producer that changes
    /// the shape of its spool name either changes the matcher with it or fails
    /// here.
    #[test]
    fn every_namer_round_trips_through_its_own_matcher() {
        let oid = "a".repeat(64);
        let write_id = uuid::Uuid::new_v4();
        let directory = tempfile::tempdir().expect("cache directory");
        let cache = cache_spool_in(directory.path());
        let cache = cache
            .file_name()
            .and_then(|name| name.to_str())
            .expect("cache spool name")
            .to_owned();

        let named = [
            (SiblingSpool::LfsObject, lfs_object_spool_name(&oid)),
            (SiblingSpool::CiCacheArchive, cache),
            (
                SiblingSpool::BlobWrite,
                blob_write_spool_name("pino-9.13.1.tgz", write_id),
            ),
            (
                SiblingSpool::AuditArchive,
                audit_archive_spool_name(write_id),
            ),
            (
                SiblingSpool::AttachmentBackup,
                attachment_backup_spool_name(write_id),
            ),
        ];

        for (family, name) in &named {
            assert!(
                family.matches(name),
                "{family:?} does not recognise the name it produces: {name}"
            );
            assert!(is_sibling_spool(name), "{name} is not swept by anything");
            for other in SiblingSpool::ALL {
                if other != family {
                    assert!(
                        !other.matches(name),
                        "{other:?} also claims {family:?}'s spool {name}"
                    );
                }
            }
        }
        assert_eq!(
            named.len(),
            SiblingSpool::ALL.len(),
            "a spool family was registered without a namer to round-trip it"
        );
    }

    /// The half that decides whether the sweep reclaims disk or destroys data:
    /// a name whose variable part does not parse back is not ours.
    ///
    /// Each of these is a real neighbour of a real spool — a published LFS
    /// object, a published cache archive, the blob a write was publishing, a
    /// finished audit archive, and the database backup temp file, which lives
    /// in a directory an operator may well point at `repo_root`.
    #[test]
    fn a_name_that_does_not_parse_back_is_not_a_spool() {
        for innocent in [
            // A published LFS object: the oid without the spool prefix.
            &"b".repeat(64),
            // The prefix with something that is not an oid behind it.
            ".tmp_not-an-oid",
            &format!(".tmp_{}", "c".repeat(63)),
            // Uppercase is not LFS oid alphabet.
            &format!(".tmp_{}", "A".repeat(64)),
            // A published cache archive, and the spool shape without its parts.
            "e3b0c44298fc1c14.9.tar",
            "cache-.upload",
            "cache-not alnum.upload",
            "cache-abc123.tar",
            // A blob whose name merely looks temporary.
            ".gitignore",
            ".config.tmp",
            ".pkg.not-a-uuid.tmp",
            "pkg.00000000-0000-0000-0000-000000000000.tmp",
            // The database backup's own temp file, which is `backup`'s to
            // rotate and must not be taken by this sweep.
            ".forgekeep-backup-00000000-0000-0000-0000-000000000000.tmp",
            // A finished audit archive, and the spool shape without a real id.
            "audit-20260908T101500-00000000-0000-0000-0000-000000000000.ndjson.zst",
            ".audit-.tmp",
            ".audit-1234.tmp",
            // The attachment rollback copy's shape without a real id, and an
            // attachment a user uploaded under a name that imitates one — the
            // leading dot is not decoration, it is half of what makes the name
            // ours rather than a stored object's.
            ".attachment-backup-.tmp",
            ".attachment-backup-1234.tmp",
            "attachment-backup-00000000-0000-0000-0000-000000000000.tmp",
        ] {
            assert!(
                !is_sibling_spool(innocent),
                "the sweep would have deleted {innocent}"
            );
        }
    }

    /// The sibling families, each in the directory it is really written to, and
    /// each with the second half that proves the sweep is not simply deleting
    /// everything it walks past: a fresh spool of the same shape stays, and so
    /// does the live object it was going to become.
    #[tokio::test]
    async fn sibling_spools_are_swept_where_their_producers_write_them() {
        let root = tempfile::tempdir().expect("repo root");
        let root = root.path();

        // LFS: `<repo_root>/<owner>.lfs/<repo>/`, beside the objects.
        let lfs = root.join("octocat.lfs").join("payloads");
        std::fs::create_dir_all(&lfs).expect("lfs root");
        let stale_lfs = lfs.join(lfs_object_spool_name(&"a".repeat(64)));
        let fresh_lfs = lfs.join(lfs_object_spool_name(&"b".repeat(64)));
        let live_object = lfs.join("c".repeat(64));

        // CI cache: `<repo_root>/_ci_cache/<repo_id>/`.
        let cache = root.join("_ci_cache").join("42");
        std::fs::create_dir_all(&cache).expect("cache directory");
        let stale_cache = cache_spool_in(&cache);
        let fresh_cache = cache_spool_in(&cache);
        let live_archive = cache.join("e3b0c442.9.tar");

        // Blob write: beside whatever key the local backend was publishing, at
        // whatever depth that key has.
        let blobs = root.join("packages").join("octocat").join("payloads");
        std::fs::create_dir_all(&blobs).expect("blob directory");
        let stale_blob = blobs.join(blob_write_spool_name("pino.tgz", uuid::Uuid::new_v4()));
        let fresh_blob = blobs.join(blob_write_spool_name("pino.tgz", uuid::Uuid::new_v4()));
        let live_blob = blobs.join("pino.tgz");

        // Audit archive: its own configured directory, swept through the same
        // entry point by `audit::archiver`.
        let archives = root.join("audit-archive");
        std::fs::create_dir_all(&archives).expect("archive directory");
        let stale_audit = archives.join(audit_archive_spool_name(uuid::Uuid::new_v4()));
        let fresh_audit = archives.join(audit_archive_spool_name(uuid::Uuid::new_v4()));
        let live_audit = archives.join("audit-20260908T101500-x.ndjson.zst");

        // Attachment rollback copy: beside the attachment blob it protects,
        // which is where it went instead of the system temp directory.
        let attachments = root.join("attachments").join("7").join("evidence");
        std::fs::create_dir_all(&attachments).expect("attachment directory");
        let stale_backup = attachments.join(attachment_backup_spool_name(uuid::Uuid::new_v4()));
        let fresh_backup = attachments.join(attachment_backup_spool_name(uuid::Uuid::new_v4()));
        let live_attachment = attachments.join("evidence.txt");

        let stale = [
            &stale_lfs,
            &stale_cache,
            &stale_blob,
            &stale_audit,
            &stale_backup,
        ];
        let fresh = [
            &fresh_lfs,
            &fresh_cache,
            &fresh_blob,
            &fresh_audit,
            &fresh_backup,
        ];
        let live = [
            &live_object,
            &live_archive,
            &live_blob,
            &live_audit,
            &live_attachment,
        ];
        for path in stale.iter().chain(fresh.iter()).chain(live.iter()) {
            if !path.exists() {
                std::fs::write(path, b"bytes").expect("write the fixture");
            }
        }
        for path in stale.iter().chain(live.iter()) {
            age_file(path, STALE_SPOOL_AGE + Duration::from_secs(60));
        }

        let report = sweep_stale_spools(root, STALE_SPOOL_AGE).await;

        assert_eq!(
            report,
            SweepReport {
                removed: stale.len(),
                retained: fresh.len(),
                failed: 0,
            },
            "one stale spool per family had to go and one fresh spool per family had to stay"
        );
        for path in stale {
            assert!(
                !path.exists(),
                "{} outlived a stop that ran no destructors",
                path.display()
            );
        }
        for path in fresh.iter().chain(live.iter()) {
            assert!(
                path.exists(),
                "{} was deleted, and it may still belong to a live request",
                path.display()
            );
        }
    }

    /// A bare repository is not descended into. Its loose-object directories are
    /// the reason: they are the only tree under the storage root big enough to
    /// make a startup walk cost minutes, and no spool of any family is inside
    /// one.
    ///
    /// The skip has to rest on more than the name, so this also asserts the
    /// other half — a directory that merely *ends* in `.git` and holds no
    /// `HEAD` is a blob directory, and its spools are still swept.
    #[tokio::test]
    async fn bare_repositories_are_skipped_and_look_alikes_are_not() {
        let root = tempfile::tempdir().expect("repo root");
        let root = root.path();

        let bare = root.join("octocat").join("payloads.git");
        std::fs::create_dir_all(bare.join("objects")).expect("git objects");
        std::fs::write(bare.join("HEAD"), b"ref: refs/heads/main\n").expect("HEAD");
        let inside_git = bare
            .join("objects")
            .join(blob_write_spool_name("pack", uuid::Uuid::new_v4()));
        std::fs::write(&inside_git, b"git's own business").expect("write");
        age_file(&inside_git, STALE_SPOOL_AGE + Duration::from_secs(60));

        let look_alike = root.join("packages").join("octocat").join("payloads.git");
        std::fs::create_dir_all(&look_alike).expect("blob directory");
        let inside_blobs = look_alike.join(blob_write_spool_name("bundle", uuid::Uuid::new_v4()));
        std::fs::write(&inside_blobs, b"a leaked blob spool").expect("write");
        age_file(&inside_blobs, STALE_SPOOL_AGE + Duration::from_secs(60));

        let report = sweep_stale_spools(root, STALE_SPOOL_AGE).await;

        assert_eq!(
            report,
            SweepReport {
                removed: 1,
                retained: 0,
                failed: 0
            }
        );
        assert!(
            inside_git.exists(),
            "the sweep walked into a bare repository"
        );
        assert!(
            !inside_blobs.exists(),
            "a directory was skipped on its name alone, so the blob spool under it survived"
        );
    }

    /// Every tree namer's output is recognised by its own matcher and by no
    /// other — the same join as `every_namer_round_trips_through_its_own_matcher`
    /// makes for the families whose spool is a file.
    #[test]
    fn every_staging_tree_namer_round_trips_through_its_own_matcher() {
        let token = uuid::Uuid::new_v4();
        let named = [
            (
                SiblingSpoolTree::ImportClone,
                import_clone_staging_name("payloads", token),
            ),
            (
                SiblingSpoolTree::ImportWikiClone,
                import_wiki_clone_staging_name("payloads", token),
            ),
        ];

        for (family, name) in &named {
            assert!(
                family.matches(name),
                "{family:?} does not recognise the name it produces: {name}"
            );
            assert!(
                is_sibling_spool_tree(name),
                "{name} is not swept by anything"
            );
            for other in SiblingSpoolTree::ALL {
                if other != family {
                    assert!(
                        !other.matches(name),
                        "{other:?} also claims {family:?}'s staged tree {name}"
                    );
                }
            }
        }
        assert_eq!(
            named.len(),
            SiblingSpoolTree::ALL.len(),
            "a staged tree family was registered without a namer to round-trip it"
        );

        // A repository name may hold dots of its own; the token is taken from
        // the right, so `pino.js` stages a clone like any other name.
        assert!(is_sibling_spool_tree(&import_clone_staging_name(
            "pino.js", token
        )));
    }

    /// The half that decides whether the sweep reclaims disk or destroys a
    /// repository: a name whose variable part does not parse back is not ours.
    ///
    /// What a match authorises here is `remove_dir_all` on a directory sitting
    /// among live bare repositories, so each of these is a real neighbour — the
    /// repository the import is aiming at, the skeleton the journal owns, and
    /// the shapes that merely resemble a staged clone.
    #[test]
    fn a_tree_name_that_does_not_parse_back_is_not_a_staged_tree() {
        let token = uuid::Uuid::new_v4().simple().to_string();
        for innocent in [
            // The live repository the clone is going to be moved onto.
            "payloads.git".to_string(),
            // The skeleton an install moved aside. The deletion-recovery
            // journal owns it and its pass runs *after* this sweep, so a match
            // here would delete bytes that pass is about to put back.
            format!(".payloads.git.replaced-{token}"),
            // The shape with nothing where the token goes, and with something
            // that is not one.
            ".payloads.git.importing-".to_string(),
            ".payloads.git.importing-not-a-uuid".to_string(),
            // No repository name in front of the suffix.
            format!(".git.importing-{token}"),
            // The leading dot is not decoration: it is half of what makes the
            // name ours rather than a directory somebody else created.
            format!("payloads.git.importing-{token}"),
            // A bare repository whose own name contains the fragment.
            format!(".payloads.importing-{token}.git"),
        ] {
            assert!(
                !is_sibling_spool_tree(&innocent),
                "the sweep would have deleted {innocent}"
            );
        }
    }

    /// The whole point of retiring a tree: a clone an import was killed in the
    /// middle of is the size of the upstream, and nothing else will ever name
    /// those bytes again.
    ///
    /// Each assertion has its second half, because a pass that removed every
    /// directory it recognised would satisfy the first: a fresh clone belongs to
    /// an import running in another process right now, and the repository the
    /// import is aiming at sits beside it under a name one character different.
    ///
    /// The last pair is what proves the sweep does not *enter* a staged clone.
    /// A spool of a family it does sweep is planted inside the fresh one, aged
    /// well past the bound: descending would delete it and count it, so its
    /// survival — and the count staying at two — is the measurement, not an
    /// impression of one.
    #[tokio::test]
    async fn partial_import_clones_are_retired_whole_and_fresh_ones_are_kept() {
        let root = tempfile::tempdir().expect("repo root");
        let root = root.path();
        let owner = root.join("octocat");
        std::fs::create_dir_all(&owner).expect("owner directory");

        let stale = owner.join(import_clone_staging_name("payloads", uuid::Uuid::new_v4()));
        let stale_wiki = owner.join(import_wiki_clone_staging_name(
            "payloads",
            uuid::Uuid::new_v4(),
        ));
        let fresh = owner.join(import_clone_staging_name("arrivals", uuid::Uuid::new_v4()));
        // The repository the first clone is going to be installed onto, with
        // history of its own.
        let live = owner.join("payloads.git");
        for tree in [&stale, &stale_wiki, &fresh, &live] {
            write_partial_clone(tree);
        }

        let canary = fresh
            .join("objects")
            .join(blob_write_spool_name("pack", uuid::Uuid::new_v4()));
        std::fs::write(&canary, b"not the sweep's business").expect("canary");
        age_file(&canary, STALE_SPOOL_AGE + Duration::from_secs(60));

        for tree in [&stale, &stale_wiki, &live] {
            age_tree(tree, STALE_SPOOL_AGE + Duration::from_secs(60));
        }

        let report = sweep_stale_spools(root, STALE_SPOOL_AGE).await;

        assert_eq!(
            report,
            SweepReport {
                removed: 2,
                retained: 1,
                failed: 0,
            },
            "the two abandoned clones had to go whole, the live one had to stay, and nothing \
             inside either was the sweep's to touch"
        );
        for tree in [&stale, &stale_wiki] {
            assert!(
                !tree.exists(),
                "{} outlived the import that was killed writing it",
                tree.display()
            );
        }
        assert!(
            fresh.exists(),
            "a clone young enough to belong to a live import was deleted"
        );
        assert!(
            canary.exists(),
            "the sweep walked into a staged clone and deleted what it found there"
        );
        assert!(
            live.join("HEAD").exists(),
            "the sweep removed the repository the import was aiming at"
        );
    }

    /// A clone that is old at the top and being written to underneath is a
    /// *running* clone, not an abandoned one.
    ///
    /// `git clone --bare` writes the skeleton in its first moments and then
    /// spends the whole transfer streaming a pack into `objects/pack/`, so the
    /// directory's own mtime dates the clone from when it started. On a large
    /// upstream over a slow link that number passes the bound while the
    /// transfer is still running, and the sweep of a second process would
    /// delete the clone out from under it.
    #[tokio::test]
    async fn a_clone_still_receiving_its_pack_is_not_retired_on_the_age_of_its_top() {
        let root = tempfile::tempdir().expect("repo root");
        let root = root.path();
        let owner = root.join("octocat");
        std::fs::create_dir_all(&owner).expect("owner directory");

        let running = owner.join(import_clone_staging_name("payloads", uuid::Uuid::new_v4()));
        write_partial_clone(&running);
        age_tree(&running, STALE_SPOOL_AGE + Duration::from_secs(60));
        // The pack this clone is still receiving, written a moment ago.
        std::fs::write(
            running.join("objects").join("pack").join("tmp_pack_live"),
            b"the byte that just arrived",
        )
        .expect("incoming pack");

        let report = sweep_stale_spools(root, STALE_SPOOL_AGE).await;

        assert_eq!(
            report,
            SweepReport {
                removed: 0,
                retained: 1,
                failed: 0,
            },
            "a clone whose pack is still growing was dated by its top instead"
        );
        assert!(
            running.exists(),
            "the sweep deleted a clone that another process was still writing"
        );
    }

    /// Every production spool name of a sibling family has to be built by this
    /// module, because [`SiblingSpool::matches`] is the only thing that will
    /// ever recognise one again. A producer that spells its own is a file
    /// nothing sweeps — the exact defect this module was extended to close.
    ///
    /// Structured like its neighbour `staging_root_joins`, and for the same
    /// reason: the call is located in the *code-only* view, where a comment and
    /// a call-shaped string literal contribute nothing and a `#[cfg(test)]`
    /// fixture cannot hold the floor green, and only then is the argument
    /// decoded out of the string-bearing view at that byte offset.
    ///
    /// What it does NOT hold, said plainly: `cache-` reaches its producer as a
    /// constant handed to `tempfile::Builder`, never as a literal in a call
    /// this census can see, so that family's namer and matcher are held
    /// together by `every_namer_round_trips_through_its_own_matcher` alone.
    #[test]
    fn staging_is_the_only_producer_of_sibling_spool_names() {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..");
        let home = workspace
            .join("crates")
            .join("rg-core")
            .join("src")
            .join("staging.rs");

        let mut here = std::collections::BTreeSet::new();
        let mut elsewhere = Vec::new();
        for file in production_rust_files(&workspace.join("crates")) {
            let text = std::fs::read_to_string(&file).expect("read a workspace source file");
            for (line, fragment) in spool_name_literals(&text) {
                if file == home {
                    here.insert(fragment);
                } else {
                    elsewhere.push(format!("{}:{line} ({fragment})", file.display()));
                }
            }
        }

        assert!(
            here.contains(".tmp_")
                && here.contains(".audit-")
                && here.contains(".attachment-backup-")
                && here.contains(".importing-"),
            "the census found {here:?} in {} — it has gone blind on a family it is supposed to \
             hold, and would now stay green over a producer spelling that name itself",
            home.display()
        );
        assert!(
            elsewhere.is_empty(),
            "these spell a sibling-spool name without going through `rg_core::staging`, so \
             `sweep_stale_sibling_spools` will never recognise what they leave behind: {}",
            elsewhere.join(", ")
        );
    }

    /// 1-based lines of the production calls whose first argument literal names
    /// a sibling spool, paired with the fragment that identified it.
    ///
    /// The call names are the shapes a spool name is actually built or taken
    /// apart with. `.upload` is deliberately not among the fragments: it is the
    /// CI cache spool's suffix, but `releases` and `packages` give their spools
    /// under `.tmp/` the same one, and those are [`StagingArea`]'s to sweep.
    /// `cache-` is the half that distinguishes the family.
    fn spool_name_literals(text: &str) -> Vec<(usize, &'static str)> {
        const FRAGMENTS: &[&str] = &[".tmp_", ".audit-", "cache-", ".attachment-backup-"];
        // A name whose variable part comes *first* cannot be recognised by
        // where it starts: a staged import clone is `.<repo>.git.importing-…`,
        // so the fragment that identifies the family sits in the middle. Kept
        // as a second list rather than relaxing the first to `contains`,
        // because `cache-` at the start of a literal is a spool while
        // `cache-` inside one is an ordinary word.
        const INFIX_FRAGMENTS: &[&str] = &[".importing-"];
        let source = rust_source::production_rust_source(text);
        rust_source::production_call_sites(
            text,
            &[
                "format!",
                "prefix",
                "suffix",
                "strip_prefix",
                "strip_suffix",
            ],
        )
        .into_iter()
        .filter_map(|call| {
            let value = literal_argument(&source, call.open_paren)?;
            let fragment = FRAGMENTS
                .iter()
                .find(|fragment| value.starts_with(**fragment))
                .or_else(|| {
                    INFIX_FRAGMENTS
                        .iter()
                        .find(|fragment| value.contains(**fragment))
                })?;
            Some((call.line, *fragment))
        })
        .collect()
    }

    fn production_rust_files(root: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut pending = vec![root.to_path_buf()];
        let mut files = Vec::new();
        while let Some(directory) = pending.pop() {
            let entries = match std::fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) => panic!("read {}: {error}", directory.display()),
            };
            for entry in entries {
                let path = entry.expect("directory entry").path();
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                if path.is_dir() {
                    // `src/` is the shipped half of every crate; `tests/`,
                    // `target/` and fixtures are not what the sweep has to
                    // reach.
                    if name != "target" && name != "tests" && name != "benches" {
                        pending.push(path);
                    }
                } else if path.extension().is_some_and(|ext| ext == "rs") {
                    files.push(path);
                }
            }
        }
        files.sort();
        files
    }
}
