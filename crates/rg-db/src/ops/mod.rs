pub(crate) mod aggregate;
pub mod artifact_ops;
pub mod attachment_ops;
pub mod audit_log_ops;
pub mod board_ops;
pub mod ci_environment_ops;
pub mod ci_retention_ops;
pub mod ci_secret_ops;
pub mod commit_status_ops;
pub mod deploy_key_ops;
pub mod email_confirmation_ops;
pub mod encryption_key_check_ops;
pub mod import_task_ops;
pub mod instance_settings_ops;
pub mod instance_signing_key_ops;
pub mod issue_comment_ops;
pub mod issue_label_ops;
pub mod issue_ops;
pub mod label_ops;
pub mod lfs_lock_ops;
pub mod lfs_object_ops;
pub mod login_log_ops;
pub mod merge_queue_ops;
pub mod mfa_backup_code_ops;
pub mod milestone_ops;
pub mod mirror_ops;
pub mod notification_ops;
pub mod notification_setting_ops;
pub mod npm_dist_tag_ops;
pub mod oauth_account_ops;
pub mod oci_ops;
pub mod org_ops;
pub mod package_file_ops;
pub mod package_ops;
pub mod package_registry_ops;
pub mod package_version_ops;
pub mod passkey_credential_ops;
pub mod password_reset_token_ops;
pub mod pipeline_ops;
pub mod pr_event_ops;
pub mod pr_review_ops;
pub mod pr_reviewer_request_ops;
pub mod protected_branch_ops;
pub mod protected_tag_ops;
pub mod pull_request_ops;
pub mod release_ops;
pub mod repo_collaborator_ops;
pub mod repo_ops;
pub mod repo_star_ops;
pub mod repo_watch_ops;
pub mod review_comment_ops;
pub mod runner_ops;
pub mod ssh_key_ops;
pub mod sso_provider_ops;
pub mod thread_subscription_ops;
pub mod time_entry_ops;
pub mod token_ops;
pub mod user_avatar_ops;
pub mod user_ops;
pub mod webauthn_ceremony_ops;
pub mod webhook_ops;
pub mod wiki_page_ops;
pub mod wiki_revision_ops;

/// How far a credential's `last_used_at` may lag behind its latest use.
///
/// The stamp is observability — "this token was used today" — and writing it
/// on every request made each `git clone`, LFS object or registry layer a write
/// transaction on SQLite's single writer (card_b83b9bc36e3a). A minute is far
/// below anything a person reads off that column.
pub(crate) const LAST_USED_RESOLUTION: chrono::Duration = chrono::Duration::seconds(60);

/// Whether a credential last stamped at `previous` should be stamped at `now`.
pub(crate) fn last_used_is_due(
    previous: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    previous.is_none_or(|stamped| now - stamped >= LAST_USED_RESOLUTION)
}

#[cfg(test)]
mod last_used_tests {
    use super::*;

    #[test]
    fn a_stamp_is_due_once_per_resolution_window() {
        let now = chrono::Utc::now();
        assert!(last_used_is_due(None, now), "never used");
        assert!(!last_used_is_due(Some(now), now), "just stamped");
        assert!(
            !last_used_is_due(Some(now - chrono::Duration::seconds(59)), now),
            "inside the window"
        );
        assert!(
            last_used_is_due(Some(now - LAST_USED_RESOLUTION), now),
            "the window has passed"
        );
        assert!(
            !last_used_is_due(Some(now + chrono::Duration::seconds(5)), now),
            "a stamp from a clock slightly ahead is not rewritten backwards"
        );
    }
}
