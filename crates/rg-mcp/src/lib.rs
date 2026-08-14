//!
//! `rg-mcp` - ForgeKeep MCP Server
//!
//! MCP (Model Context Protocol) server that exposes ForgeKeep
//! repository data as Tools and Resources to AI agents.
//!
//! Supported transport:
//! - **stdio**: run as a subprocess of an MCP-capable agent.
//!
//! HTTP SSE transport is intentionally not advertised until implemented.

pub mod client;
pub mod error;
pub mod protocol;
pub mod resources;
pub mod tools;

// Re-export for convenience
pub use error::{Error, Result};

/// The ForgeKeep API this server talks to when `FORGEKEEP_URL` is unset.
///
/// A named constant rather than a literal inside the resolve, because the same
/// address is restated on both pages that describe this binary — the `//!`
/// table of `main.rs` and the README's MCP section — and a value with no name
/// is one no check can bind them to. The tests at the bottom of this file are
/// what hold the three together.
const DEFAULT_API_BASE: &str = "http://localhost:8080";

/// Request timeout for MCP → ForgeKeep API calls (whole request, incl. body),
/// so a slow/hanging server can't pin a tool call forever.
const HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Connect timeout (TCP + TLS handshake only).
const HTTP_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Keep the PAT on the exact origin of each initiating request. Reqwest's
/// default sensitive-header stripping compares host and port but not scheme,
/// so it is not sufficient for an HTTPS-to-HTTP redirect on the same socket.
fn same_origin_redirect_policy() -> reqwest::redirect::Policy {
    let default = reqwest::redirect::Policy::default();
    reqwest::redirect::Policy::custom(move |attempt| {
        let stays_on_origin = attempt.previous().first().is_some_and(|initial| {
            initial.scheme() == attempt.url().scheme()
                && initial.host_str() == attempt.url().host_str()
                && initial.port_or_known_default() == attempt.url().port_or_known_default()
        });
        if stays_on_origin {
            default.redirect(attempt)
        } else {
            attempt.stop()
        }
    })
}

/// Build the `reqwest::Client` used for every ForgeKeep API call: the static
/// `Bearer` header plus the outbound request + connect timeouts.
///
/// Built once and cached on [`AppState`] — cloning a `reqwest::Client` is a
/// cheap `Arc` bump that shares one connection pool + TLS config, so tool calls
/// reuse keep-alive connections instead of standing up a fresh pool each time.
fn build_http_client(pat: &str) -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    if !pat.is_empty() {
        let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {pat}"))
            .unwrap_or(reqwest::header::HeaderValue::from_static(""));
        headers.insert(reqwest::header::AUTHORIZATION, value);
    }
    // unwrap is acceptable here — build() only fails if native TLS is
    // entirely unavailable, which means the system is fundamentally broken
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(HTTP_CONNECT_TIMEOUT)
        .redirect(same_origin_redirect_policy())
        .default_headers(headers)
        .build()
        .expect("reqwest::Client::build() failed: no native TLS backend available")
}

/// ForgeKeep API base URL + PAT cache.
///
/// Constructed once at startup from environment variables.
#[derive(Clone)]
pub struct AppState {
    pub api_base: String,
    pub pat: String,
    /// Pre-built, reusable API client (Bearer header + timeouts baked in).
    http_client: reqwest::Client,
}

impl AppState {
    pub fn from_env() -> Result<Self> {
        let api_base =
            std::env::var("FORGEKEEP_URL").unwrap_or_else(|_| DEFAULT_API_BASE.to_string());
        let pat = std::env::var("FORGEKEEP_PAT").unwrap_or_default();

        if pat.is_empty() {
            tracing::warn!("FORGEKEEP_PAT not set – API calls may fail");
        }

        Ok(Self::new(api_base, pat))
    }

    /// Construct from an explicit base URL + PAT, building the cached HTTP
    /// client once. The `Bearer` header depends only on `pat`, which is fixed
    /// for the lifetime of an `AppState`, so the client never needs rebuilding.
    pub fn new(api_base: String, pat: String) -> Self {
        let http_client = build_http_client(&pat);
        Self {
            api_base,
            pat,
            http_client,
        }
    }

    /// Cheap clone of the shared `reqwest::Client` (Bearer header + timeouts).
    pub fn http_client(&self) -> reqwest::Client {
        self.http_client.clone()
    }
}

#[cfg(test)]
mod redirect_tests {
    use super::build_http_client;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_headers(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = stream.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    async fn write_response(stream: &mut TcpStream, status: &str, headers: &str) {
        let response =
            format!("HTTP/1.1 {status}\r\n{headers}Content-Length: 0\r\nConnection: close\r\n\r\n");
        stream.write_all(response.as_bytes()).await.unwrap();
    }

    #[derive(Clone, Copy, Debug)]
    enum OriginChange {
        Scheme,
        Host,
        Port,
    }

    async fn assert_origin_change_is_stopped(
        client: &reqwest::Client,
        expected_authorization: &str,
        change: OriginChange,
    ) {
        let source = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_address = source.local_addr().unwrap();
        let sink = if matches!(change, OriginChange::Port) {
            Some(TcpListener::bind("127.0.0.1:0").await.unwrap())
        } else {
            None
        };
        let sink_address = sink.as_ref().map(|listener| listener.local_addr().unwrap());
        let location = match change {
            OriginChange::Scheme => {
                format!("https://127.0.0.1:{}/changed-scheme", source_address.port())
            }
            OriginChange::Host => {
                format!("http://127.0.0.1:{}/changed-host", source_address.port())
            }
            OriginChange::Port => format!("http://{}/changed-port", sink_address.unwrap()),
        };
        let initial_host = if matches!(change, OriginChange::Host) {
            "localhost"
        } else {
            "127.0.0.1"
        };
        let initial_url = format!("http://{initial_host}:{}/start", source_address.port());

        let sink_task = sink.map(|sink| {
            tokio::spawn(async move {
                matches!(
                    tokio::time::timeout(std::time::Duration::from_secs(1), sink.accept()).await,
                    Ok(Ok(_))
                )
            })
        });
        let source_task = tokio::spawn(async move {
            let (mut first, _) = source.accept().await.unwrap();
            let first_request = read_headers(&mut first).await;
            write_response(
                &mut first,
                "302 Found",
                &format!("Location: {location}\r\n"),
            )
            .await;
            let same_listener_followed = if matches!(change, OriginChange::Port) {
                false
            } else {
                matches!(
                    tokio::time::timeout(std::time::Duration::from_secs(1), source.accept()).await,
                    Ok(Ok(_))
                )
            };
            (first_request, same_listener_followed)
        });

        let response = client
            .get(initial_url)
            .send()
            .await
            .expect("the cross-origin redirect must be returned, not followed");
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);

        let (first_request, same_listener_followed) = source_task.await.unwrap();
        assert!(
            first_request
                .to_ascii_lowercase()
                .contains(expected_authorization),
            "baseline request did not carry its credential: {first_request}"
        );
        let separate_sink_followed = match sink_task {
            Some(task) => task.await.unwrap(),
            None => false,
        };
        assert!(
            !same_listener_followed && !separate_sink_followed,
            "{change:?}-changing destination was contacted"
        );
    }

    #[tokio::test]
    async fn mcp_client_stops_every_origin_change_before_sending_the_pat() {
        let client = build_http_client("mcp-pat");
        for change in [OriginChange::Scheme, OriginChange::Host, OriginChange::Port] {
            assert_origin_change_is_stopped(&client, "authorization: bearer mcp-pat", change).await;
        }
    }

    #[tokio::test]
    async fn mcp_client_keeps_same_origin_redirects_and_the_pat() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let first_request = read_headers(&mut first).await;
            write_response(
                &mut first,
                "302 Found",
                &format!("Location: http://{address}/renamed\r\n"),
            )
            .await;
            let (mut second, _) = listener.accept().await.unwrap();
            let second_request = read_headers(&mut second).await;
            write_response(&mut second, "204 No Content", "").await;
            (first_request, second_request)
        });

        let response = build_http_client("mcp-pat")
            .get(format!("http://{address}/start"))
            .send()
            .await
            .expect("same-origin redirect");
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);

        let (first, second) = server.await.unwrap();
        for request in [first, second] {
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer mcp-pat"),
                "same-origin request lost the MCP PAT: {request}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// The environment against the pages that describe it.
//
// `forgekeep-mcp` takes no flags at all — `--help` states nothing, and the
// whole configuration is two environment variables. So the only description of
// them is prose, and it exists twice: the `//!` table of `main.rs`, and the
// README section whoever wires this binary into an agent reads while writing
// the `mcpServers` block.
//
// The *names* on those pages are already policed, by the workspace-wide census
// in `rg-cli` — a variable the code reads that no operator document names fails
// there. What that census never looks at is the column beside the name. Until
// `DEFAULT_API_BASE` existed there was nothing it could have looked at either:
// the address was a literal inside `unwrap_or_else`, so the two pages restated
// a value that had no name, and the only thing holding all three equal was that
// nobody had changed one of them yet.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod documented_environment_tests {
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    /// This crate's own doc-comment table.
    ///
    /// Read as text rather than through its items: `main.rs` is compiled into
    /// the `forgekeep-mcp` *binary* and this file into the library, so the two
    /// never see each other — and it is the prose that has to be checked
    /// anyway. `include_str!` makes a moved file break the build instead of
    /// quietly skipping the checks.
    const CRATE_DOC: (&str, &str) = ("crates/rg-mcp/src/main.rs", include_str!("main.rs"));

    /// The page an operator reaches for before running anything.
    const README: (&str, &str) = ("README.md", include_str!("../../../README.md"));

    /// The heading that opens the crate doc's environment table.
    const CRATE_DOC_HEADING: &str = "//! # Environment";

    /// The heading that opens the README's half of this binary. Everything up
    /// to the next `## ` heading is the section these checks read.
    const MCP_SECTION: &str = "## MCP server (`forgekeep-mcp`)";

    /// How both pages spell "this variable has no default at all".
    const NO_DEFAULT: &str = "_(none)_";

    /// The body of the page that follows `heading`, up to the next `## `.
    fn doc_section<'a>((name, content): (&str, &'a str), heading: &str) -> &'a str {
        let (_, rest) = content.split_once(heading).unwrap_or_else(|| {
            panic!(
                "{name} no longer has a `{heading}` section — that section is one of only two \
                 descriptions of this binary's environment, and the other cannot be checked \
                 against a page that is gone"
            )
        });

        match rest.split_once("\n## ") {
            Some((section, _)) => section,
            None => rest,
        }
    }

    /// Both pages, each cut down to the fragment carrying its table.
    fn documented_pages() -> Vec<(&'static str, &'static str)> {
        vec![
            (CRATE_DOC.0, doc_section(CRATE_DOC, CRATE_DOC_HEADING)),
            (README.0, doc_section(README, MCP_SECTION)),
        ]
    }

    /// The row of a `| Variable | Default | … |` table whose first cell names
    /// `variable`, with the `//!` of a doc-comment table stripped.
    ///
    /// The first cell has to *equal* the name: `FORGEKEEP_URL` must not be
    /// answered by a row describing `FORGEKEEP_URL_FILE`.
    fn table_row<'a>(section: &'a str, variable: &str) -> Option<&'a str> {
        let named = format!("`{variable}`");

        section
            .lines()
            .map(|line| line.trim_start().trim_start_matches("//!").trim_start())
            .find(|line| {
                line.strip_prefix('|')
                    .and_then(|row| row.split('|').next())
                    .is_some_and(|first| first.trim() == named)
            })
    }

    /// What a row states in its `Default` column.
    #[derive(Debug, PartialEq, Eq)]
    enum Stated<'a> {
        /// A literal value, spelled in backticks.
        Value(&'a str),
        /// The column says the variable has no default.
        Absent,
        /// Prose: neither a backticked value nor the "no default" spelling.
        /// Kept apart from the two above so a sentence cannot pass for either.
        Prose(&'a str),
    }

    /// The `Default` column of `row`.
    fn stated_default(row: &str) -> Option<Stated<'_>> {
        let cell = row.strip_prefix('|')?.split('|').nth(1)?.trim();
        if cell == NO_DEFAULT {
            return Some(Stated::Absent);
        }

        match cell
            .split_once('`')
            .and_then(|(_, rest)| rest.split_once('`'))
        {
            Some((value, _)) => Some(Stated::Value(value)),
            None => Some(Stated::Prose(cell)),
        }
    }

    /// What the production resolve falls back to when a variable is unset.
    #[derive(Debug, PartialEq, Eq)]
    enum Fallback<'a> {
        /// `unwrap_or_else(|_| NAME.to_string())` — a named constant, the only
        /// shape a page can be bound to.
        Constant(&'a str),
        /// `unwrap_or_default()` — the empty string, i.e. no default.
        Empty,
        /// `unwrap_or_else(|_| "…".to_string())` — the value written into the
        /// resolve itself. This is the shape this whole module exists to stop
        /// coming back.
        Literal(&'a str),
        /// Anything else, carrying the rest of the statement so the failure
        /// names the shape: read it before believing it.
        Unknown(&'a str),
    }

    /// How `source` resolves `variable`, if it resolves it at all.
    fn env_fallback<'a>(source: &'a str, variable: &str) -> Option<Fallback<'a>> {
        let call = format!("env::var(\"{variable}\")");
        let (_, rest) = source.split_once(&call)?;
        let (statement, _) = rest.split_once(';')?;

        if statement.contains(".unwrap_or_default()") {
            return Some(Fallback::Empty);
        }
        let Some((_, tail)) = statement.split_once(".unwrap_or_else(|_| ") else {
            return Some(Fallback::Unknown(statement.trim()));
        };
        if let Some((literal, _)) = tail.strip_prefix('"').and_then(|rest| rest.split_once('"')) {
            return Some(Fallback::Literal(literal));
        }

        let end = tail
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(tail.len());
        Some(Fallback::Constant(&tail[..end]))
    }

    /// A variable both pages describe, bound to what the resolve really does.
    struct DocumentedVariable {
        /// The variable an operator sets.
        name: &'static str,
        /// The `DEFAULT_*` constant the resolve must fall back to, with its
        /// value read *from* the constant rather than copied beside it — so
        /// renaming it breaks the build and changing it fails every check
        /// below. `None` states that the variable deliberately has no default.
        default: Option<(&'static str, String)>,
    }

    /// The pairing table. The variable spellings have to be written out — no
    /// rule derives `DEFAULT_API_BASE` from `FORGEKEEP_URL` — but no value is.
    fn documented_variables() -> Vec<DocumentedVariable> {
        macro_rules! from_constant {
            ($konst:ident) => {
                Some((stringify!($konst), super::$konst.to_string()))
            };
        }

        vec![
            DocumentedVariable {
                name: "FORGEKEEP_URL",
                default: from_constant!(DEFAULT_API_BASE),
            },
            DocumentedVariable {
                name: "FORGEKEEP_PAT",
                default: None,
            },
        ]
    }

    /// Built-in defaults of this crate that no operator page states, each with
    /// the reason. Empty today; the list exists so the next unpaired default is
    /// a decision someone wrote down rather than one that slipped past the
    /// census below.
    const DEFAULTS_NOT_OPERATOR_FACING: [(&str, &str); 0] = [];

    /// The `const DEFAULT_*` names `source` declares, whatever their
    /// visibility: a default that is private today is still a default an
    /// operator meets. Read off the declarations rather than listed beside
    /// them — a constant added to the crate joins the census by existing.
    fn declared_default_constants(source: &str) -> BTreeSet<&str> {
        source
            .lines()
            .map(str::trim_start)
            .map(|line| line.strip_prefix("pub(crate) ").unwrap_or(line))
            .map(|line| line.strip_prefix("pub ").unwrap_or(line))
            .filter_map(|line| line.strip_prefix("const "))
            .filter_map(|rest| rest.split_once(':'))
            .map(|(name, _)| name.trim())
            .filter(|name| name.starts_with("DEFAULT_"))
            .collect()
    }

    /// Every production `.rs` file of this crate, with its `#[cfg(test)]` tail
    /// removed.
    ///
    /// A directory walk rather than a list of `include_str!`s: the question the
    /// census asks is whether a default exists *anywhere* in the crate, and a
    /// fixed list would have to be edited whenever one moves — which is the
    /// remembering these checks exist to remove. The `#[cfg(test)]` cut is what
    /// keeps it honest: a constant declared in a fixture is not a default any
    /// operator can meet.
    fn production_sources() -> Vec<(PathBuf, String)> {
        let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = Vec::new();
        let mut pending = vec![src];

        while let Some(dir) = pending.pop() {
            let entries = std::fs::read_dir(&dir)
                .unwrap_or_else(|error| panic!("{}: {error}", dir.display()));

            for entry in entries {
                let path = entry.expect("a readable directory entry").path();
                let name = path.file_name().unwrap_or_default().to_string_lossy();

                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if !name.ends_with(".rs") {
                    continue;
                }

                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                let production = match text.split_once("\n#[cfg(test)]\n") {
                    Some((production, _)) => production.to_string(),
                    None => text,
                };
                sources.push((path, production));
            }
        }

        sources
    }

    /// The production half of this file — where the environment is resolved.
    fn production_lib_source() -> &'static str {
        let production = include_str!("lib.rs")
            .split_once("\n#[cfg(test)]\n")
            .map(|(production, _)| production)
            .expect("lib.rs must keep its test modules behind #[cfg(test)]");

        assert!(
            production.contains("pub fn from_env()"),
            "the `#[cfg(test)]` cut now removes `from_env` itself, so the checks below would \
             read no resolve at all — move the production code above the first test module"
        );
        production
    }

    /// The address an operator's agent talks to when the environment says
    /// nothing. Both pages restate it, and neither is generated: what they
    /// state has to be asserted against the constant the binary really uses.
    #[test]
    fn every_default_the_mcp_pages_state_is_the_one_the_binary_falls_back_to() {
        // The readers have to be able to answer "no" before their "yes" is
        // worth anything.
        assert_eq!(
            table_row(
                "//! | `FORGEKEEP_URL` | `http://probe` | base |",
                "FORGEKEEP_URL"
            )
            .and_then(stated_default),
            Some(Stated::Value("http://probe")),
            "the row reader does not see through the doc-comment table's `//!` prefix"
        );
        assert!(
            table_row(
                "| `FORGEKEEP_URL_FILE` | `http://probe` | base |",
                "FORGEKEEP_URL"
            )
            .is_none(),
            "the row reader answers `FORGEKEEP_URL` with a longer name's row, so a page \
             documenting neither could pass for one documenting both"
        );
        assert_eq!(
            table_row("| `FORGEKEEP_PAT` | _(none)_ | token |", "FORGEKEEP_PAT")
                .and_then(stated_default),
            Some(Stated::Absent),
            "the cell reader does not recognise how both pages spell \"no default\""
        );
        assert_eq!(
            table_row(
                "| `FORGEKEEP_PAT` | the machine's hostname | t |",
                "FORGEKEEP_PAT"
            )
            .and_then(stated_default),
            Some(Stated::Prose("the machine's hostname")),
            "the cell reader invents a literal default out of prose that only describes a \
             behaviour"
        );

        let variables = documented_variables();
        let mut checked = 0;

        for (name, section) in documented_pages() {
            for variable in &variables {
                let row = table_row(section, variable.name).unwrap_or_else(|| {
                    panic!(
                        "{name}: the environment table has no `{}` row — that table is where \
                         the person wiring `forgekeep-mcp` into an agent looks the variable \
                         up, and the source is the only place left without it",
                        variable.name
                    )
                });
                let stated = stated_default(row).unwrap_or_else(|| {
                    panic!(
                        "{name}: the `{}` row has no `Default` column",
                        variable.name
                    )
                });

                match (&variable.default, stated) {
                    (Some((konst, value)), Stated::Value(shown)) => assert_eq!(
                        shown,
                        value.as_str(),
                        "{name}: the `{}` row states the default `{shown}`, but `{konst}` — \
                         the constant the resolve actually falls back to — is `{value}`. An \
                         agent configured from this page then talks to an address the binary \
                         will not use, and the failure surfaces as an unreachable API, not as \
                         a wrong page",
                        variable.name
                    ),
                    (None, Stated::Absent) => {}
                    (Some((konst, value)), other) => panic!(
                        "{name}: the `Default` column of the `{}` row reads {other:?}, while \
                         the resolve falls back to `{konst}` = `{value}` — state that value \
                         in backticks so this check can hold the page to it",
                        variable.name
                    ),
                    (None, other) => panic!(
                        "{name}: the `Default` column of the `{}` row reads {other:?}, but \
                         nothing in the resolve produces a default for it — the page promises \
                         a value the binary has not got. `{NO_DEFAULT}` is how both pages \
                         spell the absence",
                        variable.name
                    ),
                }
                checked += 1;
            }
        }

        // A floor, not a count: two variables on two pages. A scanner that
        // stopped matching would otherwise read as agreement.
        assert!(
            checked >= 4,
            "only {checked} table rows were matched against the resolve — the page scanner \
             has drifted away from how the tables are written"
        );
    }

    /// The other half of the same contract, and the one the pages cannot state:
    /// that the value they name is reached through a *name*. A fallback written
    /// into `unwrap_or_else` as a literal is unreachable from any check — which
    /// is exactly how this crate's address stood for three copies with nothing
    /// holding them equal.
    #[test]
    fn every_environment_default_the_mcp_resolve_uses_comes_from_a_named_constant() {
        const PROBE: &str = "let a = std::env::var(\"PROBE_CONST\")\n\
             .unwrap_or_else(|_| DEFAULT_PROBE.to_string());\n\
             let b = std::env::var(\"PROBE_EMPTY\").unwrap_or_default();\n\
             let c = std::env::var(\"PROBE_LITERAL\").unwrap_or_else(|_| \"http://probe\".to_string());\n\
             let d = std::env::var(\"PROBE_OTHER\").ok();\n";

        assert_eq!(
            env_fallback(PROBE, "PROBE_CONST"),
            Some(Fallback::Constant("DEFAULT_PROBE")),
            "the resolve reader does not recognise a fallback that comes from a constant"
        );
        assert_eq!(
            env_fallback(PROBE, "PROBE_EMPTY"),
            Some(Fallback::Empty),
            "the resolve reader does not recognise `unwrap_or_default()` as the absence of a \
             default"
        );
        assert_eq!(
            env_fallback(PROBE, "PROBE_LITERAL"),
            Some(Fallback::Literal("http://probe")),
            "the resolve reader takes a literal written into the resolve for a named \
             constant, so the one shape these checks exist to catch would pass"
        );
        assert_eq!(
            env_fallback(PROBE, "PROBE_OTHER"),
            Some(Fallback::Unknown(".ok()")),
            "the resolve reader guesses at a shape it has not been taught to read"
        );
        assert_eq!(
            env_fallback(PROBE, "PROBE_ABSENT"),
            None,
            "the resolve reader claims to read a variable nothing resolves"
        );

        let source = production_lib_source();

        for variable in &documented_variables() {
            let fallback = env_fallback(source, variable.name).unwrap_or_else(|| {
                panic!(
                    "no production source of `rg-mcp` resolves `{}`, yet both pages document \
                     it — an operator sets a variable that does nothing",
                    variable.name
                )
            });

            match (&variable.default, fallback) {
                (Some((konst, _)), Fallback::Constant(used)) => assert_eq!(
                    used, *konst,
                    "`{}` falls back to `{used}`, but the pairing table binds the pages to \
                     `{konst}` — the tables are then checked against a constant the resolve \
                     no longer uses",
                    variable.name
                ),
                (None, Fallback::Empty) => {}
                (_, Fallback::Literal(value)) => panic!(
                    "the fallback `{value}` for `{}` is written into the resolve itself. Both \
                     pages restate that value, and a literal has no name for them to be bound \
                     to — give it a `const DEFAULT_*` beside `from_env` and pair it in \
                     documented_variables()",
                    variable.name
                ),
                (Some((konst, value)), other) => panic!(
                    "`{}` resolves as {other:?}, but the pages are held to `{konst}` = \
                     `{value}` — pair the variable with what it now falls back to, or restore \
                     the constant",
                    variable.name
                ),
                (None, other) => panic!(
                    "`{}` resolves as {other:?}, and both pages state `{NO_DEFAULT}` for it — \
                     a default the code grew and the pages never learned about",
                    variable.name
                ),
            }
        }
    }

    /// The census, in both directions: a default this crate declares that no
    /// page states, and a row pairing a constant that no longer exists.
    #[test]
    fn every_default_constant_this_crate_declares_is_stated_on_an_operator_page() {
        assert_eq!(
            declared_default_constants(
                "pub const DEFAULT_X: &str = \"1\";\n    const DEFAULT_Y: u8 = 2;\n\
                 pub(crate) const DEFAULT_Z: u8 = 3;\nconst OTHER: u8 = 4;\n"
            ),
            BTreeSet::from(["DEFAULT_X", "DEFAULT_Y", "DEFAULT_Z"]),
            "the declaration scan does not read `const DEFAULT_*` the way this crate writes \
             them — a private one would escape the census entirely"
        );

        let sources = production_sources();
        assert!(
            sources.len() >= 5,
            "the crate walk found only {} production sources — it is looking in the wrong \
             place, and an empty census agrees with anything",
            sources.len()
        );

        let mut declared: BTreeSet<String> = BTreeSet::new();
        for (_, text) in &sources {
            declared.extend(
                declared_default_constants(text)
                    .into_iter()
                    .map(str::to_owned),
            );
        }

        let variables = documented_variables();
        let paired: BTreeSet<&str> = variables
            .iter()
            .filter_map(|variable| variable.default.as_ref().map(|(konst, _)| *konst))
            .collect();
        assert!(
            !paired.is_empty(),
            "documented_variables() pairs no constant at all, so nothing below is checked"
        );

        for name in &declared {
            assert!(
                paired.contains(name.as_str())
                    || DEFAULTS_NOT_OPERATOR_FACING
                        .iter()
                        .any(|(excused, _)| excused == name),
                "`{name}` is a built-in default of `rg-mcp` that no row of \
                 documented_variables() pairs with a variable — pair it with the variable \
                 whose table row states it, or name it in DEFAULTS_NOT_OPERATOR_FACING with \
                 the reason no operator ever meets it"
            );
        }

        // Renaming a paired constant breaks the build, but *moving* one out of
        // the crate would not: it would simply leave the census.
        for name in &paired {
            assert!(
                declared.contains(*name),
                "documented_variables() pairs `{name}`, which no production source of this \
                 crate declares any more — the pages would then be held to a constant that \
                 lives somewhere the census cannot see"
            );
        }

        for (excused, reason) in DEFAULTS_NOT_OPERATOR_FACING {
            assert!(
                !reason.is_empty(),
                "`{excused}` is excused from the census without a reason"
            );
            assert!(
                declared.contains(excused),
                "DEFAULTS_NOT_OPERATOR_FACING still excuses `{excused}`, which this crate no \
                 longer declares — drop the entry so the list keeps meaning something"
            );
        }
    }
}
