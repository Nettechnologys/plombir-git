//! One test binary for the `rg-cli` integration tests.
//!
//! Cargo builds a separate executable per `tests/*.rs`, and each of these links
//! the whole CLI — 69 MB apiece, three of them, relinked on every touch of the
//! crate. As modules of one harness it is one link and one executable, matching
//! `rg-db`, `rg-core`, `rg-http` and `rg-ssh`.
//!
//! A file added to this directory without a `mod` line here is silently not run.

mod repo_root_presence;
mod serve_exit_status;
mod sqlite_backup_same_file;
mod sqlite_db_presence;
mod sqlite_migration_offline;
mod sqlite_restore_offline;
mod state_permissions;
