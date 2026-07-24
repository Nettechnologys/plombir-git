# ForgeKeep Deployment Guide

Two compose files, pick one:

| File | Data lives in | Settings come from | Use it when |
|------|---------------|--------------------|-------------|
| `docker-compose.yml` | Docker named volume `forgekeep-data` | env vars + the image's default flags | trying ForgeKeep out |
| `docker-compose.hostdir.yml` | host directory `deploy/data/` | `deploy/forgekeep.toml` | running it for real: you want to see, back up and restore the data yourself |

## 🚀 Quick Start — ForgeKeep Application

```bash
cd deploy

# 1. Create runtime environment file
cp .env.example .env
secret="$(openssl rand -hex 32)"
sed -i.bak "s/^FORGEKEEP_JWT_SECRET=.*/FORGEKEEP_JWT_SECRET=${secret}/" .env
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

# 1. Environment: JWT secret + the uid the container should run as.
cp .env.example .env
secret="$(openssl rand -hex 32)"
sed -i.bak "s/^FORGEKEEP_JWT_SECRET=.*/FORGEKEEP_JWT_SECRET=${secret}/" .env
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
| `FORGEKEEP_CORS_ORIGINS` | No | unset |
| `FORGEKEEP_CSP_CONNECT_SRC` | No | unset |

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
| `http_request_duration_seconds` | Histogram | - | Request duration |
| `http_requests_in_flight` | Gauge | - | Current in-flight requests |

### Database Metrics
| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `db_queries_total` | Counter | operation | Total DB queries |
| `db_query_duration_seconds` | Histogram | - | Query duration |

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
| `ci_jobs_running` | Gauge | - | Currently running jobs |
| `ci_job_duration_seconds` | Histogram | - | Job execution duration (runner start→finish) |

> **Note.** All metric families — HTTP, rate-limit, Git, CI/CD, Database,
> Business, and Security — are registered *and* emitted; their panels/alerts are
> live.

### Business Metrics (Phase 22-C)
| Metric | Type | Description |
|--------|------|-------------|
| `forgekeep_users_registered_total` | Counter | User accounts created — self-service registration **and** LDAP/SSO first-login auto-provision (provenance split via `forgekeep_auth_events_total{event="provision",outcome="ldap"\|"sso"}`) |
| `forgekeep_repos_created_total` | Counter | Repos created |
| `forgekeep_repos_deleted_total` | Counter | Repos deleted |
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

### Health Alerts
- **ForgeKeepDown**: Target down for 2+ minutes (critical, pages on-call)
- **HighMemoryUsage**: Memory > 90% for 10+ minutes (warning)
- **LowDiskSpace**: Disk > 85% for 10+ minutes (warning)

## 📋 Dashboard Panels

The main dashboard (`forgekeep-main`) includes:

1. **Request Rate (QPS)** - per-route traffic
2. **P95/P99 Latency** - latency distribution per route
3. **Error Rate** - 4xx/5xx per route
4. **In-Flight Requests** - current load
5. **DB Query Rate** - database load by operation
6. **DB Latency (P95)** - slow query detection
7. **Git Operations** - clone/push/pull rate
8. **CI Pipeline Status** - pie chart of pipeline outcomes
9. **Running CI Jobs** - active CI load
10. **Health Status** - up/down indicator
11. **Memory Usage** - gauge
12. **Disk Usage** - gauge
13. **CPU Usage** - gauge

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
│  :7878/metrics   │  15s      │  :9090          │
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
