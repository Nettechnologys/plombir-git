# Changelog

This project records user-facing changes here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and releases use
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Public security reporting and contributor guidance.
- `POST /admin/users/{id}/mfa/reset` and `plombir-git reset-mfa <username>`
  take every second factor (authenticator, backup codes, passkeys) off an
  account whose owner is locked out and sign its sessions out.
- CI secrets can be scoped to an environment. Only jobs that declare the
  environment read them, after its approval when it is protected, and an
  environment's value overrides a repository-wide secret of the same name.
- A fresh instance writes a one-time setup token (`setup_token`, `0600`,
  beside the encryption key; also printed in the startup log). The first
  account, which becomes the administrator, has to present it as
  `setup_token` in `POST /api/v1/users/register` or as an `X-Setup-Token`
  header; the register page asks for it while the instance is empty.
  `GET /api/v1/instance` reports `setup_required`.
- `plombir-git create-admin --username <name> [--email <address>]` creates
  an instance administrator from the shell, or promotes an existing account.
- `[limits]`: opt-in storage ceilings. `repo_quota_mb` caps one
  repository's total across LFS objects, release assets, attachments, CI
  caches and artifacts, packages, and OCI blobs, manifests and the staging of
  open upload sessions (which therefore share one budget), whichever way the
  bytes arrive — an OCI cross-repository mount included: the embedded CI runner, a pull mirror or an import fetching LFS
  objects (an object past the budget is reported as missing, by oid), a fork
  copying them and a fork pull request merge adopting them (both refused with
  `413` before anything moves); `oci_blob_max_mb` caps one OCI upload
  session; `ci_cache_max_entries_per_repo` and
  `release_assets_max_per_release` cap entry counts. Every one is `0`, no
  ceiling, by default, so upgrading changes nothing until a value is set. A
  write that would pass a ceiling answers `413` (an LFS batch object `507`)
  with the used, incoming and limit numbers.

### Changed

- **Breaking:** self-service registration is closed by default. An instance
  that sets neither `[auth].registration` nor `PLOMBIR_GIT_REGISTRATION`
  answers `POST /api/v1/users/register` with `403` once its first account
  exists, and warns about it at every start. Set `registration = "open"` (or
  `PLOMBIR_GIT_REGISTRATION=open`) to let strangers sign up as before; set
  `"closed"` to keep the new behaviour without the warning.
- **Breaking:** creating an SSH, deploy or signing key, a personal or bot
  token, a passkey, an SSO link or a runner token needs a session in sudo
  mode: one that signed in within the last ten minutes or confirmed itself
  through `POST /api/v1/users/me/sudo` (password, the directory password for
  LDAP, plus the second factor when enrolled). An account without a password
  here confirms by signing in again through its SSO provider
  (`POST /api/v1/auth/sso/{slug}/reauthenticate`, which asks an OIDC provider
  for `max_age=600`) or with a passkey. Otherwise these routes answer `403`
  with `reason: "sudo_required"`; the web UI asks and retries on its own.
- **Breaking:** `POST /users/mfa/enable` needs the account `password` on
  every enrolment, the first one included, and `setup`, `enable`, `disable`
  and `backup/regenerate` refuse personal access tokens. An account without a
  password here (SSO, LDAP) sends none and needs sudo mode instead.
- `DELETE /users/passkeys/{id}` refuses personal access tokens.
- **Breaking:** the `sub` claim of CI OIDC tokens is now
  `repo:<owner>/<name>:ref:<ref>`, or `repo:<owner>/<name>:environment:<env>`
  for a job with an environment. Trust policies that match the old
  `repo:<repository id>:pipeline:<id>:job:<id>` (usually as `repo:<id>:*`) stop
  matching, and the token exchange with the cloud provider fails until they are
  rewritten. Match the whole new subject and also require the
  `repository_id` claim; see [CI: OIDC tokens](docs/ci.md#oidc-tokens). Tokens
  also carry `repository`, `repository_owner`, `ref_type`, `environment` and
  `actor`.
- Rolling back the CI secret migration refuses while environment-scoped
  secrets exist, rather than widening them to every job.
- With `[server].external_url` set, its scheme decides whether the session
  cookie is `Secure` and whether HSTS is sent, instead of the listener or
  `X-Forwarded-Proto`. HSTS no longer carries `preload` unless
  `[server].hsts_preload = true`.
- **Breaking:** a `jwt_secret` shorter than 16 characters refuses to start,
  and so does a shorter `--new` key for `rotate-encryption-key`. An at-rest
  key already in use is still only warned about, so it can be rotated away.
  `--jwt-secret` and `--encryption-key` warn that argv is readable by every
  local account.

### Security

- `POST /api/v1/users/login` and `POST /api/v1/users/password/initial`
  answer an account inside its brute-force lock with the same
  `invalid credentials` as a wrong password, and the login checks the
  password before the lock. The lock told anyone who asked five times that
  the name was a real account. An LDAP account that is deactivated is named
  so only after the directory accepted the password.
- `GET /v2/auth/token`, where `docker login` presents a password, carries the
  same credential rate limiter as the web login.
- Organization rights come from membership roles only. The account an
  organization or its repositories name as `owner_id` (the creator) kept
  reading, writing, deleting and transferring the organization's private
  repositories, seeing them in listings and search, and deleting the
  organization after it was removed from it; an `owner`-role member could
  not delete or transfer a repository the row did not name. Deleting or
  transferring an organization repository, and deleting the organization,
  now take the `owner` role (`admin` administers without disposing), and
  push and CI notifications go to a repository's owner only for a personal
  repository. On upgrade, every organization without an `owner`-role member
  gives that role to the account in its `owner_id` — the right that column
  carried until now — and records it in the audit log
  (`org.owner_role_backfill`).
- An organization keeps at least one `owner`. Removing the last one answers
  `409`, and deleting the account of an organization's last owner is refused,
  where membership used to cascade away with it. Only an owner may give the
  `owner` role or remove a member who holds it; an admin gets `403`. When the
  member `owner_id` names leaves, that column and the organization's
  repository rows that named them move to a remaining owner, so a later
  deletion of the departed account cannot cascade into them. An organization
  is created together with its creator's `owner` membership, or not at all.

## [0.1.1] - 2026-10-11

### Added

- First published release of the server, runner, and MCP binary, with Linux
  x86_64 and aarch64 archives and build attestations.
- Release image and source dependency SBOMs.

### Fixed

- Release workflow publication after the v0.1.0 tag failed to produce a
  GitHub release.

See the [v0.1.1 release notes](docs/releases/v0.1.1.md) for upgrade and
rollback guidance. Publication of a release does not establish that a registry
package is publicly accessible.

[Unreleased]: https://github.com/Nettechnologys/plombir-git/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/Nettechnologys/plombir-git/releases/tag/v0.1.1
