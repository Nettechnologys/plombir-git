# CI runner

[← README](../README.md)

## CI runner (`plombir-git-runner`)

Out of the box the server executes CI jobs itself: in Docker when `[ci].docker`
is on, or as a shell on the host only when `[ci].allow_host_runner` explicitly
allows it. `plombir-git-runner` moves that work to a build machine. It is a
separate binary: it registers once, then polls the server for jobs whose `tags:`
its own labels satisfy and executes them — natively, or in a container when the
job names an image (see [docs/ci.md](ci.md)).

Either way, a job's image is pulled by the operator's own `docker` client —
your registry logins and credential helpers, never the job's variables — and the
container then starts from the image's ID with `--pull never` (Docker 20.10 or
newer). An image already present is reused as `docker run` would; with
`DOCKER_CONTENT_TRUST=1` in the runner's environment it is pulled again for
every job, so its signature is checked each time. Rootful and rootless Docker
both work; on a rootless daemon the runner must run as the daemon's user (see
[`image`](ci.md#image)).

The server hands jobs to registered runners only with `external_runners = true`
under `[ci]`. With the default `false` it keeps running every job in-process, and
a registered runner polls without ever being given one.

### Register

Registration mints the runner's identity, and it needs an **admin user's JWT** —
not a runner token, which is what registration produces. Pass it as
`--auth-token`, or as `PLOMBIR_GIT_AUTH_TOKEN` to keep the secret out of the
process list. A runner token outlives the session that registered it, so the
JWT must be in sudo mode: a login from the last ten minutes, or the token
`POST /api/v1/users/me/sudo` answers after confirming the password:

```bash
PLOMBIR_GIT_AUTH_TOKEN="$ADMIN_JWT" plombir-git-runner register \
  --server https://forge.example.com \
  --repository owner/project \
  --name builder-1 \
  --label docker --label linux --label amd64 \
  --save --config ~/.plombir-git/runner.toml
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
plombir-git-runner run --config ~/.plombir-git/runner.toml
```

`run` registers on its own when the config file carries no identity yet — which
needs `--auth-token` / `PLOMBIR_GIT_AUTH_TOKEN` for the same reason. Ordinary
settings resolve as **CLI arg > config file > built-in default**. The
`allow_insecure_http` safety exception is additive: explicit `true` in the file
or `--allow-insecure-http` enables it. `plombir-git runner` is a deprecated alias
for `plombir-git-runner run`; it delegates to the same implementation, flag for
flag.

Runner credentials require HTTPS for a remote server. Plaintext HTTP remains
available without an exception only for `localhost`, `127.0.0.0/8`, and `::1`,
which preserves the built-in local-development workflow. A deliberately
plaintext remote deployment must opt in with `--allow-insecure-http` or
`allow_insecure_http = true`; redirects are still restricted to the configured
scheme, host, and effective port.

Stopping the runner is a first-class operation, not a kill: on `SIGTERM` (what
`docker stop`, `docker compose down` and systemd send) or Ctrl-C it drops the job
it is holding, removes that job's container, and tells the server it is going
offline. The server hands the job straight back to the pool, so the next runner
picks it up immediately instead of it waiting out the stuck-job sweep. Give the
container at least a few seconds of stop grace so this can finish.

A stop keeps the registration, as GitHub's self-hosted runner and `act_runner`
do: after a restart or a reboot the runner starts again with the same
`runner.toml` and the same token. Only an administrator removing the runner
(`DELETE /api/v1/admin/runners/{id}`, or the admin page) revokes its token.

### `runner.toml`

`register --save` writes this file and `run` reads it. It is also what to edit by
hand when a runner moves to another server:

```toml
server = "https://forge.example.com"
state_permissions = "owner-only"
allow_insecure_http = false
runner_id = 7
token = "9f1c…"
repository = "owner/project"
name = "builder-1"
labels = ["docker", "linux", "amd64"]
job_memory = "2g"
job_cpus = "2"
job_pids = 512
```

| Key | Flag | Meaning |
|-----|------|---------|
| `server` | `--server` | Plombir Git base URL (default `http://127.0.0.1:8080`) |
| `state_permissions` | config only | Creation policy inherited by workspaces, caches, artifacts and job processes (default `owner-only`; shared-group alternatives: `group-readable`, `group-writable`) |
| `allow_insecure_http` | `--allow-insecure-http` | Permit credentials on the configured non-loopback `http://` server (default `false`) |
| `runner_id` | `--runner-id` (`run`) | Identity issued by `register` |
| `token` | `--token` (`run`) | Runner token issued by `register` |
| `repository` | `--repository` | Repository the token may serve, in `owner/name` form; required for registration |
| `name` | `--name` | Display name (default: system hostname) |
| `labels` | repeatable `--label` | What a job's `tags:` is matched against — one exact value per flag, a list in the file |
| `job_memory` | `--job-memory` (`run`) | Memory ceiling of every job container, as `docker run --memory` takes it (default `2g`) |
| `job_cpus` | `--job-cpus` (`run`) | CPU quota of every job container, as `docker run --cpus` takes it (default `2`) |
| `job_pids` | `--job-pids` (`run`) | Process limit of every job container — the fork-bomb guard, never unlimited (default `512`) |

Use `--label 'gpu,a100'` when a label contains a comma; repeated `--label`
occurrences remain separate list elements. The old comma-separated `--labels`
form remains accepted for compatibility, but cannot express a comma inside one
label and cannot be combined with `--label`.

`runner_id` and `token` are one credential — a token only authenticates the id it
was issued for, so pass both or neither. Unknown keys are rejected outright: a
typo fails the start naming the key rather than being silently ignored.

The three `job_*` limits are the operator's ceiling for every job this runner
executes in a container, checked when the runner starts. A workflow cannot raise
them — nothing in a job reaches `docker run`'s options, and `container.options`
is refused. To give heavy builds more room, run a second runner with bigger
limits and its own label (`--label big --job-memory 16g --job-cpus 8`), and let
the jobs that need it ask for that label in `runs-on:` / `tags:`. Without these
keys every job runs under the same `2g` / `2` CPUs / `512` processes it always
has.

`state_permissions = "owner-only"` makes ordinary files start as `0600` and
directories as `0700`, even when the service manager launched the runner with a
wide umask. Use `"group-readable"` for `0640`/`0750`, or
`"group-writable"` for `0660`/`0770`, when a second operator-owned account must
reach the runner's state through a shared group. No supported mode grants
access to every local account. Explicit credential writers remain stricter:
`runner.toml` is always `0600`, regardless of this baseline.

The file holds a live credential, so `register --save` writes it `0600` and
`run` refuses one that carries any group or world permission bit — a runner
token claims jobs and receives their secrets, so a readable `runner.toml` hands
the runner's place to every other account on the host. A file edited by hand
into `0644` is fixed with `chmod 600 ~/.plombir-git/runner.toml`, which is what
the refusal says.

Keep it owned by the user running the
runner. Inside a container that uid is unrelated to the host user of the same
name: create the file on the host, `chown` it to the container uid, and
bind-mount **the file**, not its directory — a bind-mount whose source is missing
gets a directory created in its place, and the runner then fails with that path.

### Environment

| Variable | Purpose |
|----------|---------|
| `PLOMBIR_GIT_AUTH_TOKEN` | Admin JWT for `register`, and for the auto-registration `run` may do — the environment spelling of `--auth-token` |
