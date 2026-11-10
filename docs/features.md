# Features and stack

[← README](../README.md)

## Features

| Area | Capability | Implementation |
|------|------------|----------------|
| Git transport | Smart Protocol V1 & V2 over HTTPS and SSH (`russh`); pkt-line, sideband-64k, packfile encode/decode | [V2 transport](../crates/rg-git/src/protocol/v2.rs) |
| Repositories | Create / delete / transfer, fork, private & public, collaborators (read/write/admin), file & tree browsing, commit log, branches, tags, pull and push mirrors | [Repository API](../crates/rg-http/src/api/repos.rs) |
| Auth | User registration & login (argon2 + JWT), SSH public-key auth, Personal Access Tokens, MFA, SSO, remembered browser sessions | [Auth API](../crates/rg-http/src/api/auth.rs) |
| Issues | Create, view, edit and delete issues and comments; close/reopen, labels, milestones, Gitea-compatible issue templates and chooser ([template reference](issue-templates.md)) | [Issue API](../crates/rg-http/src/api/issues.rs) |
| Pull requests | Diff, three merge strategies (merge / squash / rebase), cross-repo (fork) PRs, code review (approve / request changes / inline comments), branch protection, repository pull-request template ([reference](issue-templates.md#pull-request-templates)), CODEOWNERS auto-review ([reference](codeowners.md)) | [PR API](../crates/rg-http/src/api/pulls.rs) |
| Wiki | Markdown page CRUD with revision history stored in the database (no `.wiki.git` repository) | [Wiki API](../crates/rg-http/src/api/wiki.rs) |
| Git LFS | Batch API, object upload/download, zstd compression; works from both the HTTPS and the SSH clone URL ([details](using-git.md#git-lfs)) | [LFS API](../crates/rg-http/src/api/lfs.rs) |
| CI/CD | Native `.plombir-git-ci.yml` pipelines ([schema reference](ci.md)) and Gitea Actions (`.forgejo/workflows/*.yml`, `.gitea/workflows/*.yml` or `.github/workflows/*.yml`, the first the commit has, [supported subset](gitea-actions.md)); embedded or external runners, optional Docker execution, artifacts, live job logs over WebSocket | [CI API](../crates/rg-http/src/api/ci.rs) |
| Webhooks | Registration, delivery, HMAC-SHA256 signatures, delivery history | [Webhook API](../crates/rg-http/src/api/webhooks.rs) |
| Notifications | In-app, per-event email (SMTP) and Telegram, security alerts, and real-time WebSocket delivery | [Notification delivery](../crates/rg-core/src/notification/thread.rs) |
| Registries | Package registry and OCI container registry | [Package API](../crates/rg-http/src/api/packages/mod.rs) |
| Search | Global search of repositories, issues, wiki, PRs, users and indexed code; repository code search in the browser and AI API (build a code index explicitly before the first search) | [Search API](../crates/rg-http/src/api/search.rs) |
| Import | Pull repositories, issues, PRs, labels, milestones, release records and wiki from GitHub / GitLab / Gitea / Forgejo; issues and PRs keep their source numbers, and every PR head commit is kept even when its branch is gone; a PR's comments on lines of the diff come over under the review they were written in, on the line, side and commit the source placed them; who an issue or PR was assigned to, and whose review it asked for, is kept as the source's logins beside it; a GitHub, Gitea or Forgejo release brings its files, each stored under the SHA-256 of what arrived (checked against the one GitHub lists) and paid for out of the repository's `[limits]` and the import's clone budget, and a file that could not be brought is named in the task's stats with the reason — a GitLab release's links are not fetched; imported release authorship belongs to the importing account; a finished import answers a `receipt` (`GET /api/v1/imports/{id}`) that sets the source's listing beside what the repository holds — branches and tags by SHA, the default branch by its head commit, issues, PRs, labels, milestones, releases, comments, reviews and LFS objects — names what is missing, and lists what no import can bring (secret values, branch protection, webhooks, reactions) | [Import service](../crates/rg-core/src/import/service.rs) |
| Operations | TLS/HTTPS, TOML config, rate limiting, log rotation, unified pagination, registered GPG/SSH commit-signature verification, audit log, health checks | [Server startup](../crates/rg-cli/src/serve.rs) |
| AI agents | Model Context Protocol built into the server (`POST /api/v1/mcp`) and as a stdio binary (`plombir-git-mcp`); bot accounts, narrowed tokens, per-bot rate limits and audit ([details](mcp.md#mcp-over-http-and-agent-accounts)) | [MCP API](../crates/rg-http/src/api/mcp.rs) |
| Releases | Release assets with optional in-toto / DSSE attestations signed by the instance key | [Release API](../crates/rg-http/src/api/releases.rs) |
| Web UI | SvelteKit (Svelte 5) SPA (login, repos, issues, PRs, wiki, CI, review, orgs, notifications, search and file blame), English, Chinese and Russian i18n | [App layout](../web/src/routes/+layout.svelte) |

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

## Merge queue

The queue builds a speculative merge against the current base branch and waits
for its CI before merging. A pull request from a fork requires explicit CI approval of its current
head by an active human maintainer with write access to the base repository,
even if no CI is configured. Enqueueing by a human or bot does not grant that
approval. A changed head or an approver who no longer meets those requirements
fails the entry and cancels its previous pipeline. After approving the current
head, enqueue the pull request again.

Auto-merge uses the same approval check and remains pending until the current
fork head has been approved. A new push needs a new approval before that
automatic merge can complete. If the request to enable auto-merge returns an
error, its attempt is cleared. A failed background attempt can retry, but cannot
restore an authorization the user has canceled.

## Release drafts

Draft releases and their notes, files and provenance attestations are visible
only to repository writers. Other readers receive the same `404` as for a
missing release or asset; lists and totals omit drafts. Publishing makes them
visible to repository readers, and returning a release to draft hides them
again. Release API responses, including downloads, use `private, no-store`.
A CI job token does not grant writer permissions: a workflow reading a draft
needs a session or PAT with repository write access.

Configured repository, organization and system webhooks remain trusted
integrations. The existing `release.created` event includes draft metadata;
this change does not remove that delivery to configured webhook destinations.

## LDAP names

Accounts, organizations and bots share an owner namespace. Creating a bot or
organization checks enabled LDAP directories before taking a locally free name,
so a directory member can still sign in for the first time. A reserved name
returns the same `409` as a locally occupied name. As with registration, this
lets a caller learn that a name is reserved without distinguishing a local
account from a directory member. If a configured directory
cannot answer, creation fails with `502`; each directory lookup has a ten-second
timeout. Instances without LDAP do not make a network request for this check.
The host-side `create-admin` command remains a trusted operator action and does
not query LDAP.

Organization visibility accepts only `public` or `private`. An invalid PATCH
changes nothing, including other fields supplied with it. Unknown values from
older data are treated as member-only; startup warns about the first 100
affected organizations so an owner can choose a valid visibility explicitly.

## Browser sessions

Deactivating an account revokes its existing sessions and pending address-change
links. Reactivation does not restore them. PATs, SSH keys and passkeys are
suspended while the account is inactive and become usable after reactivation.

The login form offers **Remember me for 30 days** for password, passkey and SSO
sign-in, including accounts with a second factor. Checked sessions renew on
page navigation, at most once every five minutes, for thirty days of inactivity.
A session requires a new login after ninety days even when used every day.
Background notification polling does not renew it, and renewal does not extend
the ten-minute sudo window for sensitive account changes.

With the checkbox clear, the auth cookie has no persistent lifetime and the
signed token expires within seven days. Browsers may restore session cookies
when restoring a previous browsing session; use **Sign out** on a shared device.
Sign out invalidates the account's sessions on every device, as before.
Existing sessions and API clients that omit `remember_me` keep their previous
seven-day policy; a fresh login is needed to select the new behavior.

Renewal requires the current HttpOnly session cookie, a remembered login and
same-origin checks. A bearer token or PAT alone cannot renew a session. Deploy
the browser UI and API on the same origin: a cross-site UI configured through
`PLOMBIR_GIT_CORS_ORIGINS` cannot renew a `SameSite=Strict` cookie that its browser
withholds. No weaker cookie setting or bearer refresh fallback is introduced.

A live browser cookie and an explicit signed session credential must name the
same account. Conflicting credentials are refused before the requested action
runs (`401`, API reason `identity_conflict`). The UI rechecks the browser's
account and discards the old account's forms; it does not repeat the action or
sign out the newly selected account automatically. A proxy's HTML `401` is not
treated as confirmation of sign-out.

This check applies to mixed session credentials. A tab sending only its cookie
uses the account currently selected in that browser. Use separate browser
profiles to keep simultaneous accounts independent.

## Sensitive account actions and MFA

For an account with MFA enabled, changing or disabling its authenticator,
regenerating backup codes, changing its password or recovery email, and deleting
its account require a recent proof of the **existing** factor. Password alone
cannot remove MFA. The shared confirmation dialog accepts password plus TOTP or
an unused backup code, or an existing passkey with user verification. Provider
reauthentication still completes the account's MFA challenge. Confirmation lasts
10 minutes; remembered-session renewal never extends it.

Changing or resetting a password (including an administrator reset), or an
operator reset of login factors, cancels pending account-email changes.
Following an address-change link consumes it and changes the address in one
transaction; a failed write leaves the link available until expiry or
revocation. Completing an address change invalidates reset links sent to the
previous address. Link issuance rechecks the account snapshot under the same
lock, so a request already in flight cannot recreate an invalidated link.
Signing out alone does not cancel pending address changes.

An operator factor reset revokes sessions, enrolled and pending TOTP material,
unused backup codes, passkeys and pending address changes in one commit with
its notifications. A failure rolls back the whole reset. Passkey login and
step-up completion require the account generation proved before the assertion;
a factor reset during the ceremony cannot produce a session after that reset.

The same check protects issuance of personal and bot tokens, SSH and signing
keys, repository deploy keys, passkey registration and removal, SSO linking,
runner registration, agent-run profiles, and administrator actions that create
users, grant administrator status, reset credentials or configure providers.
PATs cannot use these login-session-only routes. Ordinary Git/API automation and
emergency revocation of PATs and SSH/signing/deploy keys retain their existing
permission checks; they do not acquire interactive MFA prompts.

Fresh proof is bound to the enrolled authenticator. A password-only session
issued before MFA enrolment, or a session that proved a retired authenticator,
must confirm again before a sensitive action. Existing MFA sessions issued
before this change also confirm once. Rotating the encryption key for stored
TOTP secrets similarly retires their sudo proof. Sessions remain usable for
ordinary work; enabling MFA does not sign out other sessions. Use sign-out to
revoke all browser sessions if an account may already be compromised.

First enrolment still requires the local password and a code from the new
secret, or provider/passkey sudo for a passwordless account. Promotion checks
the exact pending secret and account generation in the same transaction as the
backup-code set. Concurrent setup cannot replace the verified secret. After
initial enrolment or rotation, confirm with the new factor when next asked.
Losing both the authenticator and all recovery methods requires administrator
recovery; knowing the password alone is deliberately insufficient.

The security page reads the explicit MFA-enabled flag, independently of backup
code history. Disabling MFA revokes unused codes; used rows remain as history
and do not mean the factor is still enabled. The backup status API returns
`mfa_enabled` and `replacement_count` alongside the existing counts; `total`
includes historical used rows. The page shows unused codes and used-code
history separately, and uses the configured replacement count for re-issuance.

Repository and organization access administration also requires a login session
with fresh proof: collaborators, membership and teams, visibility changes,
transfer/deletion, webhooks, pull/push mirror configuration, branch/tag protection,
CI secrets and deployment environment policy. Administrator unlock/deletion of
accounts, organizations, SSO providers and runners, and deletion of a bot with its
repositories have the same guard. The existing resource/role check runs first;
inaccessible private resources retain their uniform 404 response.

For an MFA account the proof must cover its **current** factor, even if a password
was entered recently. A completed MFA login/step-up or verified passkey starts the
ten-minute window; another prompt is unnecessary while that proof remains fresh.
After it expires, the SPA asks for confirmation and retries the refused action once.
PATs cannot perform these security-administration writes, even with `repo` or
`admin` scope. This is an API compatibility change in 0.4.1. Mixed metadata and
visibility patches are refused as a whole; supplying a visibility field requires
proof even if its current value happens to match. Ordinary description/default
branch/name edits, Git/API work, CI runtime operations and CI variable automation
retain their role/scope checks. Maintenance/banner settings and emergency credential
revocation retain their existing policies.

Removing an organization member clears that organization's team grants in the same
transaction. A later invitation cannot restore old team privileges. Adding someone
to a team requires current organization membership, serialized against removal;
legacy dormant grants are discarded on re-invitation and excluded from team lists.
Direct repository collaborator grants remain separate and must be revoked separately.

Repository transfer preserves collaborators, keys, webhooks, mirrors, CI secrets,
variables, environments and protection rules. Review those integrations when moving
ownership; old/new owners and collaborators receive security notices according to
their settings. Revoking repository access cannot retract copies already made,
including private forks and local clones. A private fork creates a security notice
for the source owner; it grants no continuing access to new source changes.

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
