//! Merge requests imported from GitLab name their target repository as head.
//!
//! The GitLab importer used to store a merge request's `source_project_id` — a
//! project id on the *source* instance — in `pull_requests.head_repo_id`, which
//! names a row of *this* instance's `repositories` (card_2f18e715892e). Every
//! such merge request became a fork pull request from whatever local repository
//! happened to carry that primary key, or from a repository that does not exist.
//! New imports write `NULL` (card_c2799a9fca28); this migration repairs the rows
//! imported before that (card_7559ec4ef7fc).
//!
//! ## Which rows are an imported merge request
//!
//! A dangling `head_repo_id` alone proves nothing: a genuine fork pull request
//! whose fork was deleted has one too, and `fork_source::unavailable` relies on
//! it — clearing it would turn that pull request into one from the target
//! repository. So a row is repaired only on evidence that the importer wrote it:
//!
//! * `original_service LIKE 'gitlab%'` — the byline only the GitLab importer
//!   writes, as `gitlab` or `gitlab:<host>`. Native creation never sets it.
//! * The byline is empty when the merge request had no author, so for those the
//!   evidence is the import itself, all of it at once: the repository has a
//!   GitLab import task that imported pull requests; the pull request's author
//!   is the account that ran that import; it has no `pull_request_opened`
//!   timeline event, which `create_pr` records for every pull request it opens
//!   and no importer writes; and its head is not a live fork of the target —
//!   `resolve_head_ref` accepts a head repository only if its `origin_repo_id`
//!   is the target, so such a head can only have come from `create_pr`.
//!
//! What is left after this — a byline-less row whose repository no longer has
//! its import task — cannot be told apart from a fork pull request with a
//! deleted fork. The migration counts those and says so instead of guessing.

use sea_orm::{ConnectionTrait, DbErr, Statement, TryGetable};
use sea_orm_migration::prelude::*;

pub const NAME: &str = "m20261127_000002_gitlab_merge_requests_head_in_target";

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        NAME
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let repaired = repair_imported_heads(manager.get_connection()).await?;
        if repaired.with_byline + repaired.without_byline > 0 {
            tracing::warn!(
                migration = NAME,
                with_gitlab_byline = repaired.with_byline,
                without_byline = repaired.without_byline,
                "cleared pull_requests.head_repo_id on merge requests imported from GitLab: \
                 it held GitLab's source_project_id, not a local repository, and made each \
                 one a fork pull request from an unrelated or missing repository"
            );
        }
        if repaired.undecidable > 0 {
            tracing::warn!(
                migration = NAME,
                pull_requests = repaired.undecidable,
                "left head_repo_id alone on pull requests whose head repository is gone and \
                 whose origin cannot be read: each is either a fork pull request whose fork \
                 was deleted or a byline-less GitLab import whose import task no longer exists"
            );
        }
        Ok(())
    }

    /// The old value was another instance's project id; there is nothing to
    /// put back.
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

/// What one run changed, and what it could not decide.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Repaired {
    pub with_byline: u64,
    pub without_byline: u64,
    pub undecidable: i64,
}

/// A merge request the GitLab importer wrote with a byline.
const WITH_BYLINE: &str = "head_repo_id IS NOT NULL AND original_service LIKE 'gitlab%'";

/// A merge request the GitLab importer wrote without one; see the module docs
/// for why each clause is needed.
const WITHOUT_BYLINE: &str = "\
    head_repo_id IS NOT NULL \
    AND original_service IS NULL \
    AND EXISTS (SELECT 1 FROM import_tasks t \
                WHERE t.repo_id = pull_requests.repo_id \
                  AND t.platform = 'gitlab' \
                  AND t.import_pull_requests = TRUE \
                  AND t.user_id = pull_requests.author_id) \
    AND NOT EXISTS (SELECT 1 FROM pr_events e \
                    WHERE e.pr_id = pull_requests.id \
                      AND e.event_type = 'pull_request_opened') \
    AND NOT EXISTS (SELECT 1 FROM repositories r \
                    WHERE r.id = pull_requests.head_repo_id \
                      AND r.origin_repo_id = pull_requests.repo_id)";

/// Byline-less rows whose head repository is gone and that [`WITHOUT_BYLINE`]
/// did not claim — reported, never changed.
const UNDECIDABLE: &str = "\
    SELECT COUNT(*) FROM pull_requests \
    WHERE head_repo_id IS NOT NULL \
      AND original_service IS NULL \
      AND NOT EXISTS (SELECT 1 FROM repositories r WHERE r.id = pull_requests.head_repo_id) \
      AND NOT EXISTS (SELECT 1 FROM pr_events e \
                      WHERE e.pr_id = pull_requests.id \
                        AND e.event_type = 'pull_request_opened')";

/// Clear the `head_repo_id` the GitLab importer wrote, returning how many rows
/// changed. Apart from the migration so it can run against an already
/// migrated server database: a second run finds nothing to change.
pub(crate) async fn repair_imported_heads<C: ConnectionTrait>(db: &C) -> Result<Repaired, DbErr> {
    let with_byline = db
        .execute_unprepared(&format!(
            "UPDATE pull_requests SET head_repo_id = NULL WHERE {WITH_BYLINE}"
        ))
        .await?
        .rows_affected();
    let without_byline = db
        .execute_unprepared(&format!(
            "UPDATE pull_requests SET head_repo_id = NULL WHERE {WITHOUT_BYLINE}"
        ))
        .await?
        .rows_affected();
    let undecidable = count(db, UNDECIDABLE).await?;
    Ok(Repaired {
        with_byline,
        without_byline,
        undecidable,
    })
}

async fn count<C: ConnectionTrait>(db: &C, sql: &str) -> Result<i64, DbErr> {
    let row = db
        .query_one(Statement::from_string(
            db.get_database_backend(),
            sql.to_string(),
        ))
        .await?
        .ok_or_else(|| DbErr::Migration(format!("{NAME}: `{sql}` returned no row")))?;
    i64::try_get_by_index(&row, 0)
        .map_err(|error| DbErr::Migration(format!("{NAME}: decode `{sql}`: {error:?}")))
}

#[cfg(test)]
#[path = "m20261127_000002_gitlab_merge_requests_head_in_target_tests.rs"]
mod tests;
