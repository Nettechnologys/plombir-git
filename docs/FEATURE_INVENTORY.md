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
| Wall-clock bound на стриминг-пути (pack-objects/index-pack) | 🟡 🔒 | `rg-git/src/cli_gateway.rs::spawn_async` | только `kill_on_drop`; git_http-хендлеры НЕ оборачивают I/O в `timeout()` → см. card |

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
| Защита регистрации от спама (invite/approval/captcha) | ❌ 🔒 | — | открытая саморегистрация; барьер только глобальный rate-limit (выкл. по умолчанию) → см. card |

## 3. Anti-abuse / rate limiting

| Фича | Статус | Где | Заметки |
|------|--------|-----|---------|
| Rate limiter (fixed-window token bucket, per-IP) | ✅ 🔒 | `rg-http/src/rate_limit.rs` | глобальный один слой; по умолчанию `max=0` (выкл.) |
| Trusted-proxy resolve (XFF/X-Real-IP по allowlist IP) | ✅ 🔒 | `rg-http/src/rate_limit.rs:140` | |
| `max_keys` cap на карту клиентов | ❌ 🔒 | `rg-http/src/rate_limit.rs:39` | HashMap не ограничен → memory-exhaustion под distinct-IP флудом → см. card |
| Per-route / per-endpoint лимиты (register/login/push) | ❌ 🔒 | — | сейчас один общий лимит на всё → см. card |
| CAPTCHA / proof-of-work | ❌ | — | сознательно НЕ портируем iCaptcha (внешний сервис); альтернатива — hashcash PoW |

## 4. Исходящие сетевые вызовы (webhooks / mirrors)

| Фича | Статус | Где | Заметки |
|------|--------|-----|---------|
| Outbound webhook доставка | ✅ | `rg-core/src/webhook/service.rs:169` | |
| HMAC-SHA256 подпись исходящих (`X-Hub-Signature-256`) | ✅ 🔒 | `rg-core/src/webhook/service.rs:188` | GitHub-совместимо |
| Timeout на исходящем HTTP-клиенте | ❌ 🔒 | `rg-core/src/webhook/service.rs:175` | `reqwest::Client::new()` без таймаута → см. card |
| Запрет редиректов / SSRF-защита (private/loopback IP) | ❌ 🔒 | `webhook/`, `mirror/` | дефолтно следует ≤10 редиректов → SSRF на `169.254.169.254`, внутренние сервисы → см. card |
| Верификация HMAC на входящих вебхуках | 🟡 🔒 | `rg-http/src/api/webhooks_external.rs:60` | эндпоинт за JWT/PAT; подпись — defense-in-depth → см. card |

## 5. Robustness / эксплуатация

| Фича | Статус | Где | Заметки |
|------|--------|-----|---------|
| Graceful shutdown + drain in-flight запросов | ✅ | `rg-cli/src/serve.rs`, `rg-http/src/lib.rs` | SIGTERM/ctrl_c → `watch`-канал; `with_graceful_shutdown` (HTTP) + `axum_server::Handle::graceful_shutdown` (HTTPS) с конфигурируемым grace-окном (`[server].shutdown_grace_secs`, деф. 30). Живой тест: `kill -TERM` → exit 0 за ~0.2s |
| Shutdown-канал для фоновых воркеров | 🟡 | audit/ci-retention/runner-watchdog/rate-limit/ci-log | Персистентные loop-воркеры + CI-log-очередь (`spawn_with_shutdown`) сливают буфер и выходят по сигналу (unit-тест `drains_buffered_writes_on_shutdown`). Per-request fire-and-forget доставки webhook/mirror остаются detached (best-effort) — не трекаются глобальным tracker'ом |
| Разделение ошибок: 503 (БД недоступна) vs 500 | ❌ | `rg-http/src/error.rs` | всё в `InternalError`/500, БД-ошибки тоже → см. card |
| Отдельный 504 для git-timeout | ❌ | `rg-http/src/error.rs` | нет варианта Timeout → см. card |
| Range-валидация числовых конфигов | ❌ | `rg-cli/src/serve.rs` | serde-дефолты без проверки; `0`/абсурд принимается молча → см. card |
| Prometheus метрики (HTTP/db/git/ci/business/security) | ✅ | `rg-http/src/metrics.rs` | богаче, чем у аналогов; на основном порту |
| `/metrics` на отдельном приватном интерфейсе | 🟡 | `rg-http/src/metrics.rs` | сейчас на публичном app-порту (опционально вынести) |
| pack-size histogram | ❌ | — | мелкое дополнение метрик |

## 6. Хранилище / артефакты

| Фича | Статус | Где | Заметки |
|------|--------|-----|---------|
| BlobStorage trait + локальный atomic backend | ✅ | `rg-core/src/blob_storage.rs:88` | чистый trait-based дизайн |
| OCI package registry (content-addressed digests) | ✅ | `rg-core/src/package_registry/oci/` | digest для content-addressing, не provenance |
| SHA-256 digest для релизных ассетов | ❌ 🔒 | `rg-core/src/release/service.rs:153` | `upload_asset` не пишет checksum → см. card |
| Подпись/attestation артефактов (SLSA/Sigstore-совместимо) | ❌ 🔒 | — | supply-chain provenance; подписант = server/CI Ed25519 → см. card |

## 7. Agent-native / MCP

| Фича | Статус | Где | Заметки |
|------|--------|-----|---------|
| MCP-сервер (JSON-RPC, stdio) | ✅ | `rg-mcp/` | |
| Read-only tools (list_repos, read_file, read_dir, get_issue, get_pr) | ✅ | `rg-mcp/src/tools/mod.rs:14` | всего 5 |
| Write-tools (create/update issues, PR, review, merge) | ❌ | — | backend REST готов в `rg-core`; нужны только обёртки → см. card |
| CI-tools (list/retry/cancel pipelines, job logs) | ❌ | — | endpoints готовы (`rg-http/src/api/ci.rs`) → см. card |
| `search_code` / поиск через MCP | ❌ | — | endpoint `/search` готов → см. card |
| Обёртки `/ai/*` (repo index / PR summary) | ❌ | `rg-http/src/api/ai.rs` | готовый AI-namespace, MCP его не оборачивает → см. card |

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
