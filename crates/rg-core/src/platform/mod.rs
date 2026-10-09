//! Where the tree talks to the operating system — **not** a portability layer.
//!
//! It was written as one: three modules of `#[cfg(unix)] … #[cfg(windows)] …`
//! pairs whose doc comments promised that each call "behaves sensibly on
//! Windows". Nothing ever called them. `platform::process` had six public
//! functions and not one caller anywhere in the workspace; `platform::path` had
//! five and one caller; `platform::fs` had thirteen and the live half was the
//! error-message family, not the permission family. Meanwhile some fifteen call
//! sites across `rg-ssh`, `rg-cli`, `rg-runner`, `rg-core` and `rg-http` write
//! `#[cfg(unix)] use std::os::unix::fs::PermissionsExt` by hand — so the
//! promise was not merely unused, it was contradicted everywhere it mattered
//! (card_af921becd2a0).
//!
//! **The decision, so the next reader does not have to guess it.** The server
//! is Unix-only, and this module stops pretending otherwise. The evidence is
//! not a preference: CI runs `ubuntu-latest` on every job and has no Windows
//! target, the shipped deployment is a Linux container, and the tree's own
//! Unix-isms (uid/gid diagnostics, `PermissionsExt`, systemd, ssh) are load
//! bearing, not incidental. Routing those fifteen call sites through a layer
//! would have been the other honest answer, but it buys portability nobody is
//! paying for and no CI job could keep true.
//!
//! What is left is what the tree actually uses:
//!
//! * [`fs`] — the vocabulary for reporting a path operation that failed:
//!   `path_error` / `describe_path_error`, the `*_HINT` remedies, the
//!   `ownership_hint` uid/gid diagnostic (which is Unix-specific *on purpose*)
//!   and the `discard_*` helpers for cleanup whose failure must be logged
//!   rather than propagated.
//! * [`path`] — `validate_repo_path`, the traversal check every git entry point
//!   runs on an owner/repo component, and `validate_upload_filename`, the one
//!   rule release assets and attachments share for a name they will later
//!   join onto a server-owned directory.
//!
//! `scripts/db-ops-consumer-contract-check.mjs` covers this directory, so a
//! public function added here without a caller fails a gate instead of settling
//! in for another few months.

pub mod fs;
pub mod path;

pub use fs::{discard_dir, discard_dir_async, discard_file, discard_file_async};
pub use path::validate_repo_path;
