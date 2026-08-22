//! Every `rg-core` integration test lives in this one binary.
//!
//! The same arithmetic that consolidated `rg-http`'s suite applies here: Cargo
//! builds one test executable per file directly under `tests/`, and each of
//! those statically links the whole `rg-core` graph — `sea-orm`, `rg-db`,
//! `gix`, the crypto stack. The ten files this directory used to hold were ten
//! full links of that graph on every change to anything below them, plus ten
//! copies of it in `target`. As submodules of a single target they link once.
//!
//! Adding a test file means adding it here as a `mod`, otherwise it is not
//! compiled and not run — a file that nothing declares is silently dead.
//!
//! `multi_backend_smoke` deliberately stays its own target. It is `#[ignore]`d
//! opt-in work that needs live PostgreSQL and MySQL servers, and CI drives it by
//! name (`cargo test -p rg-core --test multi_backend_smoke -- --ignored`) in a
//! job that exists only to stand those servers up. Folding it in here would
//! rewrite that gate's invocation for one link's worth of build time.
//!
//! Test names are now prefixed with their module, which is where the old binary
//! name went: `--test wiki_revision_tests` becomes
//! `-E 'test(wiki_revision_tests::)'`. Isolation is unaffected — nextest, which
//! is the gate, already runs every test in its own process.

mod common;

mod code_index_contention_tests;
mod code_index_push_refresh_tests;
mod create_unique_race_tests;
mod encryption_key_check_tests;
mod encryption_rekey_tests;
mod import_wiki_tests;
mod local_number_race_tests;
mod mirror_create_race_tests;
mod status_check_gate_tests;
mod unborn_head_adoption_tests;
mod url_credentials_at_rest_tests;
mod watch_notification_tests;
mod webhook_secret_at_rest_tests;
mod wiki_revision_tests;
