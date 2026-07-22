pub mod m20260424_000001_create_users;
pub mod m20260424_000002_create_repositories;
pub mod m20260424_000003_create_keys_tokens;
pub mod m20260424_000004_create_issues;
pub mod m20260424_000005_create_pull_requests;
pub mod m20260424_000006_create_wiki_lfs_webhooks;
pub mod m20260424_000007_create_pipelines;
pub mod m20260424_000008_create_phase6;
pub mod m20260424_000009_create_phase8;
pub mod m20260427_000001_add_lfs_compression;
pub mod m20260508_000001_create_repo_stars_watches;
pub mod m20260508_000002_create_releases;
pub mod m20260508_000003_create_labels;
pub mod m20260508_000004_create_commit_statuses;
pub mod m20260508_000005_create_fts5_indexes;
pub mod m20260508_000006_add_repo_soft_delete;
pub mod m20260510_000001_create_runners;
pub mod m20260510_000002_alter_pipeline_jobs_add_runner_fields;
pub mod m20260510_000003_add_pipeline_jobs_updated_at;
pub mod m20260510_000004_create_artifacts;
pub mod m20260511_000001_add_pr_head_repo_id;
pub mod m20260511_000002_add_missing_indexes;
pub mod m20260511_000003_fix_fts5_triggers;
pub mod m20260512_000001_create_code_fts;
pub mod m20260607_000001_create_mirrors;
pub mod m20260607_000002_create_boards;
pub mod m20260607_000003_create_time_entries;
pub mod m20260607_000004_create_import_tasks;
pub mod m20260607_000005_create_package_registry;
pub mod m20260607_000006_alter_users_auth;
pub mod m20260607_000007_create_oauth_accounts;
pub mod m20260607_000008_create_mfa_backup_codes;
pub mod m20260607_000009_create_login_logs;
pub mod m20260607_000010_create_sso_providers;
pub mod m20260607_000011_create_audit_logs;
pub mod m20260608_000001_create_oci_tables;
pub mod m20260608_000002_oauth_accounts_unique;
pub mod m20260608_000003_add_job_tags;
pub mod m20260616_0000015_rename_org_team_plural;
pub mod m20260616_000001_create_password_reset_tokens;
pub mod m20260616_000002_add_soft_delete_columns;
pub mod m20260617_000001_create_wiki_revisions;
pub mod m20260617_000002_rename_board_time_tables_plural;
pub mod m20260621_000001_add_pr_labels_milestone;
pub mod m20260629_000001_rename_import_task_plural;
pub mod m20260705_000001_rename_package_tables_plural;
pub mod m20260711_000001_pr_review_workflow;
pub mod m20260711_000002_pr_auto_merge;
pub mod m20260711_000003_merge_queue;
pub mod m20260711_000004_review_suggestions;
pub mod m20260711_000005_review_comment_ranges;
pub mod m20260712_000001_create_pr_events;
pub mod m20260712_000002_create_deploy_keys;
pub mod m20260712_000003_add_pipeline_job_variables;
pub mod m20260712_000004_merge_queue_groups;
pub mod m20260712_000005_create_ci_secrets;
pub mod m20260712_000006_create_protected_tags;
pub mod m20260712_000007_require_signed_commits;
pub mod m20260712_000008_add_pipeline_job_cache;
pub mod m20260712_000009_add_pipeline_job_execution_policy;
pub mod m20260712_000010_add_pipeline_job_when;
pub mod m20260712_000011_create_ci_environments;
pub mod m20260712_000012_create_ci_retention;
pub mod m20260712_000013_add_pipeline_job_condition;
pub mod m20260712_000014_add_ldap_provider_identity;
pub mod m20260712_000015_fix_oauth_accounts_table_name;
pub mod m20260713_000001_fix_postgres_utc_timestamps;
pub mod m20260714_000001_create_attachments;
pub mod m20260714_000002_repair_mysql_fts_triggers;

use sea_orm_migration::prelude::*;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260424_000001_create_users::Migration),
            Box::new(m20260424_000002_create_repositories::Migration),
            Box::new(m20260424_000003_create_keys_tokens::Migration),
            Box::new(m20260424_000004_create_issues::Migration),
            Box::new(m20260424_000005_create_pull_requests::Migration),
            Box::new(m20260424_000006_create_wiki_lfs_webhooks::Migration),
            Box::new(m20260424_000007_create_pipelines::Migration),
            Box::new(m20260424_000008_create_phase6::Migration),
            Box::new(m20260424_000009_create_phase8::Migration),
            Box::new(m20260427_000001_add_lfs_compression::Migration),
            Box::new(m20260508_000001_create_repo_stars_watches::Migration),
            Box::new(m20260508_000006_add_repo_soft_delete::Migration),
            Box::new(m20260508_000002_create_releases::Migration),
            Box::new(m20260508_000003_create_labels::Migration),
            Box::new(m20260508_000004_create_commit_statuses::Migration),
            Box::new(m20260508_000005_create_fts5_indexes::Migration),
            Box::new(m20260510_000001_create_runners::Migration),
            Box::new(m20260510_000002_alter_pipeline_jobs_add_runner_fields::Migration),
            Box::new(m20260510_000003_add_pipeline_jobs_updated_at::Migration),
            Box::new(m20260510_000004_create_artifacts::Migration),
            Box::new(m20260511_000001_add_pr_head_repo_id::Migration),
            Box::new(m20260511_000002_add_missing_indexes::Migration),
            Box::new(m20260511_000003_fix_fts5_triggers::Migration),
            Box::new(m20260512_000001_create_code_fts::Migration),
            // Must precede migrations that reference plural org/team tables.
            // It is a no-op on fresh schemas created with the corrected names.
            Box::new(m20260616_0000015_rename_org_team_plural::Migration),
            Box::new(m20260607_000001_create_mirrors::Migration),
            Box::new(m20260607_000002_create_boards::Migration),
            Box::new(m20260607_000003_create_time_entries::Migration),
            Box::new(m20260607_000004_create_import_tasks::Migration),
            Box::new(m20260607_000005_create_package_registry::Migration),
            Box::new(m20260607_000006_alter_users_auth::Migration),
            Box::new(m20260607_000007_create_oauth_accounts::Migration),
            Box::new(m20260607_000008_create_mfa_backup_codes::Migration),
            Box::new(m20260607_000009_create_login_logs::Migration),
            Box::new(m20260607_000010_create_sso_providers::Migration),
            Box::new(m20260607_000011_create_audit_logs::Migration),
            Box::new(m20260608_000001_create_oci_tables::Migration),
            Box::new(m20260608_000002_oauth_accounts_unique::Migration),
            Box::new(m20260608_000003_add_job_tags::Migration),
            Box::new(m20260616_000001_create_password_reset_tokens::Migration),
            Box::new(m20260616_000002_add_soft_delete_columns::Migration),
            Box::new(m20260617_000001_create_wiki_revisions::Migration),
            Box::new(m20260617_000002_rename_board_time_tables_plural::Migration),
            Box::new(m20260621_000001_add_pr_labels_milestone::Migration),
            Box::new(m20260629_000001_rename_import_task_plural::Migration),
            Box::new(m20260705_000001_rename_package_tables_plural::Migration),
            Box::new(m20260711_000001_pr_review_workflow::Migration),
            Box::new(m20260711_000002_pr_auto_merge::Migration),
            Box::new(m20260711_000003_merge_queue::Migration),
            Box::new(m20260711_000004_review_suggestions::Migration),
            Box::new(m20260711_000005_review_comment_ranges::Migration),
            Box::new(m20260712_000001_create_pr_events::Migration),
            Box::new(m20260712_000002_create_deploy_keys::Migration),
            Box::new(m20260712_000003_add_pipeline_job_variables::Migration),
            Box::new(m20260712_000004_merge_queue_groups::Migration),
            Box::new(m20260712_000005_create_ci_secrets::Migration),
            Box::new(m20260712_000006_create_protected_tags::Migration),
            Box::new(m20260712_000007_require_signed_commits::Migration),
            Box::new(m20260712_000008_add_pipeline_job_cache::Migration),
            Box::new(m20260712_000009_add_pipeline_job_execution_policy::Migration),
            Box::new(m20260712_000010_add_pipeline_job_when::Migration),
            Box::new(m20260712_000011_create_ci_environments::Migration),
            Box::new(m20260712_000012_create_ci_retention::Migration),
            Box::new(m20260712_000013_add_pipeline_job_condition::Migration),
            Box::new(m20260712_000014_add_ldap_provider_identity::Migration),
            Box::new(m20260712_000015_fix_oauth_accounts_table_name::Migration),
            Box::new(m20260713_000001_fix_postgres_utc_timestamps::Migration),
            Box::new(m20260714_000001_create_attachments::Migration),
            Box::new(m20260714_000002_repair_mysql_fts_triggers::Migration),
        ]
    }
}
