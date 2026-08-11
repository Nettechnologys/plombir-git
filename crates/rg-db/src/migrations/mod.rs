mod fts_safety;
pub(crate) mod ghost_author;
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
pub mod m20260723_000001_create_passkey_credentials;
pub mod m20260723_000002_add_release_asset_sha256;
pub mod m20260724_000001_add_attachment_sha256;
pub mod m20260724_000002_add_artifact_sha256;
pub mod m20260724_000003_add_release_asset_attestation;
pub mod m20260724_000004_add_ci_cache_sha256;
pub mod m20260726_000001_rename_mirror_table_plural;
pub mod m20260727_000001_clear_plaintext_mirror_passwords;
pub mod m20260727_000002_clear_import_task_auth_tokens;
pub mod m20260728_000001_create_instance_settings;
pub mod m20260728_000002_add_package_file_digests;
pub mod m20260730_000001_repositories_namespace_unique;
pub mod m20260801_000001_create_instance_signing_key;
pub mod m20260802_000001_create_encryption_key_check;
pub mod m20260802_000002_add_user_session_version;
pub mod m20260802_000003_identity_keys_not_blank;
pub mod m20260803_000001_add_sso_provisioning_policy;
pub mod m20260803_000002_add_passkey_credential_rp_id;
pub mod m20260804_000001_add_lfs_publication_lease;
pub mod m20260804_000002_hash_runner_tokens;
pub mod m20260804_000003_rename_webhook_secret_encrypted;
pub mod m20260804_000004_wiki_revision_version_unique;
pub mod m20260804_000005_wiki_page_edit_version;
pub mod m20260804_000006_repo_fts_soft_delete;
pub mod m20260805_000001_issue_labels_single_source;
pub mod m20260805_000002_uploads_outlive_their_uploader;
pub mod m20260805_000003_create_oci_publication_lease;
pub mod m20260805_000004_repo_config_outlives_its_author;
pub mod m20260805_000005_account_owned_rows_follow_their_parent;
pub mod m20260805_000006_clean_serialized_user_grants;
pub mod m20260806_000001_normalize_user_grants;
pub mod m20260806_000002_create_repository_transfer_lease;
pub mod m20260807_000001_create_mirror_sync_lease;
pub mod m20260807_000002_add_user_totp_last_step;
pub mod m20260807_000003_create_webauthn_ceremony_spend;
pub mod m20260808_000001_drop_oci_blob_ref_count;
pub mod m20260808_000002_add_pull_request_ci_approval;
pub mod m20260808_000003_add_pipeline_concurrency_group;
pub mod m20260808_000004_drop_import_task_user_mapping;
pub mod m20260809_000001_package_file_filename_unique;
pub mod m20260810_000001_drop_unused_storage_metadata;
pub mod m20260810_000002_create_npm_dist_tags;
pub mod m20260810_000003_nuget_protocol_version_key;
pub mod m20260810_000004_pypi_protocol_version_key;
pub mod m20260811_000001_cargo_protocol_version_key;
pub mod m20260811_000002_rubygems_protocol_version_key;
pub mod m20260811_000003_helm_protocol_version_key;
pub mod m20260811_000004_composer_protocol_version_key;

use sea_orm_migration::prelude::*;

/// Replace every SQLite connection that predates a committed schema change, so
/// the next borrower cannot fail once with a false `no such table`.
///
/// Waiting until every current connection is idle and acquiring that exact set
/// at once avoids racing the pool into handing us one physical connection
/// repeatedly. One old connection remains alive as an anchor until a fresh one
/// has opened; this preserves `sqlite::memory:` databases, which disappear when
/// their final physical connection closes. A one-connection pool needs no
/// replacement — that connection performed the migration itself — but its SQLx
/// statement cache is still cleared as a future-proof belt.
pub(crate) async fn refresh_sqlite_pool_after_schema_change(
    pool: &sea_orm::sqlx::SqlitePool,
    context: &str,
) -> Result<(), DbErr> {
    use sea_orm::sqlx::Connection as _;

    let timeout = pool.options().get_acquire_timeout();
    let deadline = std::time::Instant::now() + timeout;

    loop {
        let existing = pool.size();
        if pool.num_idle() == existing as usize {
            let mut connections = Vec::with_capacity(existing as usize);
            for _ in 0..existing {
                let Some(connection) = pool.try_acquire() else {
                    break;
                };
                connections.push(connection);
            }

            if connections.len() == existing as usize {
                if connections.len() == 1 {
                    (**connections.first_mut().expect("length checked"))
                        .clear_cached_statements()
                        .await
                        .map_err(|error| {
                            DbErr::Migration(format!(
                                "{context}: schema change committed, but a SQLite connection's \
                                 cached statements could not be cleared: {error}"
                            ))
                        })?;
                } else if let Some(mut anchor) = connections.pop() {
                    for connection in &mut connections {
                        connection.close_on_drop();
                    }
                    drop(connections);

                    let replacement = pool.acquire().await.map_err(|error| {
                        DbErr::Migration(format!(
                            "{context}: schema change committed, but a fresh SQLite connection \
                             could not be opened while recycling the pool: {error}"
                        ))
                    })?;
                    anchor.close_on_drop();
                    drop(anchor);
                    drop(replacement);
                }
                tracing::info!(
                    refreshed_connections = existing,
                    context,
                    "Refreshed SQLite connections after a schema change"
                );
                return Ok(());
            }
        }

        if std::time::Instant::now() >= deadline {
            return Err(DbErr::Migration(format!(
                "{context}: schema change committed, but the SQLite pool could not be refreshed \
                 within {timeout:?} ({} of {} connection(s) idle); refusing to continue with \
                 connections that may report a false `no such table`",
                pool.num_idle(),
                pool.size()
            )));
        }
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
    }
}

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
            Box::new(m20260723_000001_create_passkey_credentials::Migration),
            Box::new(m20260723_000002_add_release_asset_sha256::Migration),
            Box::new(m20260724_000001_add_attachment_sha256::Migration),
            Box::new(m20260724_000002_add_artifact_sha256::Migration),
            Box::new(m20260724_000003_add_release_asset_attestation::Migration),
            Box::new(m20260724_000004_add_ci_cache_sha256::Migration),
            Box::new(m20260726_000001_rename_mirror_table_plural::Migration),
            Box::new(m20260727_000001_clear_plaintext_mirror_passwords::Migration),
            Box::new(m20260727_000002_clear_import_task_auth_tokens::Migration),
            Box::new(m20260728_000001_create_instance_settings::Migration),
            Box::new(m20260728_000002_add_package_file_digests::Migration),
            Box::new(m20260730_000001_repositories_namespace_unique::Migration),
            Box::new(m20260801_000001_create_instance_signing_key::Migration),
            Box::new(m20260802_000001_create_encryption_key_check::Migration),
            Box::new(m20260802_000002_add_user_session_version::Migration),
            Box::new(m20260802_000003_identity_keys_not_blank::Migration),
            Box::new(m20260803_000001_add_sso_provisioning_policy::Migration),
            Box::new(m20260803_000002_add_passkey_credential_rp_id::Migration),
            Box::new(m20260804_000001_add_lfs_publication_lease::Migration),
            Box::new(m20260804_000002_hash_runner_tokens::Migration),
            Box::new(m20260804_000003_rename_webhook_secret_encrypted::Migration),
            Box::new(m20260804_000004_wiki_revision_version_unique::Migration),
            Box::new(m20260804_000005_wiki_page_edit_version::Migration),
            Box::new(m20260804_000006_repo_fts_soft_delete::Migration),
            Box::new(m20260805_000001_issue_labels_single_source::Migration),
            Box::new(m20260805_000002_uploads_outlive_their_uploader::Migration),
            Box::new(m20260805_000003_create_oci_publication_lease::Migration),
            Box::new(m20260805_000004_repo_config_outlives_its_author::Migration),
            Box::new(m20260805_000005_account_owned_rows_follow_their_parent::Migration),
            Box::new(m20260805_000006_clean_serialized_user_grants::Migration),
            Box::new(m20260806_000001_normalize_user_grants::Migration),
            Box::new(m20260806_000002_create_repository_transfer_lease::Migration),
            Box::new(m20260807_000001_create_mirror_sync_lease::Migration),
            Box::new(m20260807_000002_add_user_totp_last_step::Migration),
            Box::new(m20260807_000003_create_webauthn_ceremony_spend::Migration),
            Box::new(m20260808_000001_drop_oci_blob_ref_count::Migration),
            Box::new(m20260808_000002_add_pull_request_ci_approval::Migration),
            Box::new(m20260808_000003_add_pipeline_concurrency_group::Migration),
            Box::new(m20260808_000004_drop_import_task_user_mapping::Migration),
            Box::new(m20260809_000001_package_file_filename_unique::Migration),
            Box::new(m20260810_000001_drop_unused_storage_metadata::Migration),
            Box::new(m20260810_000002_create_npm_dist_tags::Migration),
            Box::new(m20260810_000003_nuget_protocol_version_key::Migration),
            Box::new(m20260810_000004_pypi_protocol_version_key::Migration),
            Box::new(m20260811_000001_cargo_protocol_version_key::Migration),
            Box::new(m20260811_000002_rubygems_protocol_version_key::Migration),
            Box::new(m20260811_000003_helm_protocol_version_key::Migration),
            Box::new(m20260811_000004_composer_protocol_version_key::Migration),
        ]
    }
}
