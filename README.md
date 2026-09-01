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
| Issues | CRUD, labels, milestones, comments, Gitea-compatible issue templates and chooser ([template reference](docs/issue-templates.md)) |
| Pull requests | Diff, three merge strategies (merge / squash / rebase), cross-repo (fork) PRs, code review (approve / request changes / inline comments), branch protection, repository pull-request template ([reference](docs/issue-templates.md#pull-request-templates)), CODEOWNERS auto-review ([reference](docs/codeowners.md)) |
| Wiki | Page CRUD backed by Git |
| Git LFS | Batch API, object upload/download, zstd compression |
| CI/CD | Native `.forgekeep-ci.yml` pipelines ([schema reference](docs/ci.md)) and Gitea Actions (`.gitea/workflows/*.yml`, [supported subset](docs/gitea-actions.md)); embedded or external runners, optional Docker execution, artifacts, live job logs over WebSocket |
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
| `--encryption-key` | Key for data at rest — see [Secrets and rotation](#secrets-and-rotation) | `[auth].key_file` |
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

### Passkeys need one canonical public URL

Set `[server].external_url` before anyone registers a passkey:

```toml
[server]
external_url = "https://git.example.com"
```

WebAuthn credentials are bound to a relying-party hostname. Without this
setting ForgeKeep has to use each request's `Host`, so the same person opening
the instance through another proxy name will not see the credential in their
browser. Credentials registered after this setting is present retain that RP
id and are deliberately excluded from challenges at a different host. Existing
credentials from before RP tracking remain compatible where possible; the
startup warning names them so they can be re-enrolled at the canonical URL.

### Secrets and rotation

ForgeKeep holds two configured secrets and one stored key. They do different
jobs, and telling them apart is what makes rotation safe.

| Secret | Sources (first wins) | Protects |
|--------|----------------------|----------|
| **JWT secret** | `FORGEKEEP_JWT_SECRET` › `--jwt-secret` › `[auth].jwt_secret` | Signatures: session tokens, PAT-derived tokens, CI job tokens |
| **Encryption key** | `FORGEKEEP_ENCRYPTION_KEY` › `--encryption-key` › `[auth].encryption_key` › `[auth].key_file` | Data at rest: TOTP secrets, CI secrets, mirror and LDAP passwords, SSO client secrets, OAuth tokens, the instance signing key below |
| **Instance signing key** | Stored in the database, established on first start | This instance's public identity: release-asset provenance attestations and the CI OIDC JWKS |

**The encryption key establishes itself.** With no explicit key source, first
start generates `[auth].key_file` beside `[server].host_key` with mode `0600`
(the container configuration therefore uses `/data/encryption_key`). The file
belongs in the same backup as the database and is independent of the JWT
signing secret. Set an explicit key only when a KMS or vault is the intended
source:

```toml
[auth]
jwt_secret     = "..."   # rotate this freely
# encryption_key = "..." # optional external key source
# key_file = "/data/encryption_key"
```

**Rotating the JWT secret** (a leak, or routine hygiene): change `jwt_secret`
and restart. Existing sessions are invalidated — everyone logs in again — while
the durable at-rest key file remains unchanged.

**The instance signing key is not a config value.** It signs release-asset
attestations and backs the public keys at `/api/v1/ci/oidc/jwks`, so it has to
outlive the secrets you rotate: a key that changed with `jwt_secret` would make
every attestation this server ever issued fail verification — against the very
server that signed it — and change the `kid` under external verifiers that
already fetched it. The server establishes the key on first start (adopting the
one the old derivation produced, so nothing signed earlier is invalidated by
upgrading), seals it with the encryption key, and keeps it in the database.

Replace it only if the key itself is compromised, and knowing that every
attestation signed with it stops verifying for good:

```bash
forgekeep rotate-instance-key --config forgekeep.toml --yes
```

**Rotating the encryption key** is a different operation — the stored
ciphertext has to be re-encrypted — so it has its own command. It refuses to
start while a ForgeKeep server holds a file-backed SQLite database, for two
reasons that both point the same way: a handler writing an encrypted column
mid-pass would leave a value under the old key, and the pass holds the single
write lock for its whole duration. Stop the server first, and look before you
leap:

```bash
forgekeep rotate-encryption-key --config forgekeep.toml \
    --old "<the current key>" --new "$(forgekeep gen-secret)" --dry-run
```

The dry run reports, per column, how many stored values the old key opens and
how many it does not, and writes nothing. Re-run it with `--yes` instead of
`--dry-run` to apply: every value is re-sealed in one transaction, and then
the configured source has to carry the new secret before the server is started
again: update `[auth].encryption_key` / `FORGEKEEP_ENCRYPTION_KEY`, or replace
`[auth].key_file` with mode `0600`.

`--old` defaults to the key this deployment already resolves, so you only need
it when the current key is not what the config says — on an instance that
rotated `jwt_secret` without ever setting `encryption_key`, the key that opens
the data is the *previous* signing secret. If the old key opens nothing at all,
the command refuses and changes nothing rather than sealing the database away.
Values it cannot open — legacy plaintext, or rows damaged earlier — are counted
and reported, never rewritten and never deleted. Keep the old secret until the
server has started under the new one.

On startup the server samples the encrypted columns and checks the configured
key opens them. If it opens none of them, it **refuses to start** and prints
the recovery, instead of starting and then failing MFA logins, CI jobs, mirror
syncs and LDAP binds one at a time with unrelated-looking 500s. A brand-new
database has nothing to check, so a fresh install is never blocked.

If the old secret is genuinely lost, no tool can recover the encrypted values:
clear them and have MFA re-enrolled and the stored credentials re-entered. That
includes the instance signing key — `rotate-instance-key` mints a new identity,
and attestations signed under the old one stay unverifiable.

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
| `migrate` | Run database migrations and exit (file-backed SQLite requires the server to be stopped; this is enforced) |
| `rebuild-fts` | Rebuild full-text search indexes (file-backed SQLite requires the server to be stopped; this is enforced) |
| `gen-secret` | Print a fresh 256-bit secret for `[auth].jwt_secret` or an encryption key |
| `backup-db` / `restore-db` | Create / restore a consistent SQLite backup by hand (for a schedule, use `[backup]` — the server snapshots itself) |
| `rotate-encryption-key` | Re-encrypt every at-rest secret onto a new encryption key (file-backed SQLite requires the server to be stopped, `--dry-run` included; this is enforced) |
| `rotate-instance-key` | Mint a new provenance signing identity (invalidates past attestations) |
| `create-repo` | Create a bare repository (no DB record — quick testing) |
| `runner` | Run as a CI runner (polls and executes jobs) |
| `import github\|gitlab <url>` | Import a repository (and metadata) from GitHub/GitLab (file-backed SQLite requires the server to be stopped) |
| `index-repo <owner/name>` | Index a repository for code search |
| `package` | Manage the package registry (`package list` requires a stopped server on file-backed SQLite) |

Every subcommand that touches the database or the repository directory
(`migrate`, `rebuild-fts`, `backup-db`, `restore-db`, `rotate-instance-key`,
`rotate-encryption-key`, `create-repo`, `import`, `index-repo`, `package list`)
takes the same `--config` as `serve` and resolves `--db-url` / `--repo-root` as
**CLI arg > config file > built-in default**. On a config-file deployment, pass
`--config` rather than repeating the URL: with neither,
they fall back to `sqlite://./forgekeep.db?mode=rwc` in the working directory,
so `migrate` would migrate an empty database and `backup-db` would back it up.

For a file-backed SQLite deployment, two groups of commands are deliberately
offline-only, for two different reasons, and both are enforced rather than
documented and hoped for.

Every CLI path that can apply pending migrations (`migrate`, `import`, and
`package list`) is offline-only because another process caches the schema it is
about to change. Every whole-database maintenance pass (`rebuild-fts`,
`rotate-encryption-key`, `--dry-run` included) is offline-only because it holds
SQLite's single write lock from its first statement to its commit: a live
writer that meets a held write lock is refused with `database is locked` rather
than queued behind it, so running one of these against a live instance turns
ordinary user writes into errors for as long as the pass takes.

Either way: stop every ForgeKeep server using the database, run the command,
then restart the server. The processes coordinate through a persistent sidecar
lease next to the database; a live server makes these commands fail before they
open a pool.

PostgreSQL and MySQL migrations stay online — no server has to be stopped — but
they are no longer unserialised. Every migrator takes a lock inside the database
itself (`pg_advisory_lock` / `GET_LOCK`) first, so two replicas booting at once,
a restart overlapping its predecessor, or a `migrate` run alongside a starting
server queue up instead of racing inside `CREATE TABLE`. A migrator that waits
more than five minutes for the holder gives up with a message naming ForgeKeep
and what to do, rather than a PostgreSQL system-index name. The lock belongs to
a database session, so a crashed migrator releases it without leaving anything
to clean up.

That trap is the reason backups are not left to a manual command: enable
`[backup]` in the config file and the server takes a `VACUUM INTO` snapshot
every `interval_hours` from the pool it is already using, keeping the newest
`keep_last`. An unwritable `[backup].dir` fails the start rather than surfacing a
day later, and `forgekeep_db_backup_last_success_timestamp_seconds` lets
monitoring alert on "the last backup is older than N hours". It covers the
database only — repositories under `repo_root` need a volume snapshot of their
own. See `deploy/README.md` for the details.

Run `forgekeep <command> --help` for the full flag list.

`forgekeep package publish --token ...` follows the same boundary as the
standalone clients: remote `--server-url http://...` is refused, while loopback
HTTP remains usable for local development. `--allow-insecure-http` is the
explicit exception for a deliberately plaintext remote package server; it does
not relax the exact-origin redirect policy.

---

## CI runner (`forgekeep-runner`)

CI jobs do not run inside the server. `forgekeep-runner` is a separate binary you
install on the build machine: it registers once, then polls the server for jobs
whose `tags:` its own labels satisfy and executes them — natively, or in a
container when the job names an image (see [docs/ci.md](docs/ci.md)).

### Register

Registration mints the runner's identity, and it needs an **admin user's JWT** —
not a runner token, which is what registration produces. Pass it as
`--auth-token`, or as `FORGEKEEP_AUTH_TOKEN` to keep the secret out of the
process list:

```bash
FORGEKEEP_AUTH_TOKEN="$ADMIN_JWT" forgekeep-runner register \
  --server https://forge.example.com \
  --repository owner/project \
  --name builder-1 \
  --labels docker,linux,amd64 \
  --save --config ~/.forgekeep/runner.toml
```

`--repository owner/project` is the runner token's hard capability boundary:
the runner can claim only that repository's jobs, even when another repository
uses the same labels. `--save` writes the scope together with the issued
`runner_id` and `token` into `--config`. Without it they are only printed, and
the next start registers a second runner.

Runners issued before repository scoping was introduced are intentionally
rejected after upgrade: the server has no trustworthy owner to infer for an old
instance-wide token. Re-register each one with `--repository` and replace its
saved id/token pair.

### Run

```bash
forgekeep-runner run --config ~/.forgekeep/runner.toml
```

`run` registers on its own when the config file carries no identity yet — which
needs `--auth-token` / `FORGEKEEP_AUTH_TOKEN` for the same reason. Ordinary
settings resolve as **CLI arg > config file > built-in default**. The
`allow_insecure_http` safety exception is additive: explicit `true` in the file
or `--allow-insecure-http` enables it. `forgekeep runner` is a deprecated alias
for `forgekeep-runner run`; it delegates to the same implementation, flag for
flag.

Runner credentials require HTTPS for a remote server. Plaintext HTTP remains
available without an exception only for `localhost`, `127.0.0.0/8`, and `::1`,
which preserves the built-in local-development workflow. A deliberately
plaintext remote deployment must opt in with `--allow-insecure-http` or
`allow_insecure_http = true`; redirects are still restricted to the configured
scheme, host, and effective port.

Stopping the runner is a first-class operation, not a kill: on `SIGTERM` (what
`docker stop`, `docker compose down` and systemd send) or Ctrl-C it drops the job
it is holding, removes that job's container, and deregisters itself. The server
hands the job straight back to the pool, so the next runner picks it up
immediately instead of it waiting out the stuck-job sweep. Give the container at
least a few seconds of stop grace so this can finish.

### `runner.toml`

`register --save` writes this file and `run` reads it. It is also what to edit by
hand when a runner moves to another server:

```toml
server = "https://forge.example.com"
allow_insecure_http = false
runner_id = 7
token = "9f1c…"
repository = "owner/project"
name = "builder-1"
labels = ["docker", "linux", "amd64"]
```

| Key | Flag | Meaning |
|-----|------|---------|
| `server` | `--server` | ForgeKeep base URL (default `http://127.0.0.1:8080`) |
| `allow_insecure_http` | `--allow-insecure-http` | Permit credentials on the configured non-loopback `http://` server (default `false`) |
| `runner_id` | `--runner-id` (`run`) | Identity issued by `register` |
| `token` | `--token` (`run`) | Runner token issued by `register` |
| `repository` | `--repository` | Repository the token may serve, in `owner/name` form; required for registration |
| `name` | `--name` | Display name (default: system hostname) |
| `labels` | `--labels` | What a job's `tags:` is matched against — comma-separated on the CLI, a list in the file |

`runner_id` and `token` are one credential — a token only authenticates the id it
was issued for, so pass both or neither. Unknown keys are rejected outright: a
typo fails the start naming the key rather than being silently ignored.

The file holds a live credential, so keep it owned by the user running the
runner. Inside a container that uid is unrelated to the host user of the same
name: create the file on the host, `chown` it to the container uid, and
bind-mount **the file**, not its directory — a bind-mount whose source is missing
gets a directory created in its place, and the runner then fails with that path.

### Environment

| Variable | Purpose |
|----------|---------|
| `FORGEKEEP_AUTH_TOKEN` | Admin JWT for `register`, and for the auto-registration `run` may do — the environment spelling of `--auth-token` |

---

## MCP server (`forgekeep-mcp`)

`forgekeep-mcp` exposes repositories, issues, pull requests, pipelines and code
search to an AI agent over the
[Model Context Protocol](https://modelcontextprotocol.io). It speaks JSON-RPC on
**stdio** and is meant to be launched by the agent as a subprocess; the HTTP/SSE
transport is not implemented, and `--sse` exits with an error instead of starting
a partial server.

It takes no flags — the whole configuration is three environment variables:

| Variable | Default | Purpose |
|----------|---------|---------|
| `FORGEKEEP_URL` | `http://localhost:8080` | Base URL of the ForgeKeep API |
| `FORGEKEEP_PAT` | _(none)_ | Personal access token, sent as `Authorization: Bearer` |
| `FORGEKEEP_ALLOW_INSECURE_HTTP` | `false` | Explicitly permit the PAT on a non-loopback plaintext HTTP server |

Without `FORGEKEEP_PAT` the server still starts: it logs a warning, and every API
call goes out unauthenticated, so anything non-public fails at the first tool
call. Issue the token from the web UI under user settings.

With a PAT, remote `http://` is rejected before the first request. Loopback HTTP
remains available for local development. Set
`FORGEKEEP_ALLOW_INSECURE_HTTP=true` only for a deliberately plaintext remote
deployment; the redirect policy still prevents the PAT moving to another
scheme, host, or port.

An agent that reads the usual `mcpServers` block:

```json
{
  "mcpServers": {
    "forgekeep": {
      "command": "forgekeep-mcp",
      "env": {
        "FORGEKEEP_URL": "https://forge.example.com",
        "FORGEKEEP_PAT": "…"
      }
    }
  }
}
```

Logs go to stderr so the stdio channel stays clean; `RUST_LOG` selects what is
logged.

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
│   ├── rg-runner/  # CI runner agent  → forgekeep-runner
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

# Backend OpenAPI smoke against a running server. Replays every documented
# operation, so point it at a throwaway instance — it registers a user, sends
# sampled bodies and calls the delete verbs.
BACKEND_URL=http://127.0.0.1:8080 node scripts/openapi-interface-smoke.mjs

# The read-only half of the same script: one anonymous GET per documented path,
# asserting only that a route claims it. This is what CI runs against the built
# image, and it is safe against any instance.
OPENAPI_SMOKE_ROUTING_ONLY=1 BACKEND_URL=http://127.0.0.1:8080 \
  node scripts/openapi-interface-smoke.mjs
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the full workflow and the available
environment toggles.

---

## License

ForgeKeep is proprietary software — see the [LICENSE](LICENSE) file. All rights
reserved by [Yahook](https://github.com/Yahook); no use, copying, modification,
or distribution is permitted without prior written permission. Please read
[NOTICE](NOTICE) for the fork's upstream provenance.
