//! Every server-database test in this binary holds [`SHARED_SERVER_DB`].
//!
//! The `#[ignore]`d tests that read `PLOMBIR_GIT_TEST_DATABASE_URL` all run
//! against one disposable database, and run side by side they collide: lock
//! waits and metadata locks that a request answers as 503 (card_694fc60e94a9).
//! The lock serialises them only if every one of them takes it. When it went
//! in, four of the ten did not, because nothing asked them to; this guard is
//! what asks the next one.
//!
//! A test counts as a server-database test when its own body reads the URL, or
//! when it calls a helper of its file that does (`server_database()`).
//!
//! [`SHARED_SERVER_DB`]: crate::common::SHARED_SERVER_DB

use std::fs;
use std::path::Path;

use crate::common::source_scan::{
    calls, functions, production_rust_code_only, rust_code_only, rust_files,
};

/// The literal a server-database test reads its URL from, quotes included.
const DATABASE_URL_VAR: &str = "\"PLOMBIR_GIT_TEST_DATABASE_URL\"";

/// The server-database tests this binary held when the guard was written. A
/// census that finds fewer has lost its reader, not its tests.
const KNOWN_SERVER_DATABASE_TESTS: usize = 10;

/// Whether `body` reads the server-database URL in code. A comment or a
/// message naming the variable does not count: the call is found in the code
/// view, and only its argument is read from the source.
fn reads_database_url(body: &str) -> bool {
    let code = rust_code_only(body);
    code.match_indices("env::var(").any(|(at, call)| {
        body[at + call.len()..]
            .trim_start()
            .starts_with(DATABASE_URL_VAR)
    })
}

/// Whether the `fn` declared on 1-based `line` carries a test attribute, read
/// from the attribute lines directly above it in the code view.
fn is_test(code_lines: &[&str], line: usize) -> bool {
    code_lines[..line - 1]
        .iter()
        .rev()
        .map(|line| line.trim())
        .take_while(|line| line.starts_with("#["))
        .any(|attribute| attribute.starts_with("#[tokio::test") || attribute == "#[test]")
}

#[test]
fn every_server_database_test_holds_the_shared_lock() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/integration");
    let mut files = Vec::new();
    rust_files(&dir, &mut files);
    files.sort();

    let mut found = Vec::new();
    let mut unguarded = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file).expect("read test source");
        let code = production_rust_code_only(&text);
        let code_lines: Vec<&str> = code.lines().collect();
        let fns = functions(&text);
        let openers: Vec<&str> = fns
            .iter()
            .filter(|f| reads_database_url(&f.body))
            .map(|f| f.name.as_str())
            .collect();
        for f in &fns {
            let server =
                reads_database_url(&f.body) || openers.iter().any(|opener| calls(&f.body, opener));
            if !server || !is_test(&code_lines, f.line) {
                continue;
            }
            let at = format!(
                "{}:{} {}",
                file.strip_prefix(&dir).unwrap().display(),
                f.line,
                f.name
            );
            if !rust_code_only(&f.body).contains("SHARED_SERVER_DB.lock()") {
                unguarded.push(at.clone());
            }
            found.push(at);
        }
    }

    assert!(
        unguarded.is_empty(),
        "server-database tests that run without `SHARED_SERVER_DB.lock().await` \
         (they share one database with the rest): {unguarded:#?}"
    );
    assert!(
        found.len() >= KNOWN_SERVER_DATABASE_TESTS,
        "found {} server-database tests, expected at least {KNOWN_SERVER_DATABASE_TESTS}; \
         the reader has gone blind: {found:#?}",
        found.len()
    );
}

#[test]
fn only_a_call_reading_the_variable_makes_a_server_database_body() {
    assert!(reads_database_url(
        "fn a() {\n    let url = std::env::var(\"PLOMBIR_GIT_TEST_DATABASE_URL\").unwrap();\n}\n"
    ));
    assert!(!reads_database_url(
        "fn b() {\n    // std::env::var(\"PLOMBIR_GIT_TEST_DATABASE_URL\")\n}\n"
    ));
    assert!(!reads_database_url(
        "fn c() {\n    let _ = \"PLOMBIR_GIT_TEST_DATABASE_URL must be set\";\n}\n"
    ));
    assert!(!reads_database_url(
        "fn d() {\n    let _ = std::env::var(\"PLOMBIR_GIT_OTHER\");\n}\n"
    ));
}
