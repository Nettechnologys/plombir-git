# ForgeKeep — Feature Inventory

> Живой реестр того, **что реально реализовано** в кодовой базе, с упором на
> security-поверхность. Цель — чтобы перед «а давайте добавим X» можно было за
> 30 секунд проверить, нет ли X уже, и в каком он состоянии. Обновлять при
> добавлении/изменении фичи. Ссылки вида `crate/файл:строка` кликабельны из
> индекса.
>
> Легенда статуса: ✅ есть · 🟡 частично · ❌ нет · 🔒 security-релевантно

_Последняя сверка с кодом: 2026-07-23._

---

## 1. Git transport

| Фича | Статус | Где | Заметки |
|------|--------|-----|---------|
| Smart Protocol V1 + V2 | ✅ | `rg-git/src/protocol/` | pkt-line, sideband-64k, packfile encode/decode |
| HTTPS transport | ✅ | `rg-http/src/lib.rs` | axum + `axum-server`/rustls при TLS |
| SSH transport | ✅ | `rg-ssh/` (`russh`) | public-key auth |
| Лимит размера тела git-запроса | ✅ 🔒 | `rg-http/src/routes.rs` | RequestBodyLimit на git-роутах |
| git CLI gateway с timeout+kill | ✅ 🔒 | `rg-git/src/cli_gateway.rs` | синхронный `run()` — timeout + kill child (default 120s) |
| Wall-clock bound на стриминг-пути (pack-objects/index-pack) | ✅ 🔒 | `rg-http/src/git_http.rs::with_git_timeout` | upload-pack (V1+V2) и receive-pack обёрнуты в `tokio::time::timeout([timeouts].git_stream_secs, деф. 300, 0=off)`; по таймауту future дропается → `kill_on_drop` убивает git → 504 `GIT_TIMEOUT`. SSH-транспорт того же класса (`rg-ssh` exec spawn) пока не покрыт → см. card |

## 2. Аутентификация и авторизация

| Фича | Статус | Где | Заметки |
|------|--------|-----|---------|
| Регистрация/логин (argon2 + JWT HS256) | ✅ 🔒 | `rg-core/src/auth/jwt.rs`, `rg-core/src/user/service.rs` | |
| Personal Access Tokens (SHA-256 hash, scoped) | ✅ 🔒 | `rg-http/src/pat_auth.rs`, `pat_scope.rs` | скоупы по семействам API |
| SSH public-key auth | ✅ 🔒 | `rg-ssh/` | |
| MFA / TOTP | ✅ 🔒 | `totp-rs`, `rg-core/src/auth/` | |
| SSO / LDAP / OAuth (PKCE) | ✅ 🔒 | `rg-core/src/auth/`, `ldap3` | |
| CI OIDC — Ed25519 short-lived JWT + JWKS | ✅ 🔒 | `rg-core/src/auth/ci_oidc.rs` | **Ed25519-ключ уже в системе** — переиспользуем для подписи артефактов |
| Санитизация internal-ошибок в ответах (H-05) | ✅ 🔒 | `rg-http/src/error.rs` | internal → generic message |
| Защита регистрации от спама (invite/approval/captcha) | ⚠️ 🔒 | `rg-http/src/routes.rs` | открытая саморегистрация; барьер — always-on per-route rate-limit на `/register`+`/login` (10/60s по умолчанию); invite/approval/captcha пока нет |

## 3. Anti-abuse / rate limiting

| Фича | Статус | Где | Заметки |
|------|--------|-----|---------|
| Rate limiter (fixed-window token bucket, per-IP) | ✅ 🔒 | `rg-http/src/rate_limit.rs` | глобальный один слой; по умолчанию `max=0` (выкл.) |
| Trusted-proxy resolve (XFF/X-Real-IP по allowlist IP) | ✅ 🔒 | `rg-http/src/rate_limit.rs:140` | |
| `max_keys` cap на карту клиентов | ✅ 🔒 | `rg-http/src/rate_limit.rs` | новый ключ отвергается ДО вставки при заполнении; амортизированный inline-sweep протухших ≤1×/сек; default 100k, `[rate_limit] max_keys` |
| Per-route / per-endpoint лимиты (register/login) | ✅ 🔒 | `rg-http/src/routes.rs` | отдельный, более жёсткий лимитер per-route на `/users/register` + `/users/login`; всегда включён по умолчанию (10/60s), `[rate_limit] auth_max`/`auth_window_secs` |
| Per-route лимит на git-push | ❌ 🔒 | — | push бьётся только глобальным лимитом; отдельный лимитер не заведён |
| CAPTCHA / proof-of-work | ❌ | — | сознательно НЕ портируем iCaptcha (внешний сервис); альтернатива — hashcash PoW |

## 4. Исходящие сетевые вызовы (webhooks / mirrors)

| Фича | Статус | Где | Заметки |
|------|--------|-----|---------|
| Outbound webhook доставка | ✅ | `rg-core/src/webhook/service.rs:169` | |
| HMAC-SHA256 подпись исходящих (`X-Hub-Signature-256`) | ✅ 🔒 | `rg-core/src/webhook/service.rs:188` | GitHub-совместимо |
| Timeout на исходящем HTTP-клиенте | ✅ 🔒 | `rg-core/src/net.rs`, `auth/sso.rs` | общий `outbound_client()` с `timeout(30s)` + `connect_timeout(10s)`; все 7 SSO/OIDC call-site'ов идут через него |
| Запрет редиректов / SSRF-защита (private/loopback IP) | ✅ 🔒 | `rg-core/src/net.rs`, `webhook/service.rs`, `auth/sso.rs` | `redirect(Policy::none())` + `guard_outbound_url()` (резолв хоста, reject private/loopback/link-local/ULA/CGNAT + non-http(s)). SSO-outbound: timeout + redirect-ban применены; private-IP guard сознательно НЕ навешен (admin-config / OIDC-discovery endpoint'ы, self-hosted internal IdP — легитимен). Import github/gitlab + rg-mcp клиенты без timeout — см. cards |
| Верификация HMAC на входящих вебхуках | ✅ 🔒 | `rg-http/src/api/webhooks_external.rs` | opt-in `[webhooks].external_secret` / `FORGEKEEP_EXTERNAL_WEBHOOK_SECRET`; при заданном секрете `X-Hub-Signature-256` над сырым телом проверяется constant-time (`hmac::verify_slice`), формат `sha256=<hex>` симметричен исходящим; не задан → auth-only как раньше |

## 5. Robustness / эксплуатация

| Фича | Статус | Где | Заметки |
|------|--------|-----|---------|
| Graceful shutdown + drain in-flight запросов | ✅ | `rg-cli/src/serve.rs`, `rg-http/src/lib.rs` | SIGTERM/ctrl_c → `watch`-канал; `with_graceful_shutdown` (HTTP) + `axum_server::Handle::graceful_shutdown` (HTTPS) с конфигурируемым grace-окном (`[server].shutdown_grace_secs`, деф. 30). Живой тест: `kill -TERM` → exit 0 за ~0.2s |
| Shutdown-канал для фоновых воркеров | ✅ | audit/ci-retention/runner-watchdog/rate-limit/ci-log | Персистентные loop-воркеры + CI-log-очередь (`spawn_with_shutdown`) сливают буфер и выходят по сигналу (unit-тест `drains_buffered_writes_on_shutdown`) |
| Drain detached fire-and-forget доставок (webhook/WS) | ✅ | `rg-core/src/task_tracker.rs`, `webhook/service.rs:141`, `rg-http/src/ws.rs:319`, `lib.rs` (run drain) | Per-request detached-задачи (webhook-доставка + `webhook_delivery`-строка, WS-нотификация + persisted-строка) спавнятся через общий `tokio_util::task::TaskTracker` (`delivery_tracker()`), а не голый `tokio::spawn`; `run()` после server/CI-log-drain делает `close()`+`timeout(grace, wait())` → доставки не обрываются на полуслове по SIGTERM. Unit-тест `close_then_wait_drains_a_spawned_task`. Mirror-sync/merge-queue вызываются инлайн-await в хендлерах → уже покрыты HTTP-drain. Импорт (долгий) — recovery через watchdog, см. card_7bdb43ffa88c |
| Разделение ошибок: 503 (БД недоступна) vs 500 | ✅ 🔒 | `rg-http/src/error.rs` | `AppError::ServiceUnavailable`→503 `DB_UNAVAILABLE`; `From<DbErr>` классифицирует connection-level (`Conn`/`ConnectionAcquire`) → 503, statement-level (Exec/Query) → 500. Health-probe уже отдаёт 503 при падении БД. Осталось: часть хендлеров конвертит `DbErr` явно через `AppError::internal(...)` в обход `From` → см. card |
| Отдельный 504 для git-timeout | ✅ 🔒 | `rg-http/src/error.rs` | `AppError::Timeout`→504 `GIT_TIMEOUT`; `From<anyhow::Error>` downcast'ит `GitCliError::Timeout` (в т.ч. сквозь `.context()`). Детали (git-командная строка) не утекают клиенту |
| Range-валидация числовых конфигов | ❌ | `rg-cli/src/serve.rs` | serde-дефолты без проверки; `0`/абсурд принимается молча → см. card |
| Prometheus метрики (HTTP/db/git/ci/business/security) | ✅ | `rg-http/src/metrics.rs` | богаче, чем у аналогов; на основном порту |
| `/metrics` на отдельном приватном интерфейсе | 🟡 | `rg-http/src/metrics.rs` | сейчас на публичном app-порту (опционально вынести) |
| pack-size histogram | ❌ | — | мелкое дополнение метрик |

## 6. Хранилище / артефакты

| Фича | Статус | Где | Заметки |
|------|--------|-----|---------|
| BlobStorage trait + локальный atomic backend | ✅ | `rg-core/src/blob_storage.rs:88` | чистый trait-based дизайн |
| OCI package registry (content-addressed digests) | ✅ | `rg-core/src/package_registry/oci/` | digest для content-addressing, не provenance |
| SHA-256 digest для релизных ассетов | ✅ 🔒 | `rg-core/src/release/service.rs:154,214` | `upload_asset` пишет `sha256`, `download_asset` сверяет целостность (bail при mismatch), отдаётся `X-Checksum-Sha256`; legacy-ассеты (NULL) без guard'а |
| Подпись/attestation артефактов (SLSA/Sigstore-совместимо) | ❌ 🔒 | — | supply-chain provenance; подписант = server/CI Ed25519 → см. card_b6cfd5fdd0eb (Шаг 2) |

## 7. Agent-native / MCP

| Фича | Статус | Где | Заметки |
|------|--------|-----|---------|
| MCP-сервер (JSON-RPC, stdio) | ✅ | `rg-mcp/` | |
| Read tools (list_repos, read_file, read_dir, get_issue, get_pr, get_pr_diff) | ✅ | `rg-mcp/src/tools/mod.rs` | 6 read-обёрток |
| Write-tools (create/update/comment/labels issues, PR create/merge, review + inline-comment + apply-suggestion, request-reviewers) | ✅ | `rg-mcp/src/tools/mod.rs` | тонкие обёртки над REST, тело строится из whitelist ключей |
| CI-tools (list/get pipeline, retry/cancel, get job) | ✅ | `rg-mcp/src/tools/mod.rs` | `list_pipelines`/`get_pipeline`/`retry_pipeline`/`cancel_pipeline`/`get_ci_job` |
| `search` через MCP | ✅ | `rg-mcp/src/tools/mod.rs` | обёртка `/search` (q/type/page/per_page) |
| Обёртки `/ai/*` (summary / issues / prs / tree / search_code) | ✅ | `rg-mcp/src/tools/mod.rs` | оборачивает готовый AI-namespace `rg-http/src/api/ai.rs` |

---

## Сознательно НЕ реализуем (и почему)

Идеи из gitlawb, отвергнутые как чуждые архитектуре централизованного форджа:

- **UCAN capability-tokens + proof-chains** — решают делегирование между
  взаимно-недоверяющими пирами; у нас PAT-scopes + permission-строки в БД.
- **`did:key` / `did:web` identity** — DID нужны при отсутствии центрального
  реестра аккаунтов; у нас есть users/OAuth/LDAP/SSH.
- **libp2p / p2p-репликация / gossip** — чистая федерация.
- **iCaptcha** — требует внешний сервис, нарушает single-binary без внешних
  зависимостей. Приемлемая альтернатива — self-contained hashcash-PoW.
- **bounties / tasks как first-class сущности** — завязаны на wallet/UCAN,
  отдельный экономический продукт.
- **git-remote helper для кастомной URL-схемы** — обслуживает DID-модель.

---

## Как пользоваться

1. Перед добавлением фичи — `grep`/поиск по этой таблице.
2. Каждый ❌/🟡 с пометкой «→ см. card» имеет карточку в roadmap-фазе
   _«Hardening & agent-native: идеи из gitlawb»_.
3. Закрыл gap — обнови статус в таблице в том же PR.
