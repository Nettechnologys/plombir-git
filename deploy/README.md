# IronForge Deployment Guide

## 🚀 Quick Start — IronForge Application

```bash
cd deploy

# 1. Create runtime environment file
cp .env.example .env
secret="$(openssl rand -hex 32)"
sed -i.bak "s/^IRONFORGE_JWT_SECRET=.*/IRONFORGE_JWT_SECRET=${secret}/" .env
rm -f .env.bak

# 2. Start IronForge
docker compose up -d

# 3. Check status
docker compose ps
docker compose logs -f
```

Access: **http://localhost:8080**

### Environment variables (used with default CMD):
| Variable | Required | Default |
|----------|----------|---------|
| `IRONFORGE_JWT_SECRET` | **Yes** | set in `deploy/.env` |
| `IRONFORGE_CORS_ORIGINS` | No | unset |
| `IRONFORGE_CSP_CONNECT_SRC` | No | unset |

For a separately hosted frontend, set `IRONFORGE_CORS_ORIGINS` to the browser
origin. IronForge also adds those origins, plus matching `ws://` or `wss://`
origins, to CSP `connect-src`. Use `IRONFORGE_CSP_CONNECT_SRC` only for extra
API/WebSocket origins not covered by CORS.

### Volumes
| Path | Purpose |
|------|---------|
| `/data` | Repos, SQLite DB, logs (persistent) |

### Runtime Binaries

The Docker image includes all runtime binaries:

| Binary | Purpose |
|--------|---------|
| `ironforge` | Main server and admin CLI |
| `ironforge-runner` | Standalone CI runner agent |
| `ironforge-mcp` | MCP stdio server |

### SQLite Backup / Restore

Backups use SQLite `VACUUM INTO`, so they can be taken while IronForge is
running:

```bash
docker compose exec ironforge sh -lc \
  'mkdir -p /data/backups && ironforge backup-db \
    --db-url "sqlite:///data/ironforge.db?mode=rw" \
    "/data/backups/ironforge-$(date +%Y%m%d-%H%M%S).db"'
```

Restore requires the main service to be stopped so the database file is not in
use:

```bash
docker compose stop ironforge
docker compose run --rm ironforge restore-db \
  --db-url "sqlite:///data/ironforge.db?mode=rwc" \
  --force \
  /data/backups/ironforge-YYYYMMDD-HHMMSS.db
docker compose up -d ironforge
```

### Ports
| Port | Protocol |
|------|----------|
| 8080 | HTTP |
| 2222 | SSH Git |

---

## 📊 Observability Stack (Phase 22-C)

## Overview

This is a production-grade observability stack for IronForge, providing:

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

Prometheus scrapes the app at `ironforge:8080` through the shared Docker
network `ironforge-net`; start the main IronForge compose service first.

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
| `git_operations_total` | Counter | operation | clone/push/pull count |
| `git_operation_duration_seconds` | Histogram | - | Git op duration |

### CI/CD Metrics
| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `ci_pipelines_total` | Counter | status | Pipeline count by status |
| `ci_jobs_total` | Counter | status | Job count by status |
| `ci_jobs_running` | Gauge | - | Currently running jobs |

### Business Metrics (Phase 22-C)
| Metric | Type | Description |
|--------|------|-------------|
| `ironforge_users_registered_total` | Counter | User registrations |
| `ironforge_repos_created_total` | Counter | Repos created |
| `ironforge_repos_deleted_total` | Counter | Repos deleted |
| `ironforge_repos_forked_total` | Counter | Repos forked |
| `ironforge_issues_opened_total` | Counter | Issues opened |
| `ironforge_issues_closed_total` | Counter | Issues closed |
| `ironforge_prs_opened_total` | Counter | PRs opened |
| `ironforge_prs_merged_total` | Counter | PRs merged |
| `ironforge_stars_total` | Counter | Stars given |
| `ironforge_webhook_deliveries_total` | Counter (labels: status) | Webhook deliveries |
| `ironforge_ws_connections` | Gauge | Active WS connections |
| `ironforge_users` | Gauge | Total registered users |
| `ironforge_repositories` | Gauge | Total non-deleted repos |

## 🔔 Alert Rules

### HTTP Alerts
- **HighErrorRate**: 5xx rate > 5% for 5+ minutes (critical)
- **SlowRequestDuration**: P95 > 1s for 10+ minutes (warning)
- **HighInFlightRequests**: > 100 in-flight for 5+ minutes (warning)

### Database Alerts
- **SlowDatabaseQueries**: P95 query > 500ms for 10+ minutes (warning)
- **HighDatabaseQPS**: > 1000 QPS for 5+ minutes (info)

### Git Alerts
- **SlowGitClone**: P95 clone > 30s for 15+ minutes (warning)
- **HighGitOperationFailure**: Git 5xx > 0.1 req/s (critical)

### CI/CD Alerts
- **HighPipelineFailureRate**: > 30% failure for 30+ minutes (warning)
- **CIJobQueueBuildup**: > 50 jobs running for 15+ minutes (warning)

### Health Alerts
- **IronForgeDown**: Target down for 2+ minutes (critical, pages on-call)
- **HighMemoryUsage**: Memory > 90% for 10+ minutes (warning)
- **LowDiskSpace**: Disk > 85% for 10+ minutes (warning)

## 📋 Dashboard Panels

The main dashboard (`ironforge-main`) includes:

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
│  IronForge       │ ────────▶ │  Prometheus     │
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
