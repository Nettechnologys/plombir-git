//! The receipt of an import: what the source has, what the target has, and —
//! by name — what did not come over (card_29def7a1e172).
//!
//! The counters of [`ImportStats`] say what this import *wrote*. A receipt is a
//! different statement: the source's own listing beside what the target holds
//! once the import is done, read back from the database and from the cloned
//! repository rather than from the writer's tally. A label the target already
//! had counts as present; an issue the writer reported but the table does not
//! hold does not. Someone moving two hundred repositories reads this table, not
//! the log, to know nothing was lost.
//!
//! What cannot be transferred at all — secret values, reactions, branch
//! protection — is named as a checklist, so that its absence is a known item
//! rather than a surprise a month later. So is CI the imported default branch
//! defines in a form this server does not run (`.gitlab-ci.yml`), which is the
//! loss a moved project otherwise discovers only when no pipeline ever starts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use sea_orm::DatabaseConnection;

use super::service::ImportStats;

/// How many names a list of missing objects or refs keeps. The receipt is
/// stored in `import_tasks.stats` beside the failure lists, which share one
/// size budget; past this many the count says how many more there are.
pub const RECEIPT_NAME_SAMPLE: usize = 10;

/// Characters a name in a receipt list keeps.
const RECEIPT_NAME_CHARS: usize = 128;

/// What the source listed, gathered by the platform runners as they read it.
/// `None` for a kind this import was not asked to bring.
#[derive(Debug, Default, Clone)]
pub struct SourceInventory {
    pub(crate) repo_id: Option<i64>,
    pub(crate) labels: Option<Vec<String>>,
    pub(crate) milestones: Option<Vec<String>>,
    pub(crate) issues: Option<Vec<i64>>,
    pub(crate) pull_requests: Option<Vec<i64>>,
    /// Conversation comments of issues and pull requests (GitHub), or notes
    /// (GitLab, system notes included — they are counted as skipped).
    pub(crate) comments: Option<usize>,
    /// Submitted reviews; an unsubmitted draft is not one.
    pub(crate) reviews: Option<usize>,
    /// Comments on lines of a pull request's diff, drafts left out.
    pub(crate) review_comments: Option<usize>,
    pub(crate) releases: Option<Vec<String>>,
    /// `git ls-remote --heads --tags` of the source, taken with the clone, or
    /// why there is none.
    pub(crate) refs: Option<std::result::Result<String, String>>,
}

/// The receipt an import task's API answer carries as `receipt`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportReceipt {
    /// One row per kind of object the import was asked to bring.
    pub objects: Vec<ObjectReceipt>,
    /// Branches and tags, by name and SHA.
    pub refs: RefsReceipt,
    /// What no import can bring, to be set up by hand.
    pub not_transferable: Vec<&'static str>,
    /// CI the imported default branch defines and this server will not run.
    /// Absent when nothing was cloned, so there is no branch to look at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ci: Option<CiReceipt>,
}

/// The CI of the imported default branch, as far as it will not run here.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CiReceipt {
    /// Why the branch could not be looked at, when it could not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    /// Each CI definition of the branch that will not run here, by path.
    /// Empty when the branch has none, or has one this server runs.
    pub not_run: Vec<CiNotRun>,
}

/// One CI definition that will not run here, and what to do instead.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CiNotRun {
    pub path: &'static str,
    pub reason: &'static str,
}

/// CI definitions of other forges that this server does not read, with what to
/// write instead. Named in the receipt only when the branch has none of the
/// sources the engine does read: a `.gitlab-ci.yml` kept beside a workflow is a
/// choice, not a loss.
const FOREIGN_CI: [(&str, &str); 1] = [(
    ".gitlab-ci.yml",
    "GitLab CI is not run here: describe the pipeline as an Actions workflow under \
     `.forgejo/workflows/`, `.gitea/workflows/` or `.github/workflows/`, or in \
     `.plombir-git-ci.yml`",
)];

/// One kind of object: how many the source has, how many are here.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ObjectReceipt {
    pub kind: &'static str,
    /// What the source listed; `None` where it has no listing to compare with.
    pub source: Option<usize>,
    /// What the target holds of it.
    pub imported: usize,
    /// Left out on purpose, with the reason in [`Self::skipped_reason`].
    #[serde(skip_serializing_if = "is_zero")]
    pub skipped: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<&'static str>,
    /// What the source has and the target does not.
    pub not_imported: usize,
    /// The first [`RECEIPT_NAME_SAMPLE`] of them by name.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub missing_omitted: usize,
}

/// Branches and tags of the source beside the target's.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RefsReceipt {
    /// Why the source's refs could not be compared, when they could not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branches: Option<RefKindReceipt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tags: Option<RefKindReceipt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<DefaultBranchReceipt>,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RefKindReceipt {
    pub source: usize,
    /// Present here at the SHA the source has.
    pub imported: usize,
    /// Absent here, by name.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub missing_omitted: usize,
    /// Present here at another SHA, by name.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub different: Vec<String>,
    #[serde(skip_serializing_if = "is_zero")]
    pub different_omitted: usize,
}

/// The default branch, compared by the commit it points at: the same SHA is the
/// same history, every commit of it, which no count can say as well.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DefaultBranchReceipt {
    pub name: String,
    pub source_sha: Option<String>,
    pub imported_sha: Option<String>,
    /// Commits reachable from the imported head.
    pub commits: Option<usize>,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// Up to [`RECEIPT_NAME_SAMPLE`] names, each cut to [`RECEIPT_NAME_CHARS`],
/// and how many were left out.
fn sample(names: impl IntoIterator<Item = String>) -> (Vec<String>, usize) {
    let mut kept = Vec::new();
    let mut omitted = 0;
    for name in names {
        if kept.len() < RECEIPT_NAME_SAMPLE {
            kept.push(if name.chars().count() > RECEIPT_NAME_CHARS {
                let mut short: String = name.chars().take(RECEIPT_NAME_CHARS).collect();
                short.push('…');
                short
            } else {
                name
            });
        } else {
            omitted += 1;
        }
    }
    (kept, omitted)
}

/// A kind the source lists by name: present is what both sides have.
fn named(
    kind: &'static str,
    source: &[String],
    target: &BTreeSet<String>,
    render: impl Fn(&str) -> String,
) -> ObjectReceipt {
    let listed: BTreeSet<&String> = source.iter().collect();
    let missing = listed
        .iter()
        .filter(|name| !target.contains(name.as_str()))
        .map(|name| render(name))
        .collect::<Vec<_>>();
    let not_imported = missing.len();
    let (missing, missing_omitted) = sample(missing);
    ObjectReceipt {
        kind,
        source: Some(listed.len()),
        imported: listed.len() - not_imported,
        skipped: 0,
        skipped_reason: None,
        not_imported,
        missing,
        missing_omitted,
    }
}

/// A kind only counted at the source: what is here is what the import wrote,
/// and what is missing is what it named as failed.
fn counted(
    kind: &'static str,
    source: Option<usize>,
    imported: usize,
    failed: impl IntoIterator<Item = String>,
) -> ObjectReceipt {
    let failed = failed.into_iter().collect::<Vec<_>>();
    let not_imported = match source {
        Some(source) => source.saturating_sub(imported).max(failed.len()),
        None => failed.len(),
    };
    let (missing, missing_omitted) = sample(failed);
    ObjectReceipt {
        kind,
        source,
        imported,
        skipped: 0,
        skipped_reason: None,
        not_imported,
        missing,
        missing_omitted,
    }
}

/// Compare what `ls-remote` advertised with the refs `repository` holds.
fn refs_receipt(listing: &str, repository: &Path) -> anyhow::Result<RefsReceipt> {
    let mut source: BTreeMap<String, String> = BTreeMap::new();
    for line in listing.lines() {
        let Some((sha, name)) = line.split_once('\t') else {
            continue;
        };
        // A peeled annotated tag: the tag itself is the line without `^{}`.
        if name.ends_with("^{}") {
            continue;
        }
        source.insert(name.trim().to_owned(), sha.trim().to_owned());
    }

    let repo = rg_git::repository::open(repository)
        .map_err(|error| anyhow::anyhow!("failed to open the imported repository: {error}"))?;
    let mut target: BTreeMap<String, String> = BTreeMap::new();
    let references = repo
        .references()
        .map_err(|error| anyhow::anyhow!("failed to list the imported refs: {error}"))?;
    for prefix in ["refs/heads/", "refs/tags/"] {
        for reference in references
            .prefixed(prefix)
            .map_err(|error| anyhow::anyhow!("failed to list {prefix}: {error}"))?
        {
            let reference =
                reference.map_err(|error| anyhow::anyhow!("failed to read a ref: {error}"))?;
            if let Some(id) = reference.target().try_id() {
                target.insert(reference.name().as_bstr().to_string(), id.to_string());
            }
        }
    }

    let kind = |prefix: &str| {
        let mut receipt = RefKindReceipt::default();
        let mut missing = Vec::new();
        let mut different = Vec::new();
        for (name, sha) in source.iter().filter(|(name, _)| name.starts_with(prefix)) {
            receipt.source += 1;
            let short = name.trim_start_matches(prefix).to_owned();
            match target.get(name) {
                Some(here) if here == sha => receipt.imported += 1,
                Some(_) => different.push(short),
                None => missing.push(short),
            }
        }
        (receipt.missing, receipt.missing_omitted) = sample(missing);
        (receipt.different, receipt.different_omitted) = sample(different);
        receipt
    };

    let default_branch = repo
        .head_name()
        .ok()
        .flatten()
        .map(|head| head.as_bstr().to_string())
        .filter(|head| head.starts_with("refs/heads/"))
        .map(|head| {
            let imported_sha = target.get(&head).cloned();
            let commits = imported_sha.as_deref().and_then(|sha| {
                let id = gix::ObjectId::from_hex(sha.as_bytes()).ok()?;
                Some(repo.rev_walk([id]).all().ok()?.count())
            });
            DefaultBranchReceipt {
                name: head.trim_start_matches("refs/heads/").to_owned(),
                source_sha: source.get(&head).cloned(),
                imported_sha,
                commits,
            }
        });

    Ok(RefsReceipt {
        unavailable: None,
        branches: Some(kind("refs/heads/")),
        tags: Some(kind("refs/tags/")),
        default_branch,
    })
}

/// The CI definitions at the head of `repository`'s default branch that will
/// not run here — see [`FOREIGN_CI`]. An unborn branch has none.
fn ci_not_run(repository: &Path) -> anyhow::Result<Vec<CiNotRun>> {
    let repo = rg_git::repository::open(repository)
        .map_err(|error| anyhow::anyhow!("failed to open the imported repository: {error}"))?;
    let head = repo
        .head()
        .map_err(|error| anyhow::anyhow!("failed to read the imported HEAD: {error}"))?;
    if head.is_unborn() {
        return Ok(Vec::new());
    }
    let tree = repo
        .head_commit()
        .map_err(|error| anyhow::anyhow!("failed to read the imported head commit: {error}"))?
        .tree()
        .map_err(|error| anyhow::anyhow!("failed to read the imported head tree: {error}"))?;
    let present = |path: &str| {
        tree.lookup_entry_by_path(path)
            .map(|entry| entry.is_some())
            .map_err(|error| anyhow::anyhow!("failed to look up {path}: {error}"))
    };

    for path in crate::ci::WORKFLOW_DIRS
        .iter()
        .chain(crate::ci::NATIVE_CI_FILES.iter())
    {
        if present(path)? {
            return Ok(Vec::new());
        }
    }
    let mut not_run = Vec::new();
    for (path, reason) in FOREIGN_CI {
        if present(path)? {
            not_run.push(CiNotRun { path, reason });
        }
    }
    Ok(not_run)
}

/// The checklist of what no import brings, for `platform`.
fn not_transferable(platform: &str) -> Vec<&'static str> {
    match platform {
        "git" => vec![
            "Issues, pull requests, releases and the wiki: a plain Git source has none to give",
            "Branch protection rules: set them under the repository's settings",
            "Webhooks and deploy keys: register them again",
        ],
        _ => vec![
            "CI secret values: the source's API never returns them; add each under the \
             repository's CI settings",
            "Branch protection rules: set them under the repository's settings",
            "Webhooks and deploy keys: register them again",
            "Reactions on issues and comments",
            "Projects and boards",
            "Discussions",
        ],
    }
}

/// Build the receipt of a finished import into `<owner>/<name>.git` under
/// `repo_root`.
pub(crate) async fn build(
    db: &DatabaseConnection,
    repo_root: &Path,
    platform: &str,
    target: (&str, &str),
    stats: &ImportStats,
) -> anyhow::Result<ImportReceipt> {
    let inventory = &stats.inventory;
    let mut objects = Vec::new();
    if let Some(repo_id) = inventory.repo_id {
        if let Some(source) = &inventory.labels {
            let here = rg_db::ops::label_ops::list_by_repo(db, repo_id)
                .await?
                .into_iter()
                .map(|label| label.name)
                .collect();
            objects.push(named("labels", source, &here, str::to_owned));
        }
        if let Some(source) = &inventory.milestones {
            let here = rg_db::ops::milestone_ops::list_by_repo(db, repo_id, None)
                .await?
                .into_iter()
                .map(|milestone| milestone.title)
                .collect();
            objects.push(named("milestones", source, &here, str::to_owned));
        }
        let numbered = |numbers: &[i64]| numbers.iter().map(i64::to_string).collect::<Vec<_>>();
        let hash = |number: &str| format!("#{number}");
        if let Some(source) = &inventory.issues {
            let here = rg_db::ops::issue_ops::numbers_in_repo(db, repo_id)
                .await?
                .into_iter()
                .map(|number| number.to_string())
                .collect();
            objects.push(named("issues", &numbered(source), &here, hash));
        }
        if let Some(source) = &inventory.pull_requests {
            let here = rg_db::ops::pull_request_ops::numbers_in_repo(db, repo_id)
                .await?
                .into_iter()
                .map(|number| number.to_string())
                .collect();
            objects.push(named("pull_requests", &numbered(source), &here, hash));
        }
        if let Some(source) = inventory.comments {
            let mut comments = counted(
                "comments",
                Some(source),
                stats.issue_comments_imported + stats.pr_comments_imported,
                stats
                    .comments_failed
                    .iter()
                    .filter(|failure| !failure.source.contains(" review comment "))
                    .map(|failure| failure.source.clone()),
            );
            comments.skipped = stats.system_notes_skipped;
            if comments.skipped > 0 {
                comments.skipped_reason =
                    Some("system notes describe the source's own events, not what a person wrote");
            }
            comments.not_imported = comments.not_imported.saturating_sub(comments.skipped);
            objects.push(comments);
        }
        if let Some(source) = inventory.reviews {
            objects.push(counted(
                "reviews",
                Some(source),
                stats.pr_reviews_imported,
                stats
                    .pr_reviews_failed
                    .iter()
                    .map(|failure| failure.source.clone()),
            ));
        }
        if let Some(source) = inventory.review_comments {
            // Their failures are listed with the other comments'; the count
            // here is what the writer put under a review.
            objects.push(counted(
                "review_comments",
                Some(source),
                stats.pr_review_comments_imported,
                stats
                    .comments_failed
                    .iter()
                    .filter(|failure| failure.source.contains(" review comment "))
                    .map(|failure| failure.source.clone()),
            ));
        }
        if let Some(source) = &inventory.releases {
            let here = rg_db::ops::release_ops::tag_names_in_repo(db, repo_id)
                .await?
                .into_iter()
                .collect();
            objects.push(named("releases", source, &here, str::to_owned));
        }
    }
    if stats.repo_cloned {
        objects.push(counted(
            "lfs_objects",
            Some(stats.lfs_objects_imported + stats.lfs_objects_failed.len()),
            stats.lfs_objects_imported,
            stats
                .lfs_objects_failed
                .iter()
                .map(|failure| failure.oid.clone()),
        ));
    }
    if stats.wiki_pages_imported > 0 {
        objects.push(counted(
            "wiki_pages",
            None,
            stats.wiki_pages_imported,
            Vec::new(),
        ));
    }

    let repository = repo_root.join(format!("{}/{}.git", target.0, target.1));
    let ci = if stats.repo_cloned {
        let repository = repository.clone();
        Some(
            crate::blocking::run_blocking_git("import receipt CI", move || ci_not_run(&repository))
                .await
                .map(|not_run| CiReceipt {
                    unavailable: None,
                    not_run,
                })
                .unwrap_or_else(|error| CiReceipt {
                    unavailable: Some(format!("the imported branch could not be read: {error:#}")),
                    not_run: Vec::new(),
                }),
        )
    } else {
        None
    };
    let refs = match &inventory.refs {
        None => RefsReceipt {
            unavailable: Some(
                "the target already held a repository, so nothing was cloned to compare".to_owned(),
            ),
            ..RefsReceipt::default()
        },
        Some(Err(reason)) => RefsReceipt {
            unavailable: Some(reason.clone()),
            ..RefsReceipt::default()
        },
        Some(Ok(listing)) => {
            let listing = listing.clone();
            crate::blocking::run_blocking_git("import receipt refs", move || {
                refs_receipt(&listing, &repository)
            })
            .await
            .unwrap_or_else(|error| RefsReceipt {
                unavailable: Some(format!("the imported refs could not be read: {error:#}")),
                ..RefsReceipt::default()
            })
        }
    };

    Ok(ImportReceipt {
        objects,
        refs,
        not_transferable: not_transferable(platform),
        ci,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_named_kind_counts_what_both_sides_have_and_names_the_rest() {
        let source = (1..=13).map(|n| n.to_string()).collect::<Vec<_>>();
        let target = ["1", "2", "99"].map(str::to_owned).into_iter().collect();
        let receipt = named("issues", &source, &target, |n| format!("#{n}"));
        assert_eq!(receipt.source, Some(13));
        assert_eq!(receipt.imported, 2);
        assert_eq!(receipt.not_imported, 11);
        assert_eq!(receipt.missing.len(), RECEIPT_NAME_SAMPLE);
        assert_eq!(receipt.missing_omitted, 1);
        assert_eq!(receipt.missing[0], "#10", "sorted by name, as listed");
    }

    /// A work tree with `files` committed on `main`; `None` leaves it unborn.
    fn repository_with(files: Option<&[(&str, &str)]>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        git.run_or_bail(
            &["init", "-q", "-b", "main", dir.path().to_str().unwrap()],
            None,
        )
        .unwrap();
        let Some(files) = files else {
            return dir;
        };
        for (path, contents) in files {
            let file = dir.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, contents).unwrap();
        }
        git.run_or_bail(&["add", "."], Some(dir.path())).unwrap();
        git.run_or_bail(
            &[
                "-c",
                "user.name=receipt test",
                "-c",
                "user.email=receipt@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-qm",
                "fixture",
            ],
            Some(dir.path()),
        )
        .unwrap();
        dir
    }

    /// card_5e13b87a247f: a project moved from GitLab keeps its
    /// `.gitlab-ci.yml`, nothing here runs it, and the receipt has to say so.
    #[test]
    fn a_gitlab_ci_file_with_nothing_this_server_runs_is_named() {
        let repo = repository_with(Some(&[(".gitlab-ci.yml", "build:\n  script: [make]\n")]));
        assert_eq!(
            ci_not_run(repo.path()).unwrap(),
            [CiNotRun {
                path: ".gitlab-ci.yml",
                reason: FOREIGN_CI[0].1,
            }]
        );
    }

    /// Beside CI this server does run, a foreign file is somebody's choice, not
    /// a loss — whichever source the engine reads is the one present.
    #[test]
    fn a_gitlab_ci_file_beside_ci_that_runs_here_is_not_named() {
        for source in [
            ".forgejo/workflows/ci.yml",
            ".github/workflows/ci.yml",
            ".gitea/workflows/ci.yml",
            ".plombir-git-ci.yml",
        ] {
            let repo = repository_with(Some(&[
                (".gitlab-ci.yml", "build:\n  script: [make]\n"),
                (source, "on: push\n"),
            ]));
            let not_run = ci_not_run(repo.path()).unwrap();
            assert!(not_run.is_empty(), "beside {source}: {not_run:?}");
        }

        assert!(ci_not_run(repository_with(None).path()).unwrap().is_empty());
        let plain = repository_with(Some(&[("README.md", "hello\n")]));
        assert!(ci_not_run(plain.path()).unwrap().is_empty());
    }

    /// The day the engine learns to read one of these, its entry here becomes
    /// a claim that is false — and it would never fire, so nothing else says so.
    /// The remedy names every source the engine does read.
    #[test]
    fn a_foreign_ci_source_is_one_the_engine_does_not_read() {
        let read: Vec<&str> = crate::ci::WORKFLOW_DIRS
            .iter()
            .chain(crate::ci::NATIVE_CI_FILES.iter())
            .copied()
            .collect();
        for (path, _) in FOREIGN_CI {
            assert!(
                !read.contains(&path),
                "{path} is read by the engine now; drop it from FOREIGN_CI"
            );
        }
        for source in read {
            assert!(
                FOREIGN_CI[0].1.contains(source),
                "the GitLab remedy does not name {source}, a CI source the engine reads"
            );
        }
    }

    #[test]
    fn a_counted_kind_says_more_is_missing_than_failed_when_the_tally_is_short() {
        let receipt = counted("comments", Some(10), 7, ["issue #3 comment 9".to_owned()]);
        assert_eq!(receipt.not_imported, 3, "three gone, one of them named");
        assert_eq!(receipt.missing, ["issue #3 comment 9"]);
    }
}
