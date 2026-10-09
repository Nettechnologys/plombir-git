//! Indexes that match what the server actually asks the database
//! (card_f2ca9c2005f2, card_6eba06ebef97).
//!
//! Every paginated listing filters on one column and orders on two more, but
//! the indexes it found covered only the filter. SQLite then read every
//! matching row and sorted the lot in a temporary B-tree to hand back twenty:
//! 30k issues cost 16-72 ms a page, a webhook's last 50 deliveries read 50k
//! pages because each row's payload came along for the sort, and `/explore` —
//! open to anonymous visitors — scanned the whole `repositories` table. Each
//! listing now has an index whose columns are its `WHERE` equalities followed
//! by its `ORDER BY`, so the page is read straight off the index in order.
//! `id` is spelled out as the tiebreaker even though SQLite appends the rowid
//! anyway: PostgreSQL does not.
//!
//! An index that is a strict prefix of its replacement is dropped once the
//! replacement exists. It answered no query the wider one cannot, and every
//! write paid for it.
//!
//! The runner poll (card_6eba06ebef97) asked for pending, unassigned jobs and
//! found no index on `pipeline_jobs.status`, so every poll every three seconds
//! walked the repository's whole CI history; the stuck-job watchdog and the
//! running-jobs gauge scanned the table outright. `(status, runner_id)` serves
//! all of them.
//!
//! The rest are foreign-key columns with no leading index. Deleting a user,
//! repository, board column or issue makes the engine look for children by
//! that column — a full scan of the child table per deleted parent — and the
//! membership columns (`organization_members.user_id`, `team_members.user_id`,
//! `repo_collaborators.user_id`) are read by the visibility filter of every
//! repository listing.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261009_000003_listing_and_foreign_key_indexes"
    }
}

/// `(name, table, columns)` of every index this migration creates.
pub(crate) const CREATED: &[(&str, &str, &[&str])] = &[
    // Paginated listings: WHERE equalities, then ORDER BY.
    (
        "idx_issues_repo_state_created",
        "issues",
        &["repo_id", "state", "created_at", "id"],
    ),
    (
        "idx_issues_repo_created",
        "issues",
        &["repo_id", "created_at", "id"],
    ),
    (
        "idx_pull_requests_repo_state_created",
        "pull_requests",
        &["repo_id", "state", "created_at", "id"],
    ),
    (
        "idx_pull_requests_repo_created",
        "pull_requests",
        &["repo_id", "created_at", "id"],
    ),
    (
        "idx_pipelines_repo_created",
        "pipelines",
        &["repo_id", "created_at", "id"],
    ),
    (
        "idx_notifications_user_created",
        "notifications",
        &["user_id", "created_at", "id"],
    ),
    (
        "idx_notifications_user_read_created",
        "notifications",
        &["user_id", "is_read", "created_at", "id"],
    ),
    (
        "idx_repositories_public_updated",
        "repositories",
        &["is_private", "deleted_at", "updated_at", "id"],
    ),
    (
        "idx_webhook_deliveries_webhook_created",
        "webhook_deliveries",
        &["webhook_id", "created_at", "id"],
    ),
    (
        "idx_releases_repo_created",
        "releases",
        &["repo_id", "created_at", "id"],
    ),
    (
        "idx_repo_stars_repo_created",
        "repo_stars",
        &["repo_id", "created_at", "id"],
    ),
    (
        "idx_milestones_repo_state_created",
        "milestones",
        &["repo_id", "state", "created_at", "id"],
    ),
    // Runner poll, stuck-job watchdog, running-jobs gauge.
    (
        "idx_pipeline_jobs_status_runner",
        "pipeline_jobs",
        &["status", "runner_id"],
    ),
    // Boards: listed by owner, columns and cards by position.
    ("idx_boards_repo_name", "boards", &["repo_id", "name"]),
    ("idx_boards_org_name", "boards", &["org_id", "name"]),
    (
        "idx_board_columns_board_position",
        "board_columns",
        &["board_id", "position", "id"],
    ),
    (
        "idx_board_cards_column_position",
        "board_cards",
        &["column_id", "position", "id"],
    ),
    ("idx_board_cards_issue", "board_cards", &["issue_id"]),
    // Foreign keys a parent delete or a visibility filter looks up.
    ("idx_ssh_keys_user", "ssh_keys", &["user_id"]),
    ("idx_access_tokens_user", "access_tokens", &["user_id"]),
    ("idx_runners_repo", "runners", &["repo_id"]),
    (
        "idx_pr_reviewer_requests_reviewer",
        "pr_reviewer_requests",
        &["reviewer_id"],
    ),
    ("idx_pr_events_repo", "pr_events", &["repo_id"]),
    ("idx_pr_events_actor", "pr_events", &["actor_id"]),
    ("idx_releases_author", "releases", &["author_id"]),
    (
        "idx_release_assets_uploader",
        "release_assets",
        &["uploader_id"],
    ),
    ("idx_attachments_uploader", "attachments", &["uploader_id"]),
    (
        "idx_ci_secrets_created_by",
        "ci_secrets",
        &["created_by_id"],
    ),
    (
        "idx_deploy_keys_created_by",
        "deploy_keys",
        &["created_by_id"],
    ),
    (
        "idx_commit_statuses_creator",
        "commit_statuses",
        &["creator_id"],
    ),
    ("idx_boards_created_by", "boards", &["created_by"]),
    (
        "idx_ci_environment_approvals_environment",
        "ci_environment_approvals",
        &["environment_id"],
    ),
    (
        "idx_ci_environment_approvals_approved_by",
        "ci_environment_approvals",
        &["approved_by"],
    ),
    (
        "idx_repo_collaborators_user",
        "repo_collaborators",
        &["user_id"],
    ),
    (
        "idx_organization_members_user",
        "organization_members",
        &["user_id"],
    ),
    ("idx_team_members_user", "team_members", &["user_id"]),
    (
        "idx_npm_dist_tags_version",
        "npm_dist_tags",
        &["version_id"],
    ),
    ("idx_lfs_locks_owner", "lfs_locks", &["owner_id"]),
    (
        "idx_email_confirmations_user",
        "email_confirmations",
        &["user_id"],
    ),
    (
        "idx_thread_subscriptions_repo",
        "thread_subscriptions",
        &["repo_id"],
    ),
];

/// `(name, table, columns)` of the indexes a wider one above made redundant.
/// `down` recreates them exactly.
pub(crate) const SUPERSEDED: &[(&str, &str, &[&str])] = &[
    ("idx_issues_repo_state", "issues", &["repo_id", "state"]),
    ("idx_pr_repo_state", "pull_requests", &["repo_id", "state"]),
    ("idx_pipelines_repo_id", "pipelines", &["repo_id"]),
    (
        "idx_notifications_user_id_is_read",
        "notifications",
        &["user_id", "is_read"],
    ),
    (
        "idx_delivery_webhook",
        "webhook_deliveries",
        &["webhook_id"],
    ),
    ("idx_releases_repo_id", "releases", &["repo_id"]),
    ("idx_repo_stars_repo_id", "repo_stars", &["repo_id"]),
];

/// Indexes MySQL adopts as a foreign key's own index, by name.
///
/// InnoDB keeps an implicit index on every foreign-key column that has none,
/// and silently drops it once another index can serve the constraint — these,
/// each led by such a column. From then on MySQL refuses to drop them
/// (`ERROR 1553`), so `down` leaves them standing there: the column has an
/// index either way, which is what MySQL had before `up`.
const MYSQL_CONSTRAINT_INDEXES: &[&str] = &[
    "idx_boards_repo_name",
    "idx_boards_org_name",
    "idx_board_columns_board_position",
    "idx_board_cards_column_position",
    "idx_board_cards_issue",
    "idx_ssh_keys_user",
    "idx_access_tokens_user",
    "idx_runners_repo",
    "idx_pr_reviewer_requests_reviewer",
    "idx_pr_events_repo",
    "idx_pr_events_actor",
    "idx_releases_author",
    "idx_release_assets_uploader",
    "idx_attachments_uploader",
    "idx_ci_secrets_created_by",
    "idx_deploy_keys_created_by",
    "idx_commit_statuses_creator",
    "idx_boards_created_by",
    "idx_ci_environment_approvals_environment",
    "idx_ci_environment_approvals_approved_by",
    "idx_repo_collaborators_user",
    "idx_organization_members_user",
    "idx_team_members_user",
    "idx_npm_dist_tags_version",
    "idx_lfs_locks_owner",
    "idx_email_confirmations_user",
    "idx_thread_subscriptions_repo",
];

fn create(name: &str, table: &str, columns: &[&str]) -> IndexCreateStatement {
    let mut index = Index::create();
    index.name(name).table(Alias::new(table));
    for column in columns {
        index.col(Alias::new(*column));
    }
    index.to_owned()
}

fn drop(name: &str, table: &str) -> IndexDropStatement {
    Index::drop().name(name).table(Alias::new(table)).to_owned()
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Replacements first: on MySQL a foreign key must keep an index whose
        // leading column is its own, so the narrow one may only go once the
        // wide one stands. Each step checks the catalogue rather than assume
        // it: after a MySQL `down` the indexes in MYSQL_CONSTRAINT_INDEXES are
        // still there, and a second `up` must not trip over them.
        for (name, table, columns) in CREATED {
            if !manager.has_index(table, name).await? {
                manager.create_index(create(name, table, columns)).await?;
            }
        }
        for (name, table, _) in SUPERSEDED {
            if manager.has_index(table, name).await? {
                manager.drop_index(drop(name, table)).await?;
            }
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (name, table, columns) in SUPERSEDED {
            if !manager.has_index(table, name).await? {
                manager.create_index(create(name, table, columns)).await?;
            }
        }
        let mysql = manager.get_database_backend() == sea_orm::DatabaseBackend::MySql;
        for (name, table, _) in CREATED.iter().rev() {
            if mysql && MYSQL_CONSTRAINT_INDEXES.contains(name) {
                continue;
            }
            manager.drop_index(drop(name, table)).await?;
        }
        Ok(())
    }
}
