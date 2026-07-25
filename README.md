# ForgeKeep

> A lightweight, self-hosted Git platform written in Rust.

[![Rust](https://img.shields.io/badge/rust-1.95%2B-orange)](https://www.rust-lang.org/)
[![License: Proprietary](https://img.shields.io/badge/license-Proprietary-red)](LICENSE)

ForgeKeep is a full-featured Git hosting platform — repositories, issues, pull
requests, code review, wiki, CI/CD, and a package registry — built as a single
Rust workspace. It targets a small memory footprint and single-binary
deployment, in the same space as [Gitea](https://gitea.com/) and
[Forgejo](https://forgejo.org/).

The server speaks the Git Smart Protocol (V1 + V2) over both HTTPS and SSH, so
`git clone` / `git push` work out of the box, and everything else is exposed
through a REST API and an optional SvelteKit web UI.

> **Origin.** ForgeKeep is a fork of [IronForge](https://github.com/lengyuqu/ironforge).
> See [NOTICE](NOTICE) for provenance and licensing details.

---

## Features

| Area | Capability |
|------|------------|
| Git transport | Smart Protocol V1 & V2 over HTTPS and SSH (`russh`); pkt-line, sideband-64k, packfile encode/decode |
| Repositories | Create / delete / transfer, fork, private & public, collaborators (read/write/admin), file & tree browsing, commit log, branches, tags |
| Auth | User registration & login (argon2 + JWT), SSH public-key auth, Personal Access Tokens, MFA, SSO |
| Issues | CRUD, labels, milestones, comments |
| Pull requests | Diff, three merge strategies (merge / squash / rebase), cross-repo (fork) PRs, code review (approve / request changes / inline comments), branch protection |
| Wiki | Page CRUD backed by Git |
| Git LFS | Batch API, object upload/download, zstd compression |
| CI/CD | Native `.forgekeep-ci.yml` pipelines and Gitea Actions (`.gitea/workflows/*.yml`); embedded or external runners, optional Docker execution, artifacts, live job logs over WebSocket |
| Webhooks | Registration, delivery, HMAC-SHA256 signatures, delivery history |
| Notifications | In-app, email (SMTP), and real-time WebSocket delivery |
| Registries | Package registry and OCI container registry |
| Search | Full-text search (FTS) and per-repository code indexing |
| Import | Pull repositories, issues, PRs, labels, milestones, releases and wiki from GitHub / GitLab |
| Operations | TLS/HTTPS, TOML config, rate limiting, log rotation, unified pagination, GPG signature verification, audit log, health checks |
| AI integration | `forgekeep-mcp` — a Model Context Protocol (stdio) server exposing repository tools/resources to agents |
| Web UI | SvelteKit 5 SPA (login, repos, issues, PRs, wiki, CI, review, orgs, notifications), English + Chinese i18n |

Databases: SQLite, PostgreSQL, and MySQL are all supported and selected at
runtime from the `database_url` scheme — no feature rebuild required.

---

## Quick start

### Requirements

- Rust 1.95+ (stable)
- `git` on `PATH` (used for a few pack / diff operations still delegated to the CLI)
- Linux or macOS

### Build

```bash
git clone https://github.com/Yahook/ForgeKeep.git
cd ForgeKeep
cargo build --release
```

The main binary is `target/release/forgekeep`. The workspace also produces
`forgekeep-runner` (standalone CI runner) and `forgekeep-mcp` (MCP server).

### Generate an SSH host key

The server needs an SSH host key on first run:

```bash
ssh-keygen -t ed25519 -f ./forgekeep_host_key -N ""
```

### Run the server

```bash
./target/release/forgekeep serve \
  --repo-root ./repos \
  --http-addr 0.0.0.0:8080 \
  --ssh-addr  0.0.0.0:2222 \
  --host-key  ./forgekeep_host_key \
  --db-url    "sqlite://./forgekeep.db?mode=rwc" \
  --jwt-secret "$(forgekeep gen-secret)"
```

`forgekeep gen-secret` prints a fresh 256-bit secret (the `openssl rand -base64
32` equivalent). The server refuses to start with the shipped
`change-me-in-production` placeholder, so generate your own and keep it out of
version control. Database migrations run automatically on startup. Set the log
level with `RUST_LOG` (e.g. `RUST_LOG=debug`).

Common `serve` flags:

| Flag | Description | Default |
|------|-------------|---------|
| `--repo-root` | Root directory for bare repositories | `./repos` |
| `--http-addr` | HTTP listen address | `0.0.0.0:8080` |
| `--ssh-addr` | SSH listen address | `0.0.0.0:2222` |
| `--host-key` | SSH host key path | — |
| `--db-url` | `sqlite://` / `postgres://` / `mysql://` URL | `sqlite://./forgekeep.db?mode=rwc` |
| `--jwt-secret` | JWT signing key (use a long random value) | — |
| `--config` | TOML config file; a flag you pass wins over its config key | — |
| `--tls-cert` / `--tls-key` | PEM cert/key to enable HTTPS | — |
| `--docker` | Run CI jobs with an `image` in Docker | `false` |
| `--external-runners` | Use external runners instead of the embedded one | `false` |
| `--rate-limit-max` / `--rate-limit-window` | Rate limit (0 = disabled) | `0` / `60` |
| `--smtp-host` … `--smtp-from` | SMTP settings for email notifications | — |
| `--log-file` / `--log-max-files` | Enable rotating file logs | — / `5` |

Prefer a config file? Copy `forgekeep.example.toml` to `forgekeep.toml`, edit
it, and pass `--config forgekeep.toml`. Every flag in the table above has a
config-file equivalent (named in `forgekeep serve --help`), and values resolve
as **CLI arg > config file > built-in default** — so a config-only deployment
needs no flags at all.

### Create a test repository

```bash
./target/release/forgekeep create-repo testuser testrepo --repo-root ./repos
# → ./repos/testuser/testrepo.git
```

---

## Using Git

### SSH

```bash
GIT_SSH_COMMAND="ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null" \
  git clone ssh://git@localhost:2222/testuser/testrepo /tmp/myrepo
cd /tmp/myrepo
echo hello > test.txt && git add -A && git commit -m "test"
GIT_SSH_COMMAND="ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null" \
  git push origin main
```

### HTTP

```bash
git clone http://localhost:8080/git/testuser/testrepo /tmp/myrepo-http
cd /tmp/myrepo-http
# ... edit files ...
git push origin main
```

---

## REST API

All endpoints live under `/api/v1/`. Authenticated routes expect an
`Authorization: Bearer <token>` header, where the token is a JWT (from login)
or a Personal Access Token.

```bash
# Register
curl -X POST http://localhost:8080/api/v1/users/register \
  -H "Content-Type: application/json" \
  -d '{"username":"testuser","email":"test@example.com","password":"secret123"}'

# Login → returns a JWT
curl -X POST http://localhost:8080/api/v1/users/login \
  -H "Content-Type: application/json" \
  -d '{"login":"testuser","password":"secret123"}'

# Create a repository
curl -X POST http://localhost:8080/api/v1/repos \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"name":"myrepo","description":"test repo"}'

# Open an issue
curl -X POST http://localhost:8080/api/v1/repos/testuser/myrepo/issues \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"title":"Bug report","body":"Something is wrong","labels":"bug"}'

# Open a pull request
curl -X POST http://localhost:8080/api/v1/repos/testuser/myrepo/pulls \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"title":"Add feature","body":"Description","head_branch":"feature","base_branch":"main"}'
```

The full API is documented via OpenAPI. `/api-docs/*` (OpenAPI JSON + Swagger
UI) is protected by default and requires a valid JWT or PAT:

```bash
TOKEN="$(curl -s http://localhost:8080/api/v1/users/login \
  -H 'Content-Type: application/json' \
  -d '{"login":"testuser","password":"secret123"}' | jq -r '.token')"

curl -H "Authorization: Bearer ${TOKEN}" http://localhost:8080/api-docs/openapi.json
```

---

## CLI

Beyond `serve`, the `forgekeep` binary offers:

| Command | Purpose |
|---------|---------|
| `serve` | Start the server (HTTP + SSH) |
| `migrate` | Run database migrations and exit |
| `rebuild-fts` | Rebuild full-text search indexes |
| `backup-db` / `restore-db` | Create / restore a consistent SQLite backup |
| `create-repo` | Create a bare repository (no DB record — quick testing) |
| `runner` | Run as a CI runner (polls and executes jobs) |
| `import github\|gitlab <url>` | Import a repository (and metadata) from GitHub/GitLab |
| `index-repo <owner/name>` | Index a repository for code search |
| `package` | Manage the package registry |

Every subcommand that touches the database or the repository directory
(`migrate`, `rebuild-fts`, `backup-db`, `restore-db`, `create-repo`, `import`,
`index-repo`, `package list`) takes the same `--config` as `serve` and resolves
`--db-url` / `--repo-root` as **CLI arg > config file > built-in default**. On a
config-file deployment, pass `--config` rather than repeating the URL: with
neither, they fall back to `sqlite://./forgekeep.db?mode=rwc` in the working
directory, so `migrate` would migrate an empty database and `backup-db` would
back it up.

Run `forgekeep <command> --help` for the full flag list.

---

## Tech stack

| Layer | Choice | Version |
|-------|--------|---------|
| Async runtime | tokio | 1.x |
| HTTP | axum + axum-server | 0.8 / 0.7 |
| SSH server | russh | 0.51 |
| Git objects | gix (gitoxide) + `git` CLI gateway | 0.84 |
| ORM | SeaORM (SQLite / PostgreSQL / MySQL) | 1.1 |
| Auth | argon2 + JWT | — |
| TLS | rustls + tokio-rustls | 0.23 / 0.26 |
| Logging | tracing + tracing-appender | 0.1 |
| CLI | clap | 4.x |
| Frontend | SvelteKit 5 (adapter-static, SPA) | — |
| Coverage | cargo-llvm-cov | — |

> Git object operations use a hybrid of `gix` and a `GitCommandGateway`: pack,
> rebase, archive and some diff/GPG paths still shell out to the `git` CLI
> while the corresponding `gix` capabilities mature.

---

## Repository layout

```
ForgeKeep/
├── Cargo.toml              # workspace root
├── ARCHITECTURE.md         # architecture overview
├── CONTRIBUTING.md         # development guide
├── forgekeep.example.toml  # sample configuration
├── crates/
│   ├── rg-cli/     # main binary  → forgekeep
│   ├── rg-core/    # business logic (users, repos, issues, PRs, wiki, LFS, webhooks, ...)
│   ├── rg-git/     # Git protocol layer (pkt-line, upload/receive-pack, sideband)
│   ├── rg-ssh/     # SSH server (russh)
│   ├── rg-http/    # HTTP server + REST API + WebSocket (axum)
│   ├── rg-db/      # database layer (SeaORM entities + migrations)
│   ├── rg-ci/      # CI/CD engine (YAML parsing + pipeline executor)
│   ├── rg-runner/  # standalone CI runner  → forgekeep-runner
│   └── rg-mcp/     # MCP server  → forgekeep-mcp
├── web/            # SvelteKit frontend (standalone SPA)
├── docs/           # design and protocol notes
├── deploy/         # deployment assets
└── scripts/        # regression / smoke-test scripts
```

> **Crate vs. binary naming.** Library crates keep a neutral `rg-*` prefix;
> the user-facing binaries are `forgekeep`, `forgekeep-runner`, and
> `forgekeep-mcp`.

See [ARCHITECTURE.md](ARCHITECTURE.md) for subsystem design and
[CONTRIBUTING.md](CONTRIBUTING.md) for development, testing, and crate
boundaries.

---

## Development

```bash
cargo build                 # debug build
cargo build --release       # release build
cargo clippy                # lint
cargo llvm-cov --html       # coverage report
```

Integration and runtime regressions are driven by scripts under `scripts/`
rather than ad-hoc shell snippets — for example:

```bash
# Full regression: backend tests, frontend checks/build, runtime smoke
node scripts/full-interface-regression.mjs

# Backend OpenAPI smoke against a running server
BACKEND_URL=http://127.0.0.1:8080 node scripts/openapi-interface-smoke.mjs
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full workflow and the available
environment toggles.

---

## License

ForgeKeep is proprietary software — see the [LICENSE](LICENSE) file. All rights
reserved by [Yahook](https://github.com/Yahook); no use, copying, modification,
or distribution is permitted without prior written permission. Please read
[NOTICE](NOTICE) for the fork's upstream provenance.
