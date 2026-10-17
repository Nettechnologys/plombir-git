# Changelog

This project records user-facing changes here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and releases use
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-10-17

This release has breaking changes. Read the
[v0.2.0 release notes](docs/releases/v0.2.0.md) before upgrading: they list
what to check first, the database migrations, and the runtime changes.

### Added

- Public security reporting and contributor guidance.
- With `[server].external_url` set, API responses for repositories, pull
  requests, issues, issue comments, review comments, pipelines and releases
  carry `html_url`, the page a person opens to see them. Without it the field
  is absent.
- MCP `tools/list` annotates every tool with `readOnlyHint`,
  `destructiveHint`, `idempotentHint` and `openWorldHint`, and arguments that
  take one of a few words list them in an `enum`.
- `POST /admin/users/{id}/mfa/reset` and `plombir-git reset-mfa <username>`
  take every second factor (authenticator, backup codes, passkeys) off an
  account whose owner is locked out and sign its sessions out.
- CI secrets can be scoped to an environment. Only jobs that declare the
  environment read them, after its approval when it is protected, and an
  environment's value overrides a repository-wide secret of the same name.
  `PUT /repos/{owner}/{name}/actions/secrets/{secret_name}` takes an optional
  `environment` (absent, `null` or empty keeps the secret repository-wide),
  `DELETE` on the same path takes it as `?environment=`, and both answer `404`
  for an environment the repository does not have.
  `GET /repos/{owner}/{name}/actions/secrets` lists each secret's
  `environment` (`null` when repository-wide). Deleting an environment
  deletes its secrets and names them in the audit entry.
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
  bytes arrive — an OCI cross-repository mount included: the embedded CI
  runner, a pull mirror or an import fetching LFS objects (an object past the
  budget is reported as missing, by oid), a fork copying them and a fork pull
  request merge adopting them (both refused with `413` before anything
  moves); `oci_blob_max_mb` caps one OCI upload session;
  `ci_cache_max_entries_per_repo` and `release_assets_max_per_release` cap
  entry counts. Every one is `0`, no ceiling, by default, so upgrading changes
  nothing until a value is set. A write that would pass a ceiling answers
  `413` (an LFS batch object `507`) with the used, incoming and limit numbers.
- `[mirror].max_clone_size_mb` and `[imports].max_clone_size_mb` cap what one
  mirror clone, one sync's growth of an existing mirror, or one import's
  repository or wiki clone may write to disk, LFS objects fetched by the same
  pass included. Both default to `10240`; `0` removes the ceiling. Past it the
  transfer is stopped, the partial clone removed, and the mirror or import
  fails naming the key; an LFS object that no longer fits is reported as
  missing while those already fetched stay.
- Push mirrors: a repository administrator adds them under Settings → Push
  mirrors or `POST /api/v1/repos/{owner}/{name}/push_mirrors` (`url`,
  `username`, `password`, `sync_on_commit`, `sync_interval_seconds`, `force`),
  and lists, changes and removes them with `GET`, `PATCH` and `DELETE`
  `/push_mirrors[/{id}]`. After every ref move here — a push over HTTP or
  SSH, a web edit, a merge — `refs/heads/*` and `refs/tags/*` are pushed on in
  the background, after the LFS objects the pushed history points at (sent
  through the remote's LFS batch API with the mirror's credential). Mirrors
  without `sync_on_commit` go out on their interval (at least 600 seconds,
  `0` for none), and `POST /push_mirrors/{id}/sync` queues one at once
  (`202`, `409` while the mirror is switched off). By default the remote only
  moves forward and a diverged ref is reported and left alone; `force` makes
  it an exact copy (`--prune`). A failing remote never fails the push here:
  the error is recorded and retried after ten minutes, and an LFS object this
  server lacks or the remote refuses does not hold the refs back but is named
  in the mirror's last error until a later pass delivers it. The remote is
  held to the pull mirror's rules (HTTPS unless
  `[mirror].allow_insecure_http`, the private-address guard, the credential
  encrypted at rest and never returned or logged). At most ten per
  repository (`409`); SSH remotes are not supported yet.
  `plombir_git_push_mirror_max_lag_seconds` and
  `plombir_git_push_mirrors_failing` feed the new `PushMirrorBehind` alert in
  `deploy/prometheus/alerts.yml`.
- The SSH port serves `git-lfs-transfer`, so git-lfs 3.0 and later with an
  SSH remote moves objects and locks (`git lfs lock`, `locks`, `unlock`) over
  SSH, with no HTTPS round trip and no need for `[server].external_url`. A
  session is admitted as `git-upload-pack` (download) or `git-receive-pack`
  (upload) would be, maintenance mode closes uploads, and every uploaded
  object gets the checks it gets over HTTPS: digest against oid, declared
  size, the storage budget. Deploy keys work, read-only ones for downloads
  only, and hold no locks; a forced unlock needs an administrator. An upload
  batch that holds a refused object fails as a whole. Older clients still get
  `git-lfs-authenticate`.
- Shallow clones and fetches (`--depth`, `--shallow-since`,
  `--shallow-exclude`, fetching into or unshallowing a shallow clone) work for
  clients on protocol v0/v1 — git before 2.26, libgit2, JGit, go-git — over
  HTTP and SSH. They used to fail with
  `Server does not support shallow clients`.
- `GET /api/v1/search` also searches pull requests (`type=pulls`, or `pull`,
  `pr`, `prs`), users (`type=users` or `user`) and indexed code
  (`type=code`), and `type=all` now includes all three. Pull requests match on
  title and body, users on username and display name (active accounts only),
  code on the repository's published code index; a code result carries
  `file_path` and, when the query text occurs in the file, `line`. Code search
  finds nothing in a repository until its index has been built with
  `POST /api/v1/ai/repos/{owner}/{name}/index` or
  `plombir-git index-repo <owner/name>`. The search page offers the new kinds.
  The route stays open to anonymous callers, who can now list active accounts
  by name.
- `GET /api/v1/repos/{owner}/{name}/blame/{*path}` (optional `ref`, default
  `HEAD`) attributes each line of a committed text file: `line`, `commit`,
  `author`, `author_time`, `summary`. A binary file answers `400`, a file over
  256 KiB `413`. The file view in the web UI has a blame toggle.
- Organization and instance webhooks: `/api/v1/orgs/{name}/hooks` for an
  organization `admin` or `owner`, `/api/v1/admin/hooks` for instance
  administrators, each with `GET` and `POST`, `GET`, `PATCH` and `DELETE` on
  `/{id}`, `GET /{id}/deliveries` and
  `POST /{id}/deliveries/{delivery_id}/redeliver`. A repository's events go to
  its own hooks, its organization's hooks and every instance hook. The web UI
  manages them in the organization and admin settings.
- Webhook events `issue.edited`, `issue.labeled`, `issue.assigned`,
  `ci.status`, `pull_request.synchronize`, `pull_request.review_submitted`,
  `pull_request.review_requested`, `wiki.created`, `wiki.updated` and
  `wiki.deleted`.
- Webhook `content_type` `slack`, `discord` and `matrix` post a one-line chat
  message (`text`, `content`, or an `m.text` `body`) that names the event,
  plus the repository and status when the payload has them, in place of the
  event JSON.
- `GET /api/v1/repos/{owner}/{name}/packages/oci/images` lists a repository's
  tagged container images (tag, digest, total size, push time; paged with
  `last`), and `DELETE /api/v1/repos/{owner}/{name}/packages/oci/images/{digest}`
  (write access) removes an image and its tags. The registry answers
  `DELETE /v2/{owner}/{repo}/manifests/{reference}` by tag or digest. Both
  answer `202`, and `409` for a manifest an image index still references. The
  repository's packages page lists and deletes images.
- The hourly storage cleanup also collects OCI garbage: blobs that no manifest
  references and stored objects with no database row, once they are 24 hours
  old. `DELETE /repos/{owner}/{name}/actions/retention/expired` runs it for one
  repository and reports `oci_blobs_deleted` and `oci_orphan_objects_deleted`.
- `[ci]` `job_memory`, `job_cpus` and `job_pids` set the `docker run`
  `--memory`, `--cpus` and `--pids-limit` of every job container the built-in
  runner starts; `plombir-git-runner run` takes `--job-memory`, `--job-cpus`
  and `--job-pids` over `runner.toml`'s `job_*` keys. The defaults are the
  previous fixed `2g`, `2` and `512`. A value Docker would refuse stops the
  start naming the key; nothing a workflow writes can raise them.
- `[rate_limit].search_max` / `search_window_secs` (`60` per `60` seconds by
  default) limit `GET /api/v1/search` per client for requests without a
  session or token, even while the global limiter is off; past it the route
  answers `429`. `0` disables it. Behind a reverse proxy it needs
  `[rate_limit].trusted_proxies`, or every anonymous visitor shares one
  budget.
- Every release carries `plombir-git-deploy-<tag>.tar.gz`:
  `deploy/docker-compose.release.yml` with the image pinned to the digest that
  release published, plus the config template, `.env.example`, reverse-proxy
  examples and deployment guide of the same commit. The compose file runs the
  published image as the uid/gid set in `.env` (`user:`), so the data
  directory no longer has to belong to uid 1000.
- The release workflow refuses a tag whose image lacks a subcommand that the
  tag's `README.md` or `deploy/README.md` tells an operator to run.

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
- **Breaking:** `POST /admin/users`, `POST /admin/users/{id}/password-reset`,
  `PATCH /admin/users/{id}` (which also sets `is_admin` and `is_active`),
  `POST /admin/sso/providers` and `PATCH /admin/sso/providers/{id}` need a
  login session in sudo mode as well, and refuse a personal access token with
  `403`. Scripts that drive these routes with a token have to use a session
  instead.
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
- **Breaking:** owner names are unique regardless of case on every backend.
  On SQLite and PostgreSQL `zubrenok` could be registered beside `Zubrenok`;
  registration, organization creation, bots and SSO/LDAP provisioning now
  answer `409` for a name another account or organization holds in any case,
  and the database refuses it too. Pairs that already exist are kept: the
  upgrade gives the name to one holder (an account before an organization,
  then the older row), and startup logs a warning naming the others. Rename
  one of each pair; until it is renamed, the other holder's name stays as it
  is.
- Repository names are unique per owner in any case on every backend:
  creating, forking, renaming or transferring into a name that another
  repository of the namespace holds in another case answers `409`. Pairs that
  already exist are kept and named in a warning at every start; renaming or
  moving one of them is the owner's decision.
- **Breaking:** on MySQL a repository address resolves only as its rows spell
  the owner and the repository. `alice/myrepo` used to reach `Alice/MyRepo`
  through the default collation; git, LFS, release and the other repository
  routes now answer `404` for another spelling, as on SQLite and PostgreSQL.
  The container registry is the one exception (see Fixed).
- **Breaking:** a webhook `content_type` must be `json`, `form`, `slack`,
  `discord` or `matrix`; anything else answers `400` on create and update. A
  `form` hook now posts `payload=<url-encoded JSON>`, where it used to post
  the raw JSON under an `application/x-www-form-urlencoded` header. An
  existing hook whose stored `content_type` is outside the list fails every
  delivery until it is edited.
- A webhook URL may be up to 8 KiB, and longer answers `400`; a signing
  secret may be up to 64 KiB. An event named twice in a hook's list is stored
  once.
- An SSO provider's client secret and LDAP bind password may be up to 4 KiB,
  and every other text field is checked against its column width; a longer
  value answers `400` instead of failing in the database.
- A pull-mirror or import URL and a mirror username may be up to 8 KiB and a
  mirror password up to 4 KiB; longer values answer `400` before anything is
  stored.
- Titles and one-line fields are refused past 255 characters, bodies past
  65 536 characters and a review comment's `path` past 4 096, with a `400`
  naming the field, on every backend. This covers issues, pull requests,
  comments, reviews and review suggestions, releases (`tag_name`, `title`,
  `target_commitish`, `body`), wiki pages, labels and organizations.
  PostgreSQL and SQLite used to store any length.
- **Breaking:** `POST /api/v1/repos/{owner}/{name}/statuses/{sha}` refuses a
  `sha` that is not a full 40-character hexadecimal commit id with `400`; an
  abbreviated id used to be stored. `context` is refused past 255 characters,
  `description` and `target_url` past 500, there and in the external CI
  webhook, where a long value used to be a `500` on PostgreSQL and MySQL
  alike. An LFS lock's `ref.name` is refused past 1 024 characters.
- A workflow whose job name, stage name, rendered matrix job name or rendered
  `concurrency.group` is longer than 255 characters is refused with a request
  error naming the key, on every backend. On MySQL it used to fail the
  trigger as a server error.
- **Breaking:** a CI job variable whose name is not a valid environment
  variable name (letters, digits and `_`, not starting with a digit) refuses
  the pipeline when it is created, naming the job and the variable. It used to
  be ignored at run time. A hyphenated `matrix` key such as `node-version`
  still works and is exported only as `MATRIX_NODE_VERSION`; the raw key is no
  longer set as a variable.
- **Breaking:** a CI job's image is found or pulled by a `docker` client that
  carries the runner operator's environment only, and the container then
  starts from the image's ID with `docker run --pull never`, which needs
  Docker 20.10 or newer, on both the built-in and the external runner. A
  missing image is always pulled with the operator's registry logins and
  credential helpers, never with credentials the job sets. In return every
  `AWS_*` variable (`AWS_PROFILE` included), `CLOUDSDK_*` and
  `GOOGLE_APPLICATION_CREDENTIALS` are ordinary job variables and reach the
  container. With `DOCKER_CONTENT_TRUST=1` in the runner's environment the
  image is pulled again for every job.
- CI job containers, in the embedded runner and in `plombir-git-runner`,
  start with `docker run --init`: Docker's init is PID 1 and reaps the
  processes a job leaves behind, which used to stay zombies until the
  container exited. Their names are `plombir-git-job-<job>-<random>`
  (embedded) and `plombir-git-runner-job-<job>-<random>` (external): two
  runners on one Docker daemon that ran jobs with the same id shared a
  container name, and stopping one removed the other's container.
- `?state=all` on `GET /api/v1/repos/{owner}/{name}/issues`, `.../pulls`,
  `.../milestones`, `/api/v1/ai/repos/{owner}/{name}/issues` and `.../prs`
  lists every row. It, a typo, or a state the listing does not have (`merged`
  on issues) used to answer `200 []`; an unknown value is now `400` naming the
  accepted ones, which the OpenAPI document lists as an `enum`. The MCP
  `ai_list_issues` and `ai_list_prs` tools offer `all`.
- Repository archives (`GET /api/v1/repos/{owner}/{name}/archive/{ref}.zip`
  and `.tar.gz`) of a repository that stores LFS objects carry the files'
  content, streamed from the LFS store, instead of their pointers. A pointer
  whose object is not on the server stays a pointer, and the archive then ends
  with `LFS-OBJECTS-MISSING.txt` naming it. A repository without LFS objects
  gets git's own archive as before.
- A request that carries the session cookie and changes state, or opens the
  notifications WebSocket, is refused with `403` unless its `Origin` is
  `[server].external_url` (without it, the host and port the request arrived
  at) or an exact `PLOMBIR_GIT_CORS_ORIGINS` entry, or the browser sends
  `Sec-Fetch-Site: same-origin` or `none`. Requests without the cookie, safe
  methods, and requests with neither header (git, docker, curl, the CLI) are
  not affected.
- With `[server].external_url` set, its scheme decides whether the session
  cookie is `Secure` and whether HSTS is sent, instead of the listener or
  `X-Forwarded-Proto`. HSTS no longer carries `preload` unless
  `[server].hsts_preload = true`.
- **Breaking:** a `jwt_secret` shorter than 16 characters refuses to start,
  and so does a shorter `--new` key for `rotate-encryption-key`. An at-rest
  key already in use is still only warned about, so it can be rotated away.
  `--jwt-secret` and `--encryption-key` warn that argv is readable by every
  local account.
- Outbound requests to user-supplied addresses (webhooks, mirrors, imports)
  also refuse `192.0.0.0/24`, `198.18.0.0/15`, `240.0.0.0/4`; IPv6 addresses
  that carry an IPv4 one (IPv4-mapped, NAT64 `64:ff9b::/96`, 6to4
  `2002::/16`) are judged by that address; and local-use NAT64
  `64:ff9b:1::/48`, Teredo, IPv4-compatible and -translated, discard-only,
  benchmarking, both documentation ranges (`2001:db8::/32`, `3fff::/20`),
  SRv6 `5f00::/16` and site-local `fec0::/10` are refused whole.
- An OIDC provider's authorization endpoint is held to the same transport
  rule as its token and userinfo endpoints: plain HTTP to a remote host is
  refused unless its origin is listed in `[auth].allow_insecure_oidc_origins`.
- Outbound TLS (webhooks, mirrors, imports, SSO, LDAP) uses rustls only, which
  speaks TLS 1.2 and 1.3: a server that offers nothing newer than TLS 1.1 can
  no longer be reached. reqwest still trusts the system certificate store;
  LDAP moves to `ldap3` 0.12 with rustls and the system roots instead of
  native-tls. curl and native-tls are no longer linked; OpenSSL still is,
  through `webauthn-rs`.
- YAML from pushed repositories and uploaded packages (Gitea Actions
  workflows, `.plombir-git-ci.yml`, gemspec metadata, `Chart.yaml`) is read by
  a pure-Rust parser that caps alias expansion. `serde_yaml` and
  `unsafe-libyaml` are no longer dependencies. A numeric `strategy.matrix`
  dimension in a workflow is accepted under its spelling, as GitHub reads it,
  instead of being refused.
- Package archives (NuGet, PyPI wheels, Composer) are read with zip 9, which
  is built without bzip2: an entry compressed with bzip2 is no longer read.
- `[logging].max_size_mb` (`--log-max-size-mb`) is enforced. Setting it
  switches the log file from daily rotation to size rotation: the file at
  `[logging].file` moves to `<file>.1` (then `.2`, up to `max_files`) once it
  reaches the cap. Left unset, files still rotate daily. `0` refuses to start
  when a log file is configured. It used to be accepted and only warned about.
- Every smart-HTTP and Git LFS request writes one log line — `info` when it
  succeeded, `warn` when it was refused or failed, `429` and `503` included —
  with method, path, the redacted query, status, time to the response head,
  client address and request id. No header is logged.
- A push over HTTP that a token's narrowing kept off a protected branch writes
  one `agent.scope_denied` audit row per refused ref (reason
  `protected_branch`, the branch, the rule and the token), as the REST and MCP
  doors already did.
- `plombir-git-mcp` logs at `info` when `RUST_LOG` is unset, so its warnings
  about a missing token or a token crossing plaintext HTTP reach stderr.
- Pull request diffs and branch comparisons are cached in memory by the
  current branch tips (each cache up to 128 entries and 64 MiB), so a repeated
  view of a fork pull request skips the fetch and patch work.
- The server image's entrypoint is `tini --`, so orphaned git processes (a
  `pack-objects` left by a timed-out `upload-pack`, a `git-remote-https` left
  by a mirror fetch) are reaped instead of piling up as zombies under the
  server. Whatever `CMD` or `docker run <image> …` supplies is still the
  program.
- `deploy/docker-compose.yml` and `deploy/docker-compose.hostdir.yml` run the
  server with `cap_drop: [ALL]` and `no-new-privileges`;
  `docker-compose.hostdir.yml` also mounts the root filesystem read-only with
  a tmpfs at `/tmp`, so everything the server writes has to be under `/data`.
  The observability stack's services get the same settings, and Alertmanager
  keeps its state in a new `alertmanager-data` volume.
- The observability compose file (`deploy/docker-compose.observability.yml`)
  runs Prometheus 3.15.0, Alertmanager 0.34.1, Grafana 13.2.3 and
  node-exporter 1.12.1, pinned by digest. The bundled dashboard's panels are
  Grafana `timeseries` panels (Grafana 12 removed the Angular `graph` panel),
  Grafana starts with `GF_PLUGINS_PREINSTALL_DISABLED=true` so it does not
  download newer bundled plugins on every start, and node-exporter reads udev
  data from the host's `/rootfs/run/udev/data`.
- Building: `rust-version` is 1.89 (it was declared 1.85, which the lockfile
  no longer built with), and CI checks the workspace with exactly that
  toolchain. The pinned toolchain and the builder image are Rust 1.99.0, and
  the image builds the web UI on Node 26.

### Removed

- **Breaking:** the instance no longer serves Swagger UI at `/api-docs`,
  `/api-docs/` and `/api-docs/{path}`. The OpenAPI document stays at
  `/api-docs/openapi.json` under the same sign-in; open it in Swagger UI,
  Redoc or a client generator. The page came from `utoipa-swagger-ui`, whose
  build script downloaded the UI from GitHub at compile time and unpacked it
  with `zip` 2.4.2 (RUSTSEC-2026-0367, -0368, -0369). No release of it
  depends on a fixed `zip`. Building the server no longer needs network
  access for it.

### Fixed

- `deploy/README.md`'s restore, migrate and encryption-key rotation recipes
  ran `docker compose run --rm plombir-git <subcommand>`, which the image
  answers with `executable file not found`: the command has to start with the
  program. They now name the program after the service.
- A job assigned to an external runner keeps the secret values it was issued,
  encrypted with the job, and its log uploads are masked with them as well as
  with the current values. A secret rotated or deleted while the job ran used
  to appear unmasked in that job's log.
- A CI job's log keeps the first 256 KiB and the last 768 KiB of each output
  stream, within the same 1 MiB per-stream budget, with a line counting the
  bytes omitted in between. It used to keep only the end.
- A push from a shallow clone (what CI systems and agents make by default)
  moved the ref and then ended with `unexpected disconnect while reading
  sideband packet`, exit 128, and a branch deletion pushed from one over SSH
  hung until the idle timeout. A shallow history whose boundary parents the
  server lacks is refused as `shallow update not allowed`, with the way out.
- An SSH client paid a one-second delay for every key it offered that the
  server does not know, so a client whose registered key came sixth waited
  five seconds per connection. Only a rejected password now pays a one-second
  floor, from the connection's second authentication request on, and an
  unregistered key is refused before the client is asked to sign with it, so
  a confirming agent or a FIDO key is not prompted for it.
- The built-in HTTPS listener (`[tls]`) panicked at startup with `Could not
  automatically determine the process-level CryptoProvider`, and so did the
  first `ldaps://` login. The server now selects rustls' aws-lc-rs provider at
  startup.
- Git work on slow storage (ref reads, ref advertisement, Protocol V2
  negotiation and object-info, CI configuration reads, default-branch and
  post-push work, imports) runs on the blocking pool and no longer stalls
  unrelated requests.
- Changing a repository's default branch, a mirror sync, a first push and an
  import adopting `HEAD` are serialized across processes, so concurrent
  changes no longer leave Git's `HEAD` and the recorded default branch
  disagreeing. Two imports into the same target are serialized as well.
- A branch or tag created or deleted through the API, and a merged pull
  request's head branch deleted on merge, still fire their webhooks and CI
  work when the client disconnects mid-request.
- The pipeline of a merge commit that the built-in runner unblocks
  (auto-merge, the merge queue) ignored `[ci].runner_labels` and the shutdown
  signal, so on `SIGTERM` its job stayed `running` until the stuck-job sweep.
- The built-in runner wrote workspaces, caches and artifacts under the
  directory two levels above the repository instead of the configured storage
  root the HTTP side reads them from.
- CI cache retention, after the data directory had moved, deleted the cache
  entries and left their archives on disk with nothing pointing at them; it
  now finds archives the way the runner and the download route do.
- A push during a repository's first code index build no longer takes over
  that build before it publishes. `POST /api/v1/ai/repos/{owner}/{name}/index`
  answers `409` (retry) when a newer build superseded it before publication,
  instead of reporting a file count for an index that was never published.
- Search results and the `repo:owner/name` qualifier use an organization
  repository's organization name, where they used the creating account's
  username. `GET /api/v1/search` with a huge `page` answered a database error
  instead of an empty page, and with `per_page` above 100 skipped the rows
  between pages.
- The container registry resolves `owner/repo` by its exact spelling first and
  then case-insensitively, so `docker pull host/zubrenok/myapp` (Docker
  refuses capitals) reaches `Zubrenok/MyApp`. On MySQL, layers and manifests
  pushed under another spelling were stored under a second prefix: a pull by
  the row's spelling answered `404` for layers that were present, and
  deletion and transfer left the copies behind. Storage is now keyed by the
  rows' spelling, and at startup objects under another spelling move to it.
- On MySQL, renaming or transferring a repository addressed in another case
  looked for storage that did not exist, and a transfer addressed in lowercase
  also renamed the repository.
- A refused OCI chunk (a range that did not land as declared, progress that
  could not be recorded, or a final chunk under a digest its bytes do not
  match) stayed in staging, and every later `PATCH` of that upload session
  answered `500`. Staging is now cut back to the acknowledged offset, so a
  corrected retry goes through.
- Two attachments uploaded at the same moment could together pass the
  repository's attachment quota; the later one is now refused after the fact
  and the earlier one kept.
- A pull mirror whose sync failed with a long git error on MySQL (over
  64 KiB) failed to record the failure and was retried on every tick;
  `last_sync_error` is now cut to 16 KiB. A failed first mirror clone no
  longer leaves a directory the next pass took for an existing mirror.
- A finished import whose failure lists were long (a few hundred withheld LFS
  objects) stayed `running` on MySQL until the stale sweep marked it failed.
  The stored stats keep under 48 KiB, dropping entries past that and recording
  a `<list>_omitted` count beside each shortened list.
- On MySQL, the webhook delivery journal kept request and response bodies in
  `VARCHAR(255)` columns, so a delivery with a longer payload was not recorded
  and could not be redelivered. Issue, pull request, comment, review and wiki
  bodies longer than 255 characters, release bodies near 64 KiB, long job
  scripts and runner logs, long notification titles, package metadata from
  client manifests, long branch-protection and runner-label lists, and job
  images pinned by tag and digest failed with `1406 Data too long` and a
  `500`. CI secret values near the 64 KiB limit, webhook signing secrets,
  webhook URLs and event lists, mirror and import URLs, and SSO secrets on
  MySQL and PostgreSQL also get columns that hold every value the API accepts.
- `workflow_dispatch` inputs whose stored JSON exceeds 65 535 bytes are
  refused by `POST /api/v1/repos/{owner}/{name}/pipelines` with a `400` naming
  the largest input; on MySQL they used to be a `500`.
- A repository archive whose git process timed out or failed to read ended
  its body cleanly, so a truncated download could look complete; the body now
  breaks.
- Helm `index.yaml` quotes strings that a YAML 1.1 reader takes for another
  type, so an `appVersion: yes` or a keyword `on` reaches `helm repo update`
  as a string rather than a boolean. Key order and content are unchanged.
- Startup warnings written while the configuration was resolved — plaintext
  credential transports allowed by `[imports]`, `[auth]`, `[mirror]` and
  `[webhooks]`, an open `/metrics` — were emitted before logging existed and
  never reached the log.
- An OIDC authorization endpoint that carries its own query (Azure AD B2C,
  `…/authorize?p=b2c_1_susi`) received the login parameters folded into the
  value of its last parameter.

### Security

- `POST /api/v1/users/login` and `POST /api/v1/users/password/initial`
  answer an account inside its brute-force lock with the same
  `invalid credentials` as a wrong password, and the login checks the
  password before the lock. The lock told anyone who asked five times that
  the name was a real account. An LDAP account that is deactivated is named
  so only after the directory accepted the password.
- `POST /api/v1/users/passkeys/login/finish` answered `account is
  temporarily locked` before verifying the assertion, and `login/start` needs
  only a username, so anyone could read whether an account with a passkey was
  locked. The lock is now checked after the assertion verifies, and a locked
  account gets the same `401 passkey authentication failed` as a bad
  signature.
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
- **Breaking:** the pull-mirror routes — `GET`, `POST`, `PATCH` and `DELETE
  /api/v1/repos/{owner}/{name}/mirror` and `POST .../mirror/sync` — need
  repository administration instead of write access. A mirror pass
  force-moves and prunes every branch past branch protection, so a
  collaborator with write access could point it at their own upstream and
  rewrite a protected `main`. Writers now get `403`, and the settings section
  is shown to administrators only.
- Changing a pull mirror's `url` to another scheme, host or port through
  `PATCH /repos/{owner}/{name}/mirror` kept the stored credential, so the next
  sync sent it to the new host. Such a change now needs its own `password`
  (or an empty one to sync without credentials) and is otherwise answered
  `400`; a new path on the same server keeps the credential.
- A Protocol V2 fetch passed `shallow` and `deepen-not` values unchecked to
  `git rev-list`. `shallow` must now be a full object id and `deepen-not` a
  full object id or plain ref name; anything else is a client error, and the
  command line ends option parsing before the revisions.
- `POST` and `DELETE /api/v1/repos/{owner}/{name}/contents/{*path}` and the
  MCP tools built on them accepted a `.git` path component, which wrote into
  the temporary clone's own repository directory before its `git add` and
  `git commit`. A `.git` component at any depth, in any case, is now refused
  as a client error; `.gitignore`, `.github/` and the like stay editable.
- A CI job's `image:` must be an image reference
  (`[registry[:port]/]path[:tag][@digest]`). A value such as `--privileged`
  or `-v/:/host` was read by `docker run` as a flag; it is now refused when
  the pipeline is created, refused again by both runners, and the image is
  passed after `--`.
- CI job variables no longer reach the host `docker` client's environment.
  The client starts from an allow-listed copy of the runner's environment, and
  a job variable that would reconfigure that client (`PATH`, `HOME`,
  `TMPDIR`, `LANG`, `LC_*`, `LD_*`, `DOCKER_*`, `*_PROXY`, `SSL_CERT_*`, Go
  runtime variables) is not passed to the container; the job log names each
  one dropped. The pipeline still runs.
- Label and board-column colours must be `#rrggbb` and are stored in lower
  case; anything else answers `400`. The old label check accepted any seven
  characters starting with `#` (such as `#0;x:1;`), board columns had no
  check, and the web UI wrote the value into a `style` attribute. GitHub and
  GitLab label imports apply the same rule (GitLab's `#rgb` is expanded); a
  refused colour is listed in `labels_failed` and the other labels still
  import. The web UI also falls back to a neutral colour for rows stored
  before the rule.
- The commit page links a status `target_url` only when it is an absolute
  `http(s)` URL and shows anything else as text; the import history links
  `source_url` the same way. `plombir-git import` (and any caller of the
  import service) refuses a non-git source scheme before the import task is
  recorded, as the HTTP route already did.
- `GET /repos/{owner}/{name}/raw/{*path}` on an LFS pointer takes the
  `Content-Length` of a compressed object from the size verified at upload,
  not from the `size` written in the committed pointer.
- An OCI manifest push is refused with `400` (`MANIFEST_INVALID`) when its
  `Content-Type` differs from the body's `mediaType`, when a body without one
  does not have the shape of the type the header names, or when a config,
  layer or child-manifest descriptor declares a size other than the stored
  one; an image index naming a child manifest the repository does not hold is
  refused with `MANIFEST_BLOB_UNKNOWN`. Nothing is written.
- Deleting a repository removes, in the same transaction, its webhooks and
  their delivery history, CI secrets, deploy keys, pull-mirror settings with
  their stored credentials, and repository runners (their jobs go back to the
  queue). A runner token of a repository deleted earlier answers `401`.
  Deleting an organization removes its webhooks and their delivery history.
- Issue template front matter and `config.yml` are read with a bounded YAML
  parser: an alias bomb becomes one listed template error, and the other
  templates are still served.
- `deploy/Caddyfile.example` and `deploy/nginx.conf.example` answer `404` for
  `/metrics`. Both forwarded it, so a proxy copied from them published
  per-route traffic and the failed-login rate to anyone unless
  `[observability].metrics_token` was set.
- The Docker build context excludes `.env*`, `plombir-git*.toml`, `*.pem`,
  `ssh_host_key*` and `encryption_key` at any depth, so a secret left in the
  checkout cannot reach an image layer.
- `web/package-lock.json` resolved 88 of its 151 packages from
  registry.npmmirror.com, a third-party mirror. All now resolve from
  registry.npmjs.org with unchanged `integrity` values.
- The SSH server takes russh 0.64.1, which fixes a pre-authentication panic
  on a zero Curve25519 share (GHSA-5xvq-cp9x-6p6r), unbounded decompressed
  packets and pre-banner input, an ignored authentication-attempt limit, a
  channel-open flood during rekey and a post-authentication `pty-req` panic.
  gix-packetline 0.21.5 fixes a pre-authentication panic on an empty
  side-band packet (GHSA-2vh6-hw4j-32ww).
- rustls 0.23.46 (RUSTSEC-2026-0346 to 0349). `serde_yaml` and
  `unsafe-libyaml` (RUSTSEC-2024-0449) are gone with the YAML parser change.

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

[Unreleased]: https://github.com/Nettechnologys/plombir-git/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/Nettechnologys/plombir-git/releases/tag/v0.2.0
[0.1.1]: https://github.com/Nettechnologys/plombir-git/releases/tag/v0.1.1
