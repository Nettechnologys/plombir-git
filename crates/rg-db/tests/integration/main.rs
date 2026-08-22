//! One test binary for every `rg-db` integration test.
//!
//! Cargo builds a separate executable for every `tests/*.rs`, and each one
//! links the whole crate plus `sea-orm`/`sqlx` again. With 28 files that was 28
//! links and ~2 GB of executables rewritten on every touch of `rg-db` — on a
//! machine whose builds are I/O-bound, the write volume is the cost, and the
//! evicted copies pile up in `target/debug/deps` besides. Declaring the files
//! as modules of one harness makes it one link and one executable, the same
//! shape `rg-http` and `rg-core` already use.
//!
//! Adding a test file means adding a `mod` line here — a file dropped into this
//! directory without one is silently not run.

mod identity_keys_not_blank;
mod issue_label_duplicates;
mod job_assignment_race;
mod job_liveness;
mod legacy_column_upgrade;
mod mfa_backup_code_set_atomicity;
mod mfa_backup_code_single_use;
mod mfa_disable_revokes_backup_codes;
mod mirror_sweep_queue_order;
mod oauth_account_unlink_race;
mod oauth_account_upsert_race;
mod org_permission_predicate_errors;
mod pagination_total_order;
mod passkey_counter_compare_and_swap;
mod password_reset_token_single_use;
mod repositories_namespace_rebuild;
mod runner_retirement_convergence;
mod server_migration_serialization;
mod totp_step_single_use;
mod unique_conflict_classification;
mod upsert_race;
mod user_delete_cascades_repositories;
mod user_delete_cleans_serialized_grants;
mod user_delete_decisions;
mod user_delete_keeps_foreign_config;
mod user_delete_keeps_foreign_uploads;
mod user_delete_refreshes_star_counts;
mod user_grant_writer_integrity;
mod webauthn_ceremony_single_use;
