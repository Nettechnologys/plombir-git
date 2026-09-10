# ForgeKeep — Architecture

ForgeKeep is a self-hosted Git platform written in Rust, organized as a single
Cargo workspace. This document describes the high-level architecture, the
technology choices, and the design of the core subsystems.

> ForgeKeep is a fork of [IronForge](https://github.com/lengyuqu/ironforge);
> see [NOTICE](NOTICE) for provenance.

---

## 1. Goals

- **Lightweight** — small memory footprint and a single-binary deployment.
- **Full-featured** — repositories, users/organizations, issues, pull
  requests, code review, wiki, LFS, CI/CD, and registries.
- **Standard Git** — the Git Smart Protocol (V1 + V2) over both HTTPS and SSH.
- **Portable** — Linux and macOS; pure-Rust dependencies wherever practical to
  keep cross-compilation and container images small.

---

## 2. System overview

```
┌─────────────────────────────────────────────────────────┐
│                        Clients                           │
│   git CLI (SSH / HTTPS)  ·  Web browser  ·  REST API      │
└──────────┬──────────────────────┬───────────────────────┘
           │ SSH (russh)          │ HTTPS (axum)
           ▼                      ▼
┌─────────────────────────────────────────────────────────┐
│                   Protocol adapters                       │
│  ┌──────────────┐  ┌──────────────┐  ┌───────────────┐   │
│  │ SSH handler  │  │ HTTP handler │  │ REST API      │   │
│  │ upload-pack  │  │ info/refs    │  │ /api/v1/...   │   │
│  │ receive-pack │  │ upload-pack  │  │ + WebSocket   │   │
│  └──────┬───────┘  └──────┬───────┘  └──────┬────────┘   │
└─────────┼─────────────────┼─────────────────┼───────────┘
          │                 │                 │
          ▼                 ▼                 ▼
┌─────────────────────────────────────────────────────────┐
│                      Core (rg-core)                       │
│  Repository · User/Org/Team · Issue · PullRequest         │
│  Wiki · LFS · Webhook · Review · Notification · Registry   │
└─────────────────┬───────────────────────┬───────────────┘
                  │                       │
        ┌─────────┴────────┐    ┌────────┴────────┐
        ▼                  ▼    ▼                 ▼
┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐
│ Git data     │  │ Persistence  │  │    CI/CD engine      │
│ (gix + git   │  │ (SeaORM:     │  │  (rg-ci): pipelines, │
│  CLI gateway)│  │  SQLite/PG/  │  │  jobs, runners,      │
│ objects/refs │  │  MySQL)      │  │  artifacts, logs     │
└──────────────┘  └──────────────┘  └──────────────────────┘
```

---

## 3. Technology choices

| Concern | Choice | Notes |
|---------|--------|-------|
| Async runtime | tokio | De-facto standard for async Rust |
| HTTP framework | axum 0.8 + axum-server | tokio-native, strong ecosystem |
| SSH server | russh 0.51 | Pure-Rust SSH2, server-side support |
| Git objects | gix (gitoxide) 0.84 + `git` CLI gateway | Pure Rust where available; a `GitCommandGateway` covers pack / rebase / archive / some diff & GPG paths not yet in gix |
| ORM | SeaORM 1.1 | Async, built-in migrations; SQLite / PostgreSQL / MySQL selected at runtime by the `database_url` scheme |
| Auth | argon2 + jsonwebtoken | Password hashing + JWT; PAT / MFA / SSO on top |
| TLS | rustls + tokio-rustls | No OpenSSL dependency |
| Serialization | serde (json / toml) | — |
| Logging | tracing + tracing-subscriber + tracing-appender | Structured logs, rotating file output |
| CLI | clap 4 | Derive API |
| Frontend | SvelteKit 5 (adapter-static, SPA) | Small bundle, deployed as static assets |

### Why gix + a CLI gateway

A pure-Rust Git library keeps cross-compilation and image size small, but the
**server-side** Smart Protocol (upload-pack / receive-pack, pkt-line, sideband,
packfile encode/decode) is implemented in `rg-git` directly. Operations that
`gix` does not yet cover well are delegated to the system `git` binary through a
single `GitCommandGateway`, so the fallback surface is explicit and easy to
retire over time.

### Why a runtime-selected database

`rg-db` enables the SQLite, PostgreSQL, and MySQL `sqlx` backends together, so
an operator chooses the database purely by the `database_url` scheme
(`sqlite://`, `postgres://`, `mysql://`) with no feature rebuild. SQLite (WAL
mode) is the lightweight default; PostgreSQL/MySQL suit larger deployments.

---

## 4. Workspace structure

```
ForgeKeep/
├── Cargo.toml                    # workspace root
├── crates/
│   ├── rg-core/                  # core business logic
│   │   └── src/{repo,user,auth,issue,pull_request,wiki,hook,lfs,ci,...}
│   ├── rg-git/                   # Git protocol layer
│   │   └── src/{protocol,transport,object}
│   ├── rg-ssh/                   # SSH server (russh)
│   ├── rg-http/                  # HTTP server + REST API + WebSocket (axum)
│   │   └── src/{api,git_v2,security,ws,oci,...}
│   ├── rg-db/                    # SeaORM entities + migrations
│   ├── rg-ci/                    # CI/CD engine (native + Gitea Actions)
│   ├── rg-cli/                   # main binary  → forgekeep
│   ├── rg-runner/                # CI runner agent  → forgekeep-runner
│   ├── rg-process/               # bounded child-process lifecycle helpers
│   └── rg-mcp/                   # MCP server  → forgekeep-mcp
├── web/                          # SvelteKit frontend
├── forgekeep.example.toml        # sample configuration
├── deploy/                       # deployment assets (Dockerfile, etc.)
└── docs/                         # design and protocol notes
```

> Library crates keep a neutral `rg-*` prefix; the user-facing binaries are
> `forgekeep`, `forgekeep-runner`, and `forgekeep-mcp`.

### Crate dependency direction

```
rg-ci ──> rg-core, rg-db, rg-git, rg-process
rg-cli ──> rg-ci, rg-core, rg-db, rg-git, rg-http, rg-runner, rg-ssh
rg-core ──> rg-db, rg-git
rg-db ──> none
rg-git ──> rg-process
rg-http ──> rg-core, rg-db, rg-git
rg-mcp ──> none
rg-process ──> none
rg-runner ──> rg-process
rg-ssh ──> rg-core, rg-db, rg-git
```

The graph lists normal/build Cargo dependencies between workspace crates;
test-only dev-dependencies are intentionally excluded. `rg-runner` and `rg-mcp`
are HTTP clients of APIs served by `rg-http`, but neither has a Cargo dependency
on `rg-http`. `rg-git` remains protocol-only: its sole internal dependency is
the business-agnostic `rg-process` lifecycle helper.

`scripts/architecture-crate-dependency-contract-check.mjs` compares this block
with `cargo metadata`, so adding a crate edge without updating the graph fails
the contract checks. See [CONTRIBUTING.md](CONTRIBUTING.md) for the per-crate
boundary rules.

---

## 5. Data model (core tables)

```
┌─────────────┐     ┌──────────────────┐     ┌──────────────────┐
│   users     │     │   repositories   │     │     issues       │
├─────────────┤     ├──────────────────┤     ├──────────────────┤
│ id          │◄──┐ │ id               │◄──┐ │ id               │
│ username    │   │ │ name             │   │ │ title / body     │
│ email       │   │ │ description      │   │ │ state            │
│ password    │   │ │ owner_id ────────┘   │ │ repo_id ─────────┘
│ is_admin    │   │ │ is_private       │   │ │ author_id        │
│ created_at  │   │ │ default_branch   │   │ │ milestone_id     │
└──────┬──────┘   │ └──────────────────┘   │ └──────────────────┘
       │          │                        │
       │          │ ┌──────────────────┐   │ ┌──────────────────┐
       │          │ │  pull_requests   │   │ │    wiki_pages    │
       │          │ ├──────────────────┤   │ ├──────────────────┤
       │          │ │ id / title / body│   │ │ id / title       │
       │          │ │ state            │   │ │ content (MD)     │
       │          └─│ repo_id          │   └─│ repo_id          │
       │            │ base/head_branch │     │ version          │
       │            │ merged_at        │     └──────────────────┘
       │            └──────────────────┘
       │  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────┐
       │  │  access_tokens   │  │   ci_pipelines   │  │  ssh_keys    │
       │  ├──────────────────┤  ├──────────────────┤  ├──────────────┤
       └─►│ user_id          │  │ repo_id          │  │ user_id      │
          │ token_hash       │  │ config / status  │  │ public_key   │
          │ scopes / expires │  │ trigger          │  │ fingerprint  │
          └──────────────────┘  └────────┬─────────┘  └──────────────┘
                                         │
                                ┌────────┴─────────┐
                                │    ci_jobs       │
                                ├──────────────────┤
                                │ pipeline_id      │
                                │ name / status    │
                                │ log / timestamps │
                                └──────────────────┘
```

Additional tables cover organizations/teams, labels, milestones, comments,
LFS objects, webhooks and deliveries, releases/tags, notifications, package and
container registry metadata, and full-text search indexes. Schema is owned by
`rg-db` (SeaORM entities + migrations, applied automatically on startup).

---

## 6. Core subsystems

### 6.1 Git protocol layer (`rg-git`)

The most protocol-heavy module: it implements the Git Smart Protocol (V1 + V2)
from scratch, so both SSH and HTTP transports share the same core.

**SSH (russh):**

```
git clone/push
    │ SSH
    ▼
russh server (rg-ssh)
  · public-key auth  → looked up in rg-db
  · password auth    → argon2 verification
  · exec_request routes:
      git-upload-pack  → fetch/clone
      git-receive-pack → push
    │
    ▼
rg-git protocol module
  · pkt-line parser         · want/have negotiation
  · capability negotiation  · packfile encode/decode
  · side-band multiplexing
```

**HTTP (axum):**

```
GET  /{owner}/{repo}.git/info/refs?service=git-upload-pack
POST /{owner}/{repo}.git/git-upload-pack
POST /{owner}/{repo}.git/git-receive-pack
GET  /{owner}/{repo}.git/HEAD
```

### 6.2 Pull-request engine

A pull request is a state machine over a **Git diff** plus review and webhook
side effects:

```
OPEN ──► REVIEWING ──► APPROVED ──► MERGED
  │           │
  ▼           ▼
CLOSED    CHANGES_REQUESTED

Merge strategies: merge commit (default) · squash · rebase
```

Diffs are computed against the base and head trees; cross-repository (fork)
PRs are supported by tracking a separate head repository.

### 6.3 CI/CD engine (`rg-ci`)

Pipelines are defined either in the native `.forgekeep-ci.yml` format or in the
Gitea Actions format (`.gitea/workflows/*.yml`). A push to a branch triggers a
pipeline in the background after `receive-pack`; tags and manual triggers are
also supported. The native format's keys, and the rules the engine enforces on
them, are documented in [docs/ci.md](docs/ci.md); the subset of Actions the
second format implements — and everything outside it that is refused — in
[docs/gitea-actions.md](docs/gitea-actions.md).

The two formats are tried in that order, and the fallback from Gitea Actions to
the native file happens only when `.gitea/workflows` is missing or holds no
workflow triggered by this event. A workflow file that exists but is broken —
not UTF-8, invalid YAML, an unsupported `uses:`, a missing local reusable
workflow — fails the trigger with an error naming the file and the reason; it is
never reported as "no CI config found".

```
Pipeline triggered
    │
    ▼
Scheduler (rg-ci)
    ├── Job: test   ─ runner executes ─ status: passed/failed
    ├── Job: build  ─ runner executes ─ artifacts uploaded
    └── Job: deploy ─ runner executes ─ (depends on build)
```

Jobs run on the **embedded runner** by default, or on **external runners**
(`--external-runners`) that authenticate with a runner token and poll for work.
Jobs with an `image` can be executed in Docker (`--docker`). Job logs stream to
the UI over a WebSocket (`/ws/job/:job_id`); artifacts are stored and served
through the REST API.

### 6.4 Wiki engine

A repository's wiki is a Markdown-backed page store with version history
(edits tracked in the database), rendered server-side and browsable through the
web UI.

### 6.5 MCP server (`rg-mcp`)

`forgekeep-mcp` is a Model Context Protocol server (stdio transport) that
exposes repository data as Tools and Resources to MCP-capable AI agents. It acts
as an HTTP client of the ForgeKeep REST API and authenticates with a PAT.

---

## 7. Security & operations

- **AuthN/AuthZ** — argon2 password hashing, JWT sessions, Personal Access
  Tokens, MFA and SSO; permission checks map org/team membership to
  `can_read` / `can_write` on Git transport and the REST API.
- **Transport** — optional TLS/HTTPS (rustls); configurable CORS and CSP.
- **Abuse controls** — token-bucket rate limiting with a trusted-proxy list for
  `X-Forwarded-For` / `X-Real-IP`.
- **Data safety** — parameterized queries, consistent SQLite backup/restore
  commands, health checks (DB ping + filesystem), and an audit log.
- **Observability** — structured `tracing` logs with daily file rotation and a
  request-ID middleware.

---

## 8. Configuration

Configuration is resolved as **CLI args > config file > defaults**. The config
file is TOML (`forgekeep.toml`; see `forgekeep.example.toml` for the operator
template). Model sections include `server`, `database`, `auth`, `ci`, `releases`,
`rate_limit`, `smtp`, `tls`, `logging`, `audit`, `backup`, `mirror`, `imports`,
`timeouts`, `webhooks`, and `observability`. The model and the resolution live in
`rg-cli/src/config.rs` and are shared by **every** subcommand, not just `serve`:
`migrate`, `rebuild-fts`, `backup-db`, `restore-db`, `rotate-instance-key`,
`rotate-encryption-key`, `create-repo`, `import`, `index-repo`,
`list-tombstones` and `package list` all take `--config` and read `[database].url` /
`[server].repo_root` through the same functions the server uses; `import` also
reads `[imports].trusted_origins`, so an operator-only private-origin exception
is identical in server and one-shot import modes. An admin command therefore
cannot silently address a different database or import trust boundary than the
running server.
Correspondingly, **no flag that has a config-file equivalent may carry a clap
`default_value`** — a clap default is indistinguishable from a value the operator
typed, so it makes the config key unreachable; the built-in defaults live in
`config::DEFAULT_*` and are named in each flag's `--help`.
Environment variables use the `FORGEKEEP_*` prefix.

**Path-typed keys are checked at startup, not on first use.** `server.repo_root`,
`tls.cert` / `tls.key`, `logging.file`, `audit.archive_dir` and `backup.dir` are
created and/or write-probed before the servers come up, and a failure aborts the start
with the path, the uid/ownership diagnostic and the knob to fix
(`rg_core::platform::fs::describe_path_error`). `server.host_key` is checked the
same way but only fails the SSH listener — HTTP keeps serving. The rule exists
because the alternative is silent degradation: `audit.archive_dir` used to be
touched only by the hourly archiver loop, so an unwritable directory meant audit
retention never ran and said so in a warning nobody reads.
