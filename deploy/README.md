# ForgeKeep Deployment Guide

Two compose files, pick one:

| File | Data lives in | Settings come from | Use it when |
|------|---------------|--------------------|-------------|
| `docker-compose.yml` | Docker named volume `forgekeep-data` | env vars + the image's default flags | trying ForgeKeep out |
| `docker-compose.hostdir.yml` | host directory `deploy/data/` | `deploy/forgekeep.toml` | running it for real: you want to see, back up and restore the data yourself |

## 🚀 Quick Start — ForgeKeep Application

```bash
cd deploy

# 1. Create runtime environment file. Two secrets: one signs tokens, one
#    encrypts data at rest — see "Secrets and rotation" below for why they are
#    separate and why you want both set from day one.
cp .env.example .env
secret="$(openssl rand -hex 32)"
sed -i.bak "s/^FORGEKEEP_JWT_SECRET=.*/FORGEKEEP_JWT_SECRET=${secret}/" .env
sed -i.bak "s/^FORGEKEEP_ENCRYPTION_KEY=.*/FORGEKEEP_ENCRYPTION_KEY=${secret}/" .env
rm -f .env.bak

# 2. Start ForgeKeep
docker compose up -d

# 3. Check status
docker compose ps
docker compose logs -f
```

Access: **http://localhost:8080**

---

## 🏠 Production Setup — config file + host directory

`docker-compose.hostdir.yml` bind-mounts `deploy/data/` onto `/data` and runs
`forgekeep serve --config /app/forgekeep.toml`. Everything the server persists —
repos, the SQLite DB, the audit archive and the generated SSH host key — lands
in that one host directory. Logs go to stdout, so `docker compose logs` and
your log driver work as usual; set `[logging].file` if you want them in
`data/logs/` instead.

```bash
cd deploy

# 1. Environment: the two secrets + the uid the container should run as.
cp .env.example .env
secret="$(openssl rand -hex 32)"
sed -i.bak "s/^FORGEKEEP_JWT_SECRET=.*/FORGEKEEP_JWT_SECRET=${secret}/" .env
sed -i.bak "s/^FORGEKEEP_ENCRYPTION_KEY=.*/FORGEKEEP_ENCRYPTION_KEY=${secret}/" .env
rm -f .env.bak
printf 'FORGEKEEP_UID=%s\nFORGEKEEP_GID=%s\n' "$(id -u)" "$(id -g)" >> .env

# 2. Config file. It MUST exist before `up` — see the bind-mount trap below.
cp forgekeep.docker.toml forgekeep.toml

# 3. Data directory, owned by the uid from step 1.
mkdir -p data

# 4. Build + start (the build bakes FORGEKEEP_UID into the image).
docker compose -f docker-compose.hostdir.yml up -d --build

# 5. Verify both listeners are up.
docker compose -f docker-compose.hostdir.yml logs -f
curl -sf http://127.0.0.1:8080/health && echo OK
ssh -p 2222 -o StrictHostKeyChecking=no git@localhost 2>&1 | head -1
```

HTTP is published on `127.0.0.1:8080` only — put a TLS-terminating reverse
proxy in front of it. Git-over-SSH listens on `0.0.0.0:2222`.

`forgekeep.toml` and `data/` are git-ignored, so a rebuilt container keeps
reading the settings and data you edited on the host.

### The two traps this layout avoids

**1. Bind-mounting a file that does not exist yet.** Docker silently creates a
*directory* at the mount point. Mounting a missing `forgekeep.toml` therefore
gives the server a directory to parse — which is why step 2 above copies the
file into place *before* `up`. For the same reason the SSH host key is **not**
mounted from the host here: `[server].host_key = "/data/ssh_host_key"` puts it
inside the data directory, where the server generates it on first start and it
persists across rebuilds.

**2. uid mismatch on the bind-mount.** The container's `forgekeep` user is not
the host user of the same name — only the numeric uid matters. Three ways out,
in order of preference:

```bash
# a. Build the image as your host user (what step 1 does).
printf 'FORGEKEEP_UID=%s\nFORGEKEEP_GID=%s\n' "$(id -u)" "$(id -g)" >> .env
docker compose -f docker-compose.hostdir.yml up -d --build

# b. Keep the image default (uid 1000) and chown the host directory.
sudo chown -R 1000:1000 ./data

# c. Check what the image actually runs as, if you inherited it from someone.
docker compose -f docker-compose.hostdir.yml run --rm --entrypoint id forgekeep
```

Note that `FORGEKEEP_UID` is a **build** argument: after changing it you must
`up --build`, not just `restart`.

### Troubleshooting a failed first start

Every one of these is a startup error that names the path and, for permission
failures, prints the uid to `chown` to.

| Log line | Cause | Fix |
|----------|-------|-----|
| `config file … is a directory, not a file` | mounted a `forgekeep.toml` that did not exist | `rm -rf forgekeep.toml && cp forgekeep.docker.toml forgekeep.toml` |
| `repo_root … Permission denied` + `this process runs as uid=…` | `data/` owned by a different uid | `chown` to the uid from the message, or rebuild with `FORGEKEEP_UID` |
| `SQLite database … is not writable` | same, for the DB and its `-wal`/`-shm` sidecars | as above — the *directory* must be writable, not just the file |
| `SSH host key … Permission denied` | key file readable only by another uid | `chown <uid> data/ssh_host_key && chmod 600 data/ssh_host_key` |
| `SSH host key path … is a directory` | bind-mounted a host key file that did not exist | remove the directory and let the server generate the key |
| `audit archive_dir … is unusable` | `[audit].archive_dir` not writable by the container uid | `chown` it as above, point the key elsewhere, or set `[audit].enabled = false` |
| `backup dir … is unusable` | `[backup].dir` not writable by the container uid | as above, or set `[backup].enabled = false` |
| `scheduled database backups … cannot run on the Postgres backend` | `[backup].enabled = true` on a non-SQLite database | set `[backup].enabled = false` and schedule `pg_dump` / `mysqldump` instead |
| HTTP works, SSH silent | SSH failed on its own; HTTP is unaffected by design | `docker compose logs \| grep 'SSH server error'` |

### Backup / restore

The whole instance is `deploy/data/`. Stop the container and copy the
directory, or take a hot SQLite backup with the commands in the
[SQLite Backup / Restore](#sqlite-backup--restore) section below (add
`-f docker-compose.hostdir.yml` to each `docker compose` invocation).

### Environment variables (used with default CMD):
| Variable | Required | Default |
|----------|----------|---------|
| `FORGEKEEP_JWT_SECRET` | **Yes** | set in `deploy/.env` |
| `FORGEKEEP_ENCRYPTION_KEY` | Strongly recommended | `[auth].key_file` — a durable key file the server creates on first start |
| `FORGEKEEP_CORS_ORIGINS` | No | unset |
| `FORGEKEEP_CSP_CONNECT_SRC` | No | unset |
| `FORGEKEEP_REGISTRATION` | No (set it before exposing the port) | `open` — `[auth].registration` |

### Who may create an account

`FORGEKEEP_REGISTRATION=closed` (or `[auth].registration = "closed"`, the env
var wins) makes `POST /api/v1/users/register` answer `403` before it hashes a
password or writes anything. Leave it at `open` and the endpoint accepts anyone
who can reach it — `[rate_limit].auth_max` throttles that to 10 accounts per
60 seconds but never refuses, so it is not a substitute on an instance meant
for a handful of people.

Two things `closed` still admits, on purpose:

* **The first account.** An instance that has never had a user accepts exactly
  one registration, otherwise a closed instance could never be initialised. Do
  that registration before the port is reachable by anyone else — the window is
  open until it is used.
* **LDAP / SSO first-login provisioning.** It is a separate channel
  (`forgekeep_auth_events_total{event="provision"}`) with its own switch, per
  provider — see below. `FORGEKEEP_REGISTRATION` does not reach it in either
  direction.

A value neither `open` nor `closed` fails the start with the accepted spellings
named, rather than booting an instance you believe is closed.

### Who may get an account through an identity provider

Each row in **Admin → Settings → SSO Providers** carries its own provisioning
policy, because "who may sign in" and "who may be *created*" are different
questions on a public IdP, where everyone already holds a valid identity:

* **Create accounts on first login** (`auto_provision`). Off means only people
  who already have an account here can use this provider; a stranger with a
  perfectly valid identity at it gets a `403` naming the reason, and no row in
  `users`. A provider you create today starts **off** — handing out accounts is
  something you turn on deliberately.
* **Allowed email domains** (`allowed_email_domains`). Comma-separated, empty
  for no restriction. The match is on the exact domain of the address the
  provider asserts: `example.com` admits `alice@example.com` and admits neither
  `mail.example.com` nor `evil-example.com`.

Both only govern account *creation*. Turning provisioning off never locks out an
account that already exists, whether it was linked to the provider or matched on
its email — so the switch is safe to flip on a live instance.

> **Upgrading.** Providers that already existed keep `auto_provision = true`, so
> an upgrade changes nothing about who can log in. If one of them is a public
> IdP (`github.com`, `google.com`), that is the setting to revisit first: with it
> on, anyone with an account *there* can have one *here*, and
> `FORGEKEEP_REGISTRATION=closed` does not change that.

### Secrets and rotation

`FORGEKEEP_JWT_SECRET` signs tokens. The at-rest key encrypts TOTP secrets, CI
secrets, mirror and LDAP passwords, SSO client secrets, and OAuth tokens. On a
normal first start the server generates `/data/encryption_key`, mode `0600`, and
keeps reusing it from the bind-mounted data directory. It is deliberately not
derived from the JWT secret, so rotating `FORGEKEEP_JWT_SECRET` only invalidates
sessions and never needs a second manual `.env` step.

`FORGEKEEP_ENCRYPTION_KEY` still wins over the file for an external KMS or
vault. `[auth].key_file` changes its location; by default it is beside
`[server].host_key`. The startup marker in the database makes a substituted or
deleted key file fail immediately with the recovery source named, rather than
letting MFA, CI and mirror operations fail later. A blank
`FORGEKEEP_JWT_SECRET=` in `.env` counts as unset, not as "the empty secret".

If the encryption key itself leaks, move the database onto a new one with the
server stopped. The `docker compose stop` below is not advisory: on a
file-backed SQLite database the command refuses to start while a ForgeKeep
server holds it — `--dry-run` too, since the dry run walks the same rows under
the same write lock and only rolls back at the end.

```bash
docker compose stop forgekeep
docker compose run --rm forgekeep rotate-encryption-key \
    --db-url "sqlite:///data/forgekeep.db?mode=rwc" \
    --old "$OLD_KEY" --new "$NEW_KEY" --dry-run   # reports, changes nothing
docker compose run --rm forgekeep rotate-encryption-key \
    --db-url "sqlite:///data/forgekeep.db?mode=rwc" \
    --old "$OLD_KEY" --new "$NEW_KEY" --yes
# then replace /data/encryption_key with the new value (mode 0600),
# or update FORGEKEEP_ENCRYPTION_KEY in .env for an external key source
docker compose up -d forgekeep
```

`docker compose run` replaces the image's own command, so the database has to be
named here: without `--db-url` (or `--config /app/forgekeep.toml`, if you deploy
with a config file) the command falls back to `sqlite://./forgekeep.db?mode=rwc`
under `WORKDIR /app` and re-encrypts an empty file it just created. The same
applies to every admin subcommand below.

Keep the old key until the server has come up under the new one — it is what
opens anything the pass reported as unreadable.

For a separately hosted frontend, set `FORGEKEEP_CORS_ORIGINS` to the browser
origin. ForgeKeep also adds those origins, plus matching `ws://` or `wss://`
origins, to CSP `connect-src`. Use `FORGEKEEP_CSP_CONNECT_SRC` only for extra
API/WebSocket origins not covered by CORS.

### Volumes
| Path | Purpose |
|------|---------|
| `/data` | Repos, SQLite DB, logs (persistent) |

### Runtime Binaries

The Docker image includes all runtime binaries:

| Binary | Purpose |
|--------|---------|
| `forgekeep` | Main server and admin CLI |
| `forgekeep-runner` | Standalone CI runner agent |
| `forgekeep-mcp` | MCP stdio server |

### SQLite Backup / Restore

**The server backs itself up.** `deploy/forgekeep.docker.toml` ships with:

```toml
[backup]
enabled = true
dir = "/data/backups"
interval_hours = 24
keep_last = 7
```

A background task inside the server takes a `VACUUM INTO` snapshot every
`interval_hours` and deletes everything but the newest `keep_last`, so there is
no cron entry to forget on one particular host — and the snapshot is taken from
the pool the server is already using, which is the one thing a manual
`backup-db` cannot guarantee. Snapshots are named
`forgekeep-<UTC timestamp>-<uuid>.db`; rotation only ever deletes files matching
that exact shape, so a hand-made backup left in the same directory is safe.

Things worth knowing before you rely on it:

| | |
|-|-|
| **Disk** | Budget `keep_last` × the size of `/data/forgekeep.db`. |
| **Startup check** | An unwritable `dir` fails the start with the path and the uid, rather than surfacing a day later as a warning. Set `enabled = false` if you do not want scheduled backups. |
| **Non-SQLite** | `enabled = true` on PostgreSQL/MySQL fails the start by design — `VACUUM INTO` cannot snapshot them. Schedule `pg_dump` / `mysqldump` and leave this off. |
| **Not covered** | `/data/repos`. A database backup restores users, issues, PRs and settings; the git data is a separate artifact. Snapshot the whole `/data` volume, or decide explicitly that bare repos are recoverable from developer clones. |
| **Monitoring** | `forgekeep_db_backup_last_success_timestamp_seconds` and `forgekeep_db_backups_total{status}`. The `BackupTooOld` / `BackupRunsFailing` rules in `deploy/prometheus/alerts.yml` read them. |
| **Verify it** | Restore one. A backup that has never been restored is a file, not a backup — use the `restore-db` recipe below against a throwaway `--db-url`. |

The first snapshot of a process is due from the newest file already in `dir`,
not from the boot, and never sooner than a minute after start: a server
restarted more often than `interval_hours` still gets backed up, and one stuck
in a crash loop does not snapshot on every boot and rotate the good copies out.

#### Taking one by hand

Backups use SQLite `VACUUM INTO`, so they can be taken while ForgeKeep is
running:

```bash
docker compose exec forgekeep sh -lc \
  'mkdir -p /data/backups && forgekeep backup-db \
    --db-url "sqlite:///data/forgekeep.db?mode=rw" \
    "/data/backups/forgekeep-$(date +%Y%m%d-%H%M%S).db"'
```

Restore requires the main service to be stopped so the database file is not in
use:

```bash
docker compose stop forgekeep
docker compose run --rm forgekeep restore-db \
  --db-url "sqlite:///data/forgekeep.db?mode=rwc" \
  --force \
  /data/backups/forgekeep-YYYYMMDD-HHMMSS.db
docker compose up -d forgekeep
```

If you deploy with a config file, pass `--config /app/forgekeep.toml` instead of
`--db-url`: every DB-touching subcommand (`migrate`, `rebuild-fts`, `backup-db`,
`restore-db`, `rotate-instance-key`, `rotate-encryption-key`, `import`,
`index-repo`, `package list`) reads `[database].url` from it, so the admin
command and the server cannot end up pointed at two different databases.
Passing **neither** falls back to `sqlite://./forgekeep.db?mode=rwc` relative to
the container's `WORKDIR /app` — an empty database that nothing else ever opens,
which is why the flag matters for `backup-db` in particular.

CLI commands that can apply pending migrations against file-backed SQLite use
the same offline contract as restore. `migrate`, `import`, and `package list`
enforce it through a process lease, so stop the service before running any of
them. For example:

```bash
docker compose stop forgekeep
docker compose run --rm forgekeep migrate --config /app/forgekeep.toml
docker compose up -d forgekeep
```

The `.forgekeep.lock` sidecar next to the database is persistent by design; do
not delete it as a stale PID file. The lock itself is owned by the OS and is
released automatically when the server or migration process exits.

On PostgreSQL and MySQL there is no sidecar and nothing to stop: a server
backend has no cross-process schema cache, so migrations stay online. They are
still serialised, in the database rather than the filesystem —
`pg_advisory_lock` / `GET_LOCK` — because applying two migration runs at once
makes them collide inside `CREATE TABLE`. Whoever arrives second waits for the
first (up to five minutes) and then applies whatever is left, which is usually
nothing. The lock is held by a database session, so a killed migrator releases
it immediately and the next boot is not blocked by a leftover.

### Ports
| Port | Protocol |
|------|----------|
| 8080 | HTTP |
| 2222 | SSH Git |

---

## 📊 Observability Stack (Phase 22-C)

## Overview

This is a production-grade observability stack for ForgeKeep, providing:

- **Metrics**: Prometheus scrapes `/metrics` every 15s
- **Alerting**: Alertmanager routes alerts by severity (critical/warning/info)
- **Visualization**: Grafana dashboards (auto-provisioned)
- **Host metrics**: Node Exporter for CPU/memory/disk

## 🚀 Quick Start

```bash
# Start the stack
cd deploy
docker compose -f docker-compose.observability.yml up -d

# Check status
docker compose -f docker-compose.observability.yml ps

# Access points
# - Prometheus:  http://localhost:9090
# - Grafana:     http://localhost:3000 (admin/admin)
# - Alertmanager: http://localhost:9093

# View logs
docker compose -f docker-compose.observability.yml logs -f
```

Prometheus scrapes the app at `forgekeep:8080` through the shared Docker
network `forgekeep-net`; start the main ForgeKeep compose service first.

## 📈 Available Metrics

### HTTP Metrics
| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `http_requests_total` | Counter | method, route, status | Total HTTP requests |
| `http_request_duration_seconds` | Histogram | route | Request duration |
| `http_requests_in_flight` | Gauge | - | Current in-flight requests |

### Database Metrics
| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `db_queries_total` | Counter | operation | Total DB queries |
| `db_query_duration_seconds` | Histogram | operation | Query duration |

### Git Metrics
| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `git_operations_total` | Counter | operation (`fetch`\|`push`) | Authorized upload-pack/receive-pack count |
| `git_operation_duration_seconds` | Histogram | - | Git op duration (fetch + push) |

### CI/CD Metrics
| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `ci_pipelines_total` | Counter | status | Pipeline count by terminal status |
| `ci_jobs_total` | Counter | status | Job count by outcome (success/failure/error) |
| `ci_jobs_running` | Gauge | - | Currently running jobs — **sampled** from the `running` rows every 60s by the gauge sink, so a job shorter than one sampling interval may never appear in it (`ci_jobs_total` is the throughput series) |
| `ci_job_duration_seconds` | Histogram | - | Job execution duration (runner start→finish) |

> **Note.** All metric families — HTTP, rate-limit, Git, CI/CD, Database,
> Business, and Security — are registered *and* emitted; their panels/alerts are
> live. The CI/CD family is emitted by **both** executors: the external runner
> through its `finish` handler, and the embedded in-process runner
> (`ci.external_runners = false`, the default) through the
> `rg_core::metrics_hook` observers. Until `card_e309fbb5a3fd` only the external
> path produced any of it, so a default instance answered zero for every CI
> series while its builds ran.

### Business Metrics (Phase 22-C)
| Metric | Type | Description |
|--------|------|-------------|
| `forgekeep_users_registered_total` | Counter | User accounts created — self-service registration **and** LDAP/SSO first-login auto-provision (provenance split via `forgekeep_auth_events_total{event="provision",outcome="ldap"\|"sso"}`; refusals are a separate series, `event="provision_refused",outcome=<rule>`, and never count as an account) |
| `forgekeep_repos_created_total` | Counter | Repos created |
| `forgekeep_repos_deleted_total` | Counter | Repos deleted — the REST endpoint **and** the cascades that retire repositories without one of their own (deleting an organization or an account), so this counter and the `forgekeep_repositories` gauge describe the same event |
| `forgekeep_repos_forked_total` | Counter | Repos forked |
| `forgekeep_issues_opened_total` | Counter | Issues opened |
| `forgekeep_issues_closed_total` | Counter | Issues closed |
| `forgekeep_prs_opened_total` | Counter | PRs opened |
| `forgekeep_prs_merged_total` | Counter | PRs merged |
| `forgekeep_stars_total` | Counter | Stars given |
| `forgekeep_webhook_deliveries_total` | Counter (labels: status) | Webhook deliveries |
| `forgekeep_ws_connections` | Gauge | Active WS connections |
| `forgekeep_users` | Gauge | Total registered users |
| `forgekeep_repositories` | Gauge | Total non-deleted repos |

## 🔔 Alert Rules

### HTTP Alerts
- **HighErrorRate**: 5xx rate > 5% for 5+ minutes (critical)
- **SlowRequestDuration**: P95 > 1s for 10+ minutes (warning)
- **HighInFlightRequests**: > 100 in-flight for 5+ minutes (warning)

### Database Alerts
- **SlowDatabaseQueries**: P95 query > 500ms for 10+ minutes (warning)
- **HighDatabaseQPS**: > 1000 QPS for 5+ minutes (info)

### Git Alerts
- **SlowGitFetch**: P95 git op > 30s for 15+ minutes (warning)
- **HighGitOperationFailure**: Git 5xx > 0.1 req/s (critical)

### CI/CD Alerts
- **HighPipelineFailureRate**: > 30% failure for 30+ minutes (warning)
- **CIJobQueueBuildup**: > 50 jobs running for 15+ minutes (warning)

### Health Alerts
- **ForgeKeepDown**: Target down for 2+ minutes (critical, pages on-call)
- **HighMemoryUsage**: Memory > 90% for 10+ minutes (warning)
- **LowDiskSpace**: Disk > 85% for 10+ minutes (warning)

### Backup Alerts
- **BackupTooOld**: No successful backup for 36+ hours (critical)
- **BackupRunsFailing**: One or more scheduled backup failures in the last hour (warning)

## 🔭 Distributed Tracing (OpenTelemetry)

Beyond Prometheus metrics, ForgeKeep can export **distributed traces** over
OTLP/HTTP (protobuf) to any OpenTelemetry collector (Tempo, Jaeger, the OTel
Collector, Honeycomb, …). Tracing is **opt-in** and independent of `/metrics`.

Enable it via `[observability]` in `forgekeep.toml` or the standard env vars:

```toml
[observability]
otlp_endpoint = "http://localhost:4318"   # /v1/traces is appended automatically
# service_name = "forgekeep"
# sample_ratio = 1.0                       # 0.0..=1.0 head sampling
```

```bash
# Equivalent via the standard OpenTelemetry environment variables:
export OTEL_EXPORTER_OTLP_ENDPOINT="http://localhost:4318"
export OTEL_SERVICE_NAME="forgekeep"
```

With no endpoint configured, none of the tracing machinery runs. When enabled,
each HTTP request produces an `http_request` span (method, uri, status,
request_id) plus any nested `tracing` spans, and the W3C `traceparent` header is
honoured so traces stitch across services. Spans are batched on a background
thread and flushed on graceful shutdown.

## 📋 Dashboard Panels

The main dashboard (`forgekeep-main`) includes:

- Grafana panel `1`: **📊 Request Rate (QPS)** — per-route traffic
- Grafana panel `2`: **⏱️ P95 Request Latency** — p95/p99 latency distribution per route
- Grafana panel `3`: **❌ Error Rate (5xx)** — 4xx/5xx per route
- Grafana panel `4`: **🚀 In-Flight Requests** — current load
- Grafana panel `5`: **💾 DB Query Rate** — database load by operation
- Grafana panel `6`: **🐢 P95 DB Latency** — slow query detection
- Grafana panel `7`: **📦 Git Operations** — clone/push/pull rate
- Grafana panel `8`: **🔄 CI Pipeline Status** — pie chart of pipeline outcomes
- Grafana panel `9`: **⚙️ Running CI Jobs** — active CI load
- Grafana panel `10`: **💚 Health Status** — up/down indicator
- Grafana panel `11`: **🧠 Memory Usage** — gauge
- Grafana panel `12`: **💽 Disk Usage** — gauge
- Grafana panel `13`: **🖥️ CPU Usage** — gauge

## 🔧 Configuration

### Environment Variables
```bash
# Grafana admin
GRAFANA_ADMIN_USER=admin
GRAFANA_ADMIN_PASSWORD=your-secure-password

# Alertmanager (set in alertmanager.yml)
SLACK_WEBHOOK_URL=https://hooks.slack.com/services/...
PAGERDUTY_SERVICE_KEY=your-pagerduty-key
```

### Adding New Metrics

In `crates/rg-http/src/metrics.rs`:

```rust
// 1. Add metric in the appropriate module
pub static MY_METRIC: OnceLock<IntCounter> = OnceLock::new();

// 2. Register in register() function
let m = IntCounter::with_opts(Opts::new("my_metric", "Help text"))?;
MY_METRIC.set(m.clone()).map_err(...)?;
registry.register(Box::new(m))?;
```

In `crates/rg-http/src/metrics.rs` (recorder module):

```rust
pub fn my_event() {
    if let Some(c) = business::MY_METRIC.get() {
        c.inc();
    }
}
```

In API handler:
```rust
metrics::recorder::my_event();
```

## 🔗 Architecture

```
┌──────────────────┐  scrape   ┌─────────────────┐
│  ForgeKeep       │ ────────▶ │  Prometheus     │
│  :8080/metrics   │  15s      │  :9090          │
└──────────────────┘           └────────┬────────┘
                                        │
                                        ▼
┌──────────────────┐           ┌─────────────────┐
│  Node Exporter   │ ────────▶ │  Alertmanager   │
│  :9100           │           │  :9093          │
└──────────────────┘           └────────┬────────┘
                                        │
                ┌───────────────────────┼───────────────────────┐
                ▼                       ▼                       ▼
        PagerDuty                 Slack                  Email
```

## 📚 References

- [Prometheus docs](https://prometheus.io/docs/)
- [Grafana provisioning](https://grafana.com/docs/grafana/latest/administration/provisioning/)
- [Alertmanager](https://prometheus.io/docs/alerting/latest/alertmanager/)
