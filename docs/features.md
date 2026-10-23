# Features and stack

[← README](../README.md)

## Features

| Area | Capability | Implementation |
|------|------------|----------------|
| Git transport | Smart Protocol V1 & V2 over HTTPS and SSH (`russh`); pkt-line, sideband-64k, packfile encode/decode | [V2 transport](../crates/rg-git/src/protocol/v2.rs) |
| Repositories | Create / delete / transfer, fork, private & public, collaborators (read/write/admin), file & tree browsing, commit log, branches, tags, pull and push mirrors | [Repository API](../crates/rg-http/src/api/repos.rs) |
| Auth | User registration & login (argon2 + JWT), SSH public-key auth, Personal Access Tokens, MFA, SSO | [Auth API](../crates/rg-http/src/api/auth.rs) |
| Issues | Create, view, edit and delete issues and comments; close/reopen, labels, milestones, Gitea-compatible issue templates and chooser ([template reference](issue-templates.md)) | [Issue API](../crates/rg-http/src/api/issues.rs) |
| Pull requests | Diff, three merge strategies (merge / squash / rebase), cross-repo (fork) PRs, code review (approve / request changes / inline comments), branch protection, repository pull-request template ([reference](issue-templates.md#pull-request-templates)), CODEOWNERS auto-review ([reference](codeowners.md)) | [PR API](../crates/rg-http/src/api/pulls.rs) |
| Wiki | Markdown page CRUD with revision history stored in the database (no `.wiki.git` repository) | [Wiki API](../crates/rg-http/src/api/wiki.rs) |
| Git LFS | Batch API, object upload/download, zstd compression; works from both the HTTPS and the SSH clone URL ([details](using-git.md#git-lfs)) | [LFS API](../crates/rg-http/src/api/lfs.rs) |
| CI/CD | Native `.plombir-git-ci.yml` pipelines ([schema reference](ci.md)) and Gitea Actions (`.forgejo/workflows/*.yml`, `.gitea/workflows/*.yml` or `.github/workflows/*.yml`, the first the commit has, [supported subset](gitea-actions.md)); embedded or external runners, optional Docker execution, artifacts, live job logs over WebSocket | [CI API](../crates/rg-http/src/api/ci.rs) |
| Webhooks | Registration, delivery, HMAC-SHA256 signatures, delivery history | [Webhook API](../crates/rg-http/src/api/webhooks.rs) |
| Notifications | In-app, email (SMTP), and real-time WebSocket delivery | [Notification delivery](../crates/rg-core/src/notification/thread.rs) |
| Registries | Package registry and OCI container registry | [Package API](../crates/rg-http/src/api/packages/mod.rs) |
| Search | Global search of repositories, issues, wiki, PRs, users and indexed code; repository code search in the browser and AI API (build a code index explicitly before the first search) | [Search API](../crates/rg-http/src/api/search.rs) |
| Import | Pull repositories, issues, PRs, labels, milestones, release records and wiki from GitHub / GitLab / Gitea / Forgejo; issues and PRs keep their source numbers, and every PR head commit is kept even when its branch is gone; a PR's comments on lines of the diff come over under the review they were written in, on the line, side and commit the source placed them; who an issue or PR was assigned to, and whose review it asked for, is kept as the source's logins beside it; a GitHub, Gitea or Forgejo release brings its files, each stored under the SHA-256 of what arrived (checked against the one GitHub lists) and paid for out of the repository's `[limits]` and the import's clone budget, and a file that could not be brought is named in the task's stats with the reason — a GitLab release's links are not fetched; imported release authorship belongs to the importing account; a finished import answers a `receipt` (`GET /api/v1/imports/{id}`) that sets the source's listing beside what the repository holds — branches and tags by SHA, the default branch by its head commit, issues, PRs, labels, milestones, releases, comments, reviews and LFS objects — names what is missing, and lists what no import can bring (secret values, branch protection, webhooks, reactions) | [Import service](../crates/rg-core/src/import/service.rs) |
| Operations | TLS/HTTPS, TOML config, rate limiting, log rotation, unified pagination, registered GPG/SSH commit-signature verification, audit log, health checks | [Server startup](../crates/rg-cli/src/serve.rs) |
| AI agents | Model Context Protocol built into the server (`POST /api/v1/mcp`) and as a stdio binary (`plombir-git-mcp`); bot accounts, narrowed tokens, per-bot rate limits and audit ([details](mcp.md#mcp-over-http-and-agent-accounts)) | [MCP API](../crates/rg-http/src/api/mcp.rs) |
| Releases | Release assets with optional in-toto / DSSE attestations signed by the instance key | [Release API](../crates/rg-http/src/api/releases.rs) |
| Web UI | SvelteKit (Svelte 5) SPA (login, repos, issues, PRs, wiki, CI, review, orgs, notifications, search and file blame), English + Chinese i18n | [App layout](../web/src/routes/+layout.svelte) |

Databases: SQLite, PostgreSQL, and MySQL are all supported and selected at
runtime from the scheme of `--db-url` / `[database].url` — no feature rebuild
required.

Commit signatures are checked against public keys registered at **Settings →
Commit Signing Keys** (or `GET/POST/DELETE /api/v1/users/signing-keys`). GPG
and SSH signing keys are separate from SSH login keys. Registering one requires
a login session and a verified account email; the commit's committer email must
match that account email. Unknown keys receive an undeterminable signature
status and cannot satisfy a branch's **Require signed commits** rule. The Docker
image includes GnuPG and checks signatures with a temporary, instance-owned
keyring and SSH allowed-signers file; the host account's keyring is never a
source of trust. Configure email confirmation before enabling the rule on a
fresh instance, then register at least one signing key.

## Tech stack

| Layer | Choice | Version |
|-------|--------|---------|
| Async runtime | tokio | 1.x |
| HTTP | axum + axum-server | 0.8 / 0.8 |
| SSH server | russh | 0.60 |
| Git objects | gix (gitoxide) + `git` CLI gateway | 0.84 |
| ORM | SeaORM (SQLite / PostgreSQL / MySQL) | 1.1 |
| Auth | argon2 + JWT | — |
| TLS | rustls + tokio-rustls | 0.23 / 0.26 |
| Logging | tracing + tracing-appender | 0.1 / 0.2 |
| CLI | clap | 4.x |
| Frontend | SvelteKit (adapter-static, SPA) + Svelte | 2 / 5 |
| Coverage | cargo-llvm-cov | — |

> Git object operations use a hybrid of `gix` and a `GitCommandGateway`: pack,
> rebase, archive and some diff/GPG paths still shell out to the `git` CLI
> while the corresponding `gix` capabilities mature.
