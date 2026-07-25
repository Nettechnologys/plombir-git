# Contributing to ForgeKeep

Thanks for your interest in ForgeKeep! This guide covers the development
setup, crate boundaries, coding conventions, and the common workflows.

---

## Table of contents

- [Development environment](#development-environment)
- [Project structure & crate responsibilities](#project-structure--crate-responsibilities)
- [Coding conventions](#coding-conventions)
- [Commit conventions](#commit-conventions)
- [Testing](#testing)
- [Branching & PRs](#branching--prs)

---

## Development environment

### Required tools

```bash
# Rust stable (1.95+)
rustup update stable

# Formatting & lint
cargo fmt
cargo clippy

# System dependency: git (used for a few pack / diff operations)
which git
```

### Recommended tools

```bash
cargo install cargo-watch   # incremental rebuilds on change
cargo install cargo-audit   # dependency vulnerability audit
cargo tree                  # inspect the dependency graph
```

### First-time setup

```bash
git clone https://github.com/Yahook/ForgeKeep.git
cd ForgeKeep
cargo build                 # verify dependencies fetch and compile

# Generate a test SSH host key (one-off)
ssh-keygen -t ed25519 -f ./forgekeep_host_key -N ""
```

---

## Project structure & crate responsibilities

### Dependency graph

```
rg-cli
  ├── rg-core ──> rg-db, rg-git
  ├── rg-git
  ├── rg-ssh ──> rg-git, rg-core, rg-db
  ├── rg-http ──> rg-git, rg-core, rg-db
  ├── rg-ci ──> rg-core, rg-db, rg-git
  ├── rg-runner   (only so `forgekeep runner` can delegate to the real agent)
  └── rg-db

rg-runner ──> (HTTP client of the rg-http runner API)
rg-mcp    ──> (HTTP client of the rg-http REST API)
```

Library crates keep a neutral `rg-*` prefix. The user-facing binaries are
`forgekeep` (`rg-cli`), `forgekeep-runner` (`rg-runner`), and `forgekeep-mcp`
(`rg-mcp`).

### Crate boundary rules

#### `rg-git` — Git protocol layer (protocol only, no business logic)

**Allowed:** pkt-line / sideband encode-decode; upload-pack / receive-pack
handling; invoking the system `git` for pack-objects / index-pack / update-ref /
for-each-ref; path handling.

**Forbidden:** depending on `rg-core` / `rg-db` / `rg-http` / `rg-ssh`;
authentication logic; direct database access.

#### `rg-ssh` — SSH transport

**Allowed:** the russh server; routing `exec_request` to `rg-git`; SSH auth
(public-key/password lookups via `rg-core::auth` + `rg-db`).

**Forbidden:** parsing the Git wire protocol (delegate to `rg-git`); direct
database access.

#### `rg-http` — HTTP transport

**Allowed:** axum routing; Git Smart HTTP endpoints; REST API (users, repos,
issues, PRs, wiki, LFS, webhooks, CI, registries); middleware (auth, CORS, rate
limiting).

**Forbidden:** parsing the Git wire protocol (delegate to `rg-git`); business
logic (delegate to `rg-core`).

#### `rg-core` — core business logic

**Allowed:** user/repo/issue/PR/wiki/hook logic; authentication & authorization
(argon2, JWT); permission checks.

**Forbidden:** HTTP/SSH protocol details; Git wire-protocol implementation.

#### `rg-db` — database layer

**Allowed:** SeaORM entities; migrations; CRUD operations.

**Forbidden:** business logic; HTTP/SSH layer code.

**Migration rules (please read — these have bitten us):**
- A `#[derive(Iden)] enum Foo { Table }` produces the **singular** name `foo`,
  while entities use the plural `#[sea_orm(table_name = "foos")]`. When adding a
  table, set the name explicitly (`#[sea_orm(iden = "foos")]` or raw SQL) so it
  matches the entity's `table_name`; otherwise you get a runtime `no such table`
  and later `ALTER` migrations crash the server on startup.
- Guard non-idempotent statements (`ADD COLUMN`, `CREATE`, …) with
  `manager.has_table()` / `has_column()` so a half-applied migration can be
  safely re-run.
- Verify a new migration against a fresh database: `forgekeep migrate` then
  check table names.
- When adding a field to `AppState`, also update
  `crates/rg-http/tests/common/mod.rs::build_test_app_state`.

#### `rg-cli` — entry point

**Allowed:** CLI parsing (clap); starting and wiring up services.

**Forbidden:** business logic (delegate to the other crates).

#### `rg-runner` — CI runner (library + the `forgekeep-runner` binary)

**Allowed:** runner registration and heartbeat; polling jobs from the server;
job execution (local shell or Docker); uploading logs and artifacts.

**Forbidden:** touching HTTP routes directly (it is only an HTTP client of the
`rg-http` API); business logic.

This is the **only** external-runner implementation: the deprecated
`forgekeep runner` subcommand of `rg-cli` is a thin alias that delegates here.
A second copy of the poll-and-execute loop is exactly what that alias used to be,
and it silently drifted (no `runner.toml`, no heartbeat, no workspace snapshot),
so new runner behaviour belongs here and nowhere else. The separate
`rg-ci::PipelineRunner` is not a duplicate — it is the server-side executor for
deployments that run CI in-process instead of with external runners; the two must
keep the same fail-closed contract when Docker is unavailable.

#### `rg-mcp` — MCP server (standalone binary)

**Allowed:** exposing repository Tools/Resources over the Model Context Protocol
(stdio); calling the REST API as an authenticated HTTP client.

**Forbidden:** direct database access; duplicating business logic.

---

## Coding conventions

### General

```rust
// ✅ Error handling: anyhow::Result with the ? operator
pub async fn do_something(path: &Path) -> anyhow::Result<()> {
    let output = tokio::process::Command::new("git")
        .arg("-C").arg(path)
        .args(["rev-parse", "HEAD"])
        .output().await
        .context("failed to run git rev-parse")?;
    Ok(())
}

// ✅ Logging: tracing with structured fields
tracing::info!(path = %repo_path.display(), user = %username, "starting upload-pack");

// ✅ Logging an anyhow error: ALWAYS `{:#}`, never bare `%e`
tracing::error!(error = %format!("{e:#}"), "git index-pack failed");

// ❌ Do not use println! / eprintln! for logging
```

#### Logging an error value

`Display` for `anyhow::Error` prints **only the outermost** `.context(...)` — every
cause underneath it is dropped. So `error = %e` turns a carefully layered error
into a one-line summary with the actual reason removed:

```rust
// ❌ prints "db: failed to load pipeline" and nothing about *why*
tracing::error!(error = %e, "pipeline lookup failed");

// ✅ `{:#}` flattens the whole chain: "db: failed to load pipeline: pool timed out …"
tracing::error!(error = %format!("{e:#}"), "pipeline lookup failed");
```

This matters most in background loops, watchdogs and post-push hooks, where the
log line is the *only* channel — nobody gets an HTTP response to inspect. Keep the
structural keys (`job_id`, `repo_id`, paths) as plain fields; only the error value
needs the `{:#}` treatment.

`{:#}` is **`anyhow`-specific**: on any other error type it renders identically to
`{}`, so applying it blindly produces a diff that looks fixed but changes nothing.
Classify the error type first — two cases need different handling:

- **Concrete error types whose `Display` is already self-contained** —
  `std::io::Error` ("No such file or directory (os error 2)") and thiserror enums
  that interpolate their source (`#[error("blob storage I/O error: {0}")]`). A bare
  `%error` is honest there; `{:#}` would just be noise.
- **Concrete error types that hide their cause behind a generic `Display`** —
  `reqwest::Error` says "error sending request for url (…)" while the actionable
  reason (connection refused, DNS failure, TLS rejected) sits one or two `source()`
  hops down. `{:#}` does *not* help; walk the chain instead. See `error_chain()` in
  `crates/rg-runner/src/api.rs`.

Same rule applies when *flattening* an error into a `String` for storage or a
response body: `e.to_string()` truncates, `format!("{e:#}")` does not.

### Async

```rust
// ✅ Spell out Unpin bounds on generic writers
pub async fn write_pkt_line<W: AsyncWrite + Unpin>(writer: &mut W, /* ... */) -> Result<()>

// ✅ Wrap a stream in BufReader only where read_pkt_line is needed, then drop it
{
    let mut reader = BufReader::new(&mut *stream);
    let result = process_push(repo_path, &mut reader).await?;
} // BufReader dropped here; the stream can be reused for writing

// ✅ Use tokio::process::Command for external commands
```

### Errors

```rust
// ✅ Library crates: define error types with thiserror
#[derive(thiserror::Error, Debug)]
pub enum AuthError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("user not found: {0}")]
    UserNotFound(String),
}

// ✅ Application/glue code: anyhow::Result
// ❌ No unwrap() / expect() on production paths
```

### Comments

Explain **why**, not just what — especially for protocol details:

```rust
// The receive-pack report-status response must be sent entirely as band-1
// sideband data. Sending a sideband flush before the plain pkt-lines makes the
// client stop reading, so the trailing pkt-lines are never consumed.
// Verified by capturing real git-receive-pack traffic with GIT_TRACE_PACKET=1.
```

---

## Commit conventions

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>

[body]

[footer]
```

**Types:** `feat`, `fix`, `docs`, `refactor`, `test`, `chore`, `perf`.

**Scope:** the crate name — `rg-git`, `rg-ssh`, `rg-http`, `rg-core`, `rg-db`,
`rg-ci`, `rg-cli`, `rg-runner`, `rg-mcp`.

**Example:**

```
feat(rg-ssh): implement SSH git push with sideband-64k report-status

Wrap report-status pkt-lines in band-1 sideband data instead of sending them
as plain pkt-lines after a sideband flush.

Closes #12
```

---

## Testing

### Unit tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_write_pkt_line() {
        let mut buf = Vec::new();
        write_pkt_line(&mut buf, &PktLine::text("hello")).await.unwrap();
        assert_eq!(&buf, b"000ahello\n");
    }
}
```

Prefer running tests per-crate (`cargo test -p rg-core`) rather than
`cargo test --workspace`, which can be memory-hungry.

### Coverage

The project uses `cargo-llvm-cov`:

```bash
cargo install cargo-llvm-cov
cargo llvm-cov --lib                                       # text report
cargo llvm-cov --html --open                               # HTML report
cargo llvm-cov --lcov --output-path target/coverage.lcov   # LCOV
```

Configuration lives in `cargo-llvm-cov.toml`.

### Integration & runtime regression

Integration entry points are maintained under `scripts/` — please don't add
one-off end-to-end shell scripts, to avoid test drift.

```bash
# Full regression: backend tests, frontend static checks/build, runtime smoke
node scripts/full-interface-regression.mjs

# Backend OpenAPI smoke (server must be reachable)
BACKEND_URL=http://127.0.0.1:8080 node scripts/openapi-interface-smoke.mjs

# Frontend page console/network smoke
BASE=http://127.0.0.1:5173 node scripts/console-smoke.mjs

# Frontend API client vs OpenAPI parameter alignment
BACKEND_URL=http://127.0.0.1:8080 node scripts/api-client-contract-check.mjs
```

`full-interface-regression.mjs` supports scoping and timeout/retry toggles via
environment variables (e.g. `FULL_REGRESSION_ONLY=backend`,
`SKIP_RUNTIME_SMOKES=1`, `REGRESSION_TIMEOUT_MS=...`). Run it with
`--help`-style discovery or read the script header for the full matrix.

---

## Branching & PRs

```
main          ← stable; only tested PRs are merged
dev           ← development trunk
feat/<topic>  ← feature branches
fix/<topic>   ← bug-fix branches
```

Before a PR is merged into `main`:

1. `cargo build --release` passes.
2. `cargo clippy` reports no errors (and preferably no new warnings).
3. The relevant `scripts/` regression subset passes.
