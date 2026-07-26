//! Every `rg-http` integration test lives in this one binary.
//!
//! Cargo builds one test executable per file directly under `tests/`, and each
//! of those links the whole server stack — `axum`, `sea-orm`, `rg-core`,
//! `rg-db`, `gix`. Measured on this tree, one such executable is 264 MB, so
//! the 43 files this directory used to hold cost ~11 GB of `target` and 43
//! full links on a cold build. As submodules of a single target they link once.
//!
//! Adding a test file means adding it here as a `mod`, otherwise it is not
//! compiled and not run — a file that nothing declares is silently dead.
//!
//! Test names are now prefixed with their module, which is where the old
//! binary name went: `--test oauth_pkce_tests` becomes
//! `-E 'test(oauth_pkce_tests::)'`. Isolation is unaffected — nextest, which
//! is the gate, already runs every test in its own process.

mod common;

mod admin_org_tests;
mod admin_settings_tests;
mod admin_sso_audit_tests;
mod admin_user_tests;
mod api_tests;
mod artifact_file_tests;
mod attachment_tests;
mod blob_api_tests;
mod board_tests;
mod ci_cache_tests;
mod ci_job_token_tests;
mod ci_oidc_tests;
mod ci_permission_tests;
mod ci_secrets_tag_protection_tests;
mod collaborator_tests;
mod db_outage_status_tests;
mod deploy_key_tests;
mod git_auth_tests;
mod git_http_clone_tests;
mod issue_lookup_failure_status_tests;
mod issue_template_tests;
mod issue_tests;
mod job_websocket_tests;
mod lfs_signed_url_tests;
mod merge_queue_ci_tests;
mod notification_tests;
mod oauth_pkce_tests;
mod oci_permission_tests;
mod openapi_docs_auth_tests;
mod org_tests;
mod package_format_e2e_tests;
mod package_permission_tests;
mod pat_api_tests;
mod pr_lookup_failure_status_tests;
mod pr_merge_strategy_tests;
mod pr_permission_tests;
mod release_attestation_tests;
mod release_tests;
mod review_lookup_failure_status_tests;
mod runner_auth_tests;
mod runner_workspace_tests;
mod ssh_key_tests;
mod tail_lookup_failure_status_tests;
mod time_tracking_tests;
mod upload_failure_status_tests;
mod webhook_external_hmac_tests;
mod wiki_tests;
