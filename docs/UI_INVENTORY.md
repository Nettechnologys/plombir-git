# Plombir Git — UI Inventory (generated)

> **Сгенерировано.** Не править руками — перегенерировать:
> `node scripts/ui-inventory.mjs`
>
> Выводится из исходников: роутер (`crates/rg-http/src/routes.rs`), клиент
> (`web/src/lib/api`), страницы (`web/src/routes`) и общие компоненты
> (`web/src/lib/components`). Ручной близнец — `docs/FEATURE_INVENTORY.md`.
>
> **Что значит «покрыт».** `rust` / `smoke` отвечают на слабый вопрос —
> *называет ли исполняемый тестовый код метод и полный URL роута*.
> Комментарии и тот же URL под другим HTTP-методом coverage не создают.
> `web` строже, потому что SvelteKit пишет адрес страницы ровно так же, как
> адрес API за ней: засчитывается либо исполняемое обращение к client-члену,
> привязанному к роуту (`repos.explore`), либо URL рядом с транспортом
> (`request` / `downloadApiFile` / `fetch` / `WebSocket`). Навигация вроде
> `setTestPage('/search?q=…')` сама по себе HTTP-кредита не даёт. `browser` сильнее:
> manifest называет ровно один живой control/passive call, а runtime проводит
> его через owner + outsider и сверяет фактический статус с `Access`.
> Текстовое упоминание само по себе всё ещё НЕ означает полезного теста.

## Сводка

| | |
|---|---|
| Роутов в роутере (с объявленным `Access`) | 368 |
| Из них достижимы из браузера | 249 (68%) |
| Layout-модулей | 2 |
| Страниц | 63 |
| Интерактивных элементов | 829 |
| — из них дёргают API | 311 |
| — приходят из общих компонентов | 303 |
| Browser sweep: сценариев / записей инвентаря / роутов | 40 / 158 / 150 |
| **UI-роутов без единого web/smoke/browser-теста** | **27** |
| UI-роутов без corpus-hit и browser-сценария | 0 |

## По уровню доступа

| `Access` | роутов | достижимы из UI | нет фронт-теста | нет corpus/browser coverage |
|---|---:|---:|---:|---:|
| `RepoRead` | 105 | 67 | 4 | 0 |
| `RepoWrite` | 80 | 63 | 13 | 0 |
| `User` | 42 | 30 | 6 | 0 |
| `RepoAdmin` | 28 | 28 | 0 | 0 |
| `InstanceAdmin` | 23 | 19 | 0 | 0 |
| `Public` | 20 | 9 | 2 | 0 |
| `Foreign:oci.rs` | 13 | 0 | 0 | 0 |
| `RepoAuthRead` | 12 | 12 | 1 | 0 |
| `Foreign:RUNNER_AUTH_LAYER` | 11 | 0 | 0 | 0 |
| `OrgAdmin` | 8 | 8 | 0 | 0 |
| `Foreign:git_http.rs` | 6 | 0 | 0 | 0 |
| `OrgRead` | 5 | 4 | 0 | 0 |
| `Foreign:api/lfs_locks.rs` | 4 | 2 | 0 | 2 |
| `PublicFiltered` | 3 | 3 | 0 | 0 |
| `Foreign:api/lfs.rs` | 3 | 0 | 0 | 0 |
| `RepoOwner` | 2 | 2 | 0 | 0 |
| `Foreign:ws.rs` | 2 | 2 | 1 | 0 |
| `Foreign:api/ci_oidc.rs` | 1 | 0 | 0 | 0 |

## Страницы

| Страница | элементов | дёргают API | из компонентов |
|---|---:|---:|---:|
| `/[owner]/[repo]/pulls/[number]` | 57 | 32 | 22 |
| `/[owner]/[repo]/boards` | 40 | 14 | 11 |
| `/[owner]/[repo]/issues/[number]` | 27 | 13 | 20 |
| `/[owner]/[repo]/releases` | 27 | 12 | 11 |
| `/[owner]/[repo]/issues` | 26 | 9 | 12 |
| `/[owner]/[repo]/issues/board` | 26 | 10 | 11 |
| `/[owner]/[repo]/pulls` | 25 | 10 | 12 |
| `/orgs/[name]` | 24 | 10 | 0 |
| `/[owner]/[repo]/pipelines` | 23 | 11 | 11 |
| `/[owner]/[repo]/blob/[...path]` | 22 | 7 | 11 |
| `/[owner]/[repo]/wiki/[title]` | 22 | 9 | 11 |
| `/[owner]/[repo]/releases/new` | 21 | 5 | 11 |
| `/[owner]/[repo]` | 20 | 5 | 12 |
| `/[owner]/[repo]/milestones` | 20 | 8 | 11 |
| `/[owner]/[repo]/network` | 19 | 6 | 11 |
| `/[owner]/[repo]/packages` | 18 | 9 | 11 |
| `/[owner]/[repo]/releases/edit/[id]` | 18 | 5 | 11 |
| `/[owner]/[repo]/time_tracking` | 18 | 10 | 11 |
| `/[owner]/[repo]/packages/[format]/[...name]` | 17 | 6 | 11 |
| `/[owner]/[repo]/packages/upload` | 17 | 5 | 11 |
| `/[owner]/[repo]/wiki` | 17 | 5 | 11 |
| `/admin/settings` 🔒 | 17 | 9 | 0 |
| `/[owner]/[repo]/wiki/[title]/history` | 15 | 5 | 11 |
| `/settings/security` | 15 | 9 | 0 |
| `/[owner]/[repo]/commits/[sha]` | 14 | 6 | 11 |
| `/dashboard` | 14 | 3 | 0 |
| `/imports` | 14 | 3 | 0 |
| `/[owner]/[repo]/commits` | 13 | 4 | 11 |
| `/[owner]/[repo]/packages/[format]` | 13 | 4 | 11 |
| `/[owner]/[repo]/settings/branches` | 13 | 2 | 0 |
| `/admin/users` 🔒 | 13 | 5 | 0 |
| `/[owner]/[repo]/settings/webhooks` | 11 | 6 | 0 |
| `/settings/agents` | 11 | 5 | 0 |
| `/` | 10 | 1 | 0 |
| `/admin/runners` 🔒 | 10 | 4 | 0 |
| `/login` | 10 | 1 | 0 |
| `/[owner]/[repo]/edit/[...path]` | 8 | 0 | 8 |
| `/[owner]/[repo]/new` | 8 | 0 | 8 |
| `/[owner]/[repo]/settings/labels` | 8 | 2 | 0 |
| `/admin/audit` 🔒 | 8 | 6 | 0 |
| `/orgs` | 8 | 1 | 0 |
| `/search` | 8 | 0 | 0 |
| `/admin/orgs` 🔒 | 7 | 3 | 0 |
| `/[owner]/[repo]/settings/collaborators` | 6 | 3 | 0 |
| `/[owner]/[repo]/settings/environments` | 6 | 2 | 0 |
| `/[owner]/[repo]/settings/deploy-keys` | 5 | 2 | 0 |
| `/[owner]/[repo]/settings/mirror` | 5 | 3 | 0 |
| `/[owner]/[repo]/settings/tags` | 5 | 2 | 0 |
| `/admin` 🔒 | 5 | 0 | 0 |
| `/help` | 5 | 0 | 0 |
| `/reset-password` | 5 | 1 | 0 |
| `/[owner]` | 4 | 0 | 0 |
| `/settings/ssh-keys` | 4 | 2 | 0 |
| `/settings/tokens` | 4 | 2 | 0 |
| `/[owner]/[repo]/settings/ci-secrets` | 3 | 2 | 0 |
| `/[owner]/[repo]/settings/retention` | 3 | 2 | 0 |
| `/explore` | 3 | 2 | 0 |
| `/forgot-password` | 3 | 1 | 0 |
| `/notifications` | 3 | 3 | 0 |
| `/register` | 3 | 0 | 0 |
| `/[owner]/[repo]/settings` | 2 | 2 | 0 |
| `/[owner]/[repo]/settings/lfs-locks` | 2 | 2 | 0 |
| `/[owner]/[repo]/settings/runners` | 1 | 0 | 0 |

## План тестов: элемент → роут → уровень доступа

Каждая строка — один сценарий e2e. `Access` говорит, какая персона обязана
пройти и какая обязана получить отказ.

### Глобальные layout-загрузки

| Scope | Источник | Вызов | `Access` | тест |
|---|---|---|---|---|
| `/` | `instance.get` | `GET /api/v1/instance` | `Public` | rust+web |
| `/` | `web/src/routes/+layout.svelte#checkBackendReadiness` | `GET /health` | `Public` | rust+smoke |

### `/`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Retry | :188 | `GET /api/v1/repos/explore` | `PublicFiltered` | rust+web+smoke |

### `/[owner]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}` | `PublicFiltered` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/orgs/{name}` | `OrgRead` | rust+web+smoke |
| _(загрузка страницы)_ | — | `GET /api/v1/orgs` | `User` | rust+web+smoke |

### `/[owner]/[repo]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.retry | :418 | `GET /api/v1/repos/{owner}/{name}/blob/{*path}` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke+browser |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web+browser |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web+browser |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke+browser |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/tree` | `RepoRead` | rust+web+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | web+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/log` | `RepoRead` | rust+web+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web+browser |

### `/[owner]/[repo]/blob/[...path]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:repo.blob.download | :493 | `GET /api/v1/repos/{owner}/{name}/raw/{*path}` | `RepoRead` | web |
| i18n:repo.blob.deleting | :546 | `DELETE /api/v1/repos/{owner}/{name}/contents/{*path}` | `RepoWrite` | rust+web+browser |
| i18n:repo.blob.download | :580 | `GET /api/v1/repos/{owner}/{name}/raw/{*path}` | `RepoRead` | web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/blob/{*path}` | `RepoRead` | rust+web+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/raw/{*path}` | `RepoRead` | web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/boards`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.create | :478 | `POST /api/v1/repos/{owner}/{name}/boards` | `RepoWrite` | rust+web+browser |
| i18n:common.create | :478 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust+web+browser |
| i18n:common.save | :514 | `PATCH /api/v1/repos/{owner}/{name}/boards/{id}/cards/{card_id}` | `RepoWrite` | web+browser |
| i18n:common.save | :514 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust+web |
| &times; | :540 | `DELETE /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoWrite` | rust+web+browser |
| &times; | :540 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust+web |
| i18n:common.save | :579 | `PATCH /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoWrite` | web+browser |
| &times; | :613 | `DELETE /api/v1/repos/{owner}/{name}/boards/{id}/columns/{col_id}` | `RepoWrite` | rust+web+browser |
| ↑ | :622 | `POST /api/v1/repos/{owner}/{name}/boards/{id}/cards/reorder` | `RepoWrite` | rust+web |
| ↑ | :622 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust+web |
| ↓ | :629 | `POST /api/v1/repos/{owner}/{name}/boards/{id}/cards/reorder` | `RepoWrite` | rust+web+browser |
| ↓ | :629 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust+web |
| &times; | :637 | `DELETE /api/v1/repos/{owner}/{name}/boards/{id}/cards/{card_id}` | `RepoWrite` | rust+web+browser |
| i18n:board.moveTo | :646 | `POST /api/v1/repos/{owner}/{name}/boards/{id}/cards/{card_id}/move` | `RepoWrite` | rust+web+browser |
| i18n:board.moveTo | :646 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust+web |
| i18n:common.add | :664 | `POST /api/v1/repos/{owner}/{name}/boards/{id}/columns/{col_id}/cards` | `RepoWrite` | rust+web+browser |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/boards` | `RepoRead` | rust+web+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/issues` | `RepoRead` | rust+web+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/commits`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/log` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/commits/[sha]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Retry | :172 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/status` | `RepoRead` | rust+web+browser |
| Retry | :172 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/statuses` | `RepoRead` | rust+web+browser |
| Retry | :172 | `GET /api/v1/repos/{owner}/{name}/log` | `RepoRead` | rust+web |
| Retry | :172 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/signature` | `RepoRead` | rust+web+browser |
| Retry | :224 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/status` | `RepoRead` | rust+web |
| Retry | :224 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/statuses` | `RepoRead` | rust+web |
| Retry | :224 | `GET /api/v1/repos/{owner}/{name}/log` | `RepoRead` | rust+web |
| Retry | :224 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/signature` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/edit/[...path]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/blob/{*path}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `POST /api/v1/repos/{owner}/{name}/contents/{*path}` | `RepoWrite` | rust+web |

### `/[owner]/[repo]/issues`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:issues.tabs.open | :209 | `GET /api/v1/repos/{owner}/{name}/issues` | `RepoRead` | rust+web |
| i18n:issues.tabs.closed | :216 | `GET /api/v1/repos/{owner}/{name}/issues` | `RepoRead` | rust+web |
| i18n:issues.tabs.all | :223 | `GET /api/v1/repos/{owner}/{name}/issues` | `RepoRead` | rust+web |
| i18n:issues.new | :231 | `GET /api/v1/repos/{owner}/{name}/issue_templates` | `RepoRead` | rust+web+browser |
| i18n:issues.new | :231 | `GET /api/v1/repos/{owner}/{name}/issue_config` | `RepoRead` | rust+web+browser |
| toggleLabel(label.name, event.currentTarget.checked)} /> }> | :279 | `POST /api/v1/repos/{owner}/{name}/issues` | `RepoAuthRead` | rust+web+browser |
| toggleLabel(label.name, event.currentTarget.checked)} /> }> | :279 | `GET /api/v1/repos/{owner}/{name}/issues` | `RepoRead` | rust+web+browser |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/labels` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/issues/[number]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.loading | :267 | `PATCH /api/v1/repos/{owner}/{name}/issues/{number}` | `RepoWrite` | rust+web |
| i18n:issues.comment_placeholder | :296 | `POST /api/v1/repos/{owner}/{name}/issues/{number}/comments` | `RepoAuthRead` | rust+browser |
| i18n:issues.comment_placeholder | :296 | `GET /api/v1/repos/{owner}/{name}/issues/{number}` | `RepoRead` | rust+web+browser |
| i18n:issues.comment_placeholder | :296 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/comments` | `RepoRead` | rust+web+browser |
| i18n:issues.comment_placeholder | :296 | `GET /api/v1/repos/{owner}/{name}/milestones` | `RepoRead` | rust+web+browser |
| i18n:issues.comment_placeholder | :296 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust+web+browser |
| i18n:issues.comment_placeholder | :296 | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
| i18n:issues.close_issue | :300 | `PATCH /api/v1/repos/{owner}/{name}/issues/{number}` | `RepoWrite` | rust+web+browser |
| i18n:issues.close_issue | :300 | `GET /api/v1/repos/{owner}/{name}/issues/{number}` | `RepoRead` | rust+web |
| i18n:issues.close_issue | :300 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/comments` | `RepoRead` | rust+web |
| i18n:issues.close_issue | :300 | `GET /api/v1/repos/{owner}/{name}/milestones` | `RepoRead` | rust+web |
| i18n:issues.close_issue | :300 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust+web |
| i18n:issues.close_issue | :300 | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| upload | `AttachmentPanel` | `POST /api/v1/repos/{owner}/{name}/issues/{number}/assets` | `RepoWrite` | rust |
| → item.browser_download_url | `AttachmentPanel` | `GET /api/v1/repos/{owner}/{name}/issues/{number}/assets/{attachment_id}` | `RepoRead` | rust |
| i18n:attachments.delete | `AttachmentPanel` | `DELETE /api/v1/repos/{owner}/{name}/issues/{number}/assets/{attachment_id}` | `RepoWrite` | rust |
| upload | `AttachmentPanel` | `POST /api/v1/repos/{owner}/{name}/issues/comments/{comment_id}/assets` | `RepoWrite` | rust |
| → item.browser_download_url | `AttachmentPanel` | `GET /api/v1/repos/{owner}/{name}/issues/comments/{comment_id}/assets/{attachment_id}` | `RepoRead` | rust |
| i18n:attachments.delete | `AttachmentPanel` | `DELETE /api/v1/repos/{owner}/{name}/issues/comments/{comment_id}/assets/{attachment_id}` | `RepoWrite` | rust |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/issues/{number}/assets` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/issues/comments/{comment_id}/assets` | `RepoRead` | rust+web |

### `/[owner]/[repo]/issues/board`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| () => selectBoard(b.id) | :377 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust+web |
| handleCreateBoard | :409 | `POST /api/v1/repos/{owner}/{name}/boards` | `RepoWrite` | rust+web |
| handleCreateBoard | :409 | `GET /api/v1/repos/{owner}/{name}/boards` | `RepoRead` | rust+web |
| handleCreateBoard | :409 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust+web |
| Add | :425 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust+web |
| ✕ | :449 | `DELETE /api/v1/repos/{owner}/{name}/boards/{id}/columns/{col_id}` | `RepoWrite` | rust+web |
| ✕ | :449 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust+web |
| ✕ | :485 | `DELETE /api/v1/repos/{owner}/{name}/boards/{id}/cards/{card_id}` | `RepoWrite` | rust+web |
| ✕ | :485 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust+web |
| Add | :507 | `POST /api/v1/repos/{owner}/{name}/boards/{id}/columns/{col_id}/cards` | `RepoWrite` | rust+web |
| Add | :507 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `POST /api/v1/repos/{owner}/{name}/boards/{id}/cards/reorder` | `RepoWrite` | rust+web |
| _(загрузка страницы)_ | — | `POST /api/v1/repos/{owner}/{name}/boards/{id}/cards/{card_id}/move` | `RepoWrite` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/milestones`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:milestones.name | :229 | `POST /api/v1/repos/{owner}/{name}/milestones` | `RepoWrite` | rust+web+browser |
| i18n:milestones.name | :229 | `PATCH /api/v1/repos/{owner}/{name}/milestones/{id}` | `RepoWrite` | rust+web+browser |
| i18n:milestones.name | :229 | `GET /api/v1/repos/{owner}/{name}/milestones` | `RepoRead` | rust+web+browser |
| i18n:common.edit | :289 | `GET /api/v1/repos/{owner}/{name}/milestones/{id}` | `RepoRead` | rust+web+browser |
| i18n:milestones.close | :290 | `PATCH /api/v1/repos/{owner}/{name}/milestones/{id}` | `RepoWrite` | rust+web |
| i18n:milestones.close | :290 | `GET /api/v1/repos/{owner}/{name}/milestones` | `RepoRead` | rust+web |
| i18n:common.delete | :293 | `DELETE /api/v1/repos/{owner}/{name}/milestones/{id}` | `RepoWrite` | rust+browser |
| i18n:common.delete | :293 | `GET /api/v1/repos/{owner}/{name}/milestones` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/network`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.retry | :100 | `GET /api/v1/repos/{owner}/{name}/stargazers` | `RepoRead` | rust+web+browser |
| i18n:common.retry | :154 | `GET /api/v1/repos/{owner}/{name}/forks` | `RepoRead` | rust+web+browser |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/new`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| _(загрузка страницы)_ | — | `POST /api/v1/repos/{owner}/{name}/contents/{*path}` | `RepoWrite` | rust+web+browser |

### `/[owner]/[repo]/packages`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.retry | :162 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust+web |
| i18n:common.retry | :162 | `GET /api/v1/repos/{owner}/{name}/packages` | `RepoRead` | rust+web |
| i18n:common.all | :171 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust+web |
| i18n:common.all | :171 | `GET /api/v1/repos/{owner}/{name}/packages` | `RepoRead` | rust+web |
| i18n:common.search | :186 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust+web |
| i18n:common.search | :186 | `GET /api/v1/repos/{owner}/{name}/packages` | `RepoRead` | rust+web |
| i18n:common.previous | :225 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust+web |
| i18n:common.previous | :225 | `GET /api/v1/repos/{owner}/{name}/packages` | `RepoRead` | rust+web |
| i18n:common.next | :233 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust+web |
| i18n:common.next | :233 | `GET /api/v1/repos/{owner}/{name}/packages` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/packages/[format]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust+web+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/packages/[format]/[...name]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:packages.unyank | :264 | `PATCH /api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/yank` | `RepoWrite` | rust+web |
| i18n:packages.unyank | :264 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/versions` | `RepoRead` | rust+web+browser |
| i18n:common.delete | :294 | `DELETE /api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}` | `RepoWrite` | rust+web |
| i18n:common.delete | :294 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}` | `RepoRead` | rust+web+browser |
| i18n:common.delete | :294 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/versions` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/{*file}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/packages/upload`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Name Homepage Repository URL Semver | :141 | `POST /api/v1/repos/{owner}/{name}/packages/{pkg_type}/publish` | `RepoWrite` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/pipelines`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| updateTriggerRef(event.currentTarget.value)} onchange= place | :728 | `POST /api/v1/repos/{owner}/{name}/pipelines` | `RepoWrite` | rust+web+browser |
| updateTriggerRef(event.currentTarget.value)} onchange= place | :728 | `GET /api/v1/repos/{owner}/{name}/pipelines` | `RepoRead` | rust+web+browser |
| updateTriggerRef(event.currentTarget.value)} onchange= place | :728 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}` | `RepoRead` | rust+web+browser |
| updateTriggerRef(event.currentTarget.value)} onchange= place | :728 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}/artifacts` | `RepoRead` | rust+web+browser |
| i18n:pipeline.retry | :856 | `POST /api/v1/repos/{owner}/{name}/pipelines/{id}/retry` | `RepoWrite` | rust+web |
| i18n:pipeline.retry | :856 | `GET /api/v1/repos/{owner}/{name}/pipelines` | `RepoRead` | rust+web |
| i18n:pipeline.retry | :856 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}` | `RepoRead` | rust+web |
| i18n:pipeline.retry | :856 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}/artifacts` | `RepoRead` | rust+web |
| i18n:pipeline.cancel | :859 | `POST /api/v1/repos/{owner}/{name}/pipelines/{id}/cancel` | `RepoWrite` | rust |
| i18n:pipeline.play_manual | :919 | `POST /api/v1/repos/{owner}/{name}/pipelines/{id}/jobs/{job_id}/play` | `RepoWrite` | rust |
| i18n:pipeline.play_manual | :919 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}` | `RepoRead` | rust+web |
| i18n:pipeline.play_manual | :919 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}/artifacts` | `RepoRead` | rust+web |
| i18n:pipeline.approval_recorded | :922 | `POST /api/v1/repos/{owner}/{name}/pipelines/{pipeline_id}/jobs/{job_id}/approve` | `RepoAuthRead` | rust |
| i18n:pipeline.approval_recorded | :922 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}` | `RepoRead` | rust+web |
| i18n:pipeline.approval_recorded | :922 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}/artifacts` | `RepoRead` | rust+web |
| i18n:pipeline.artifact_downloading | :958 | `GET /api/v1/artifacts/{id}/download` | `RepoRead` | rust+web |
| i18n:pipeline.artifact_deleting | :963 | `DELETE /api/v1/artifacts/{id}` | `RepoWrite` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/pipelines/workflow-dispatch` | `RepoRead` | rust+web+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}/jobs/{job_id}` | `RepoRead` | rust+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/ws/job/{job_id}` | `Foreign:ws.rs` | rust |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/pulls`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:pulls.tabs.open | :199 | `GET /api/v1/repos/{owner}/{name}/pulls` | `RepoRead` | rust+web |
| i18n:pulls.tabs.closed | :206 | `GET /api/v1/repos/{owner}/{name}/pulls` | `RepoRead` | rust+web |
| i18n:pulls.tabs.merged | :213 | `GET /api/v1/repos/{owner}/{name}/pulls` | `RepoRead` | rust+web |
| i18n:pulls.new | :221 | `GET /api/v1/repos/{owner}/{name}/pull_request_template` | `RepoRead` | rust+web+browser |
| i18n:common.retry | :232 | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | web |
| → showCreate = false}> | :242 | `POST /api/v1/repos/{owner}/{name}/pulls` | `RepoAuthRead` | rust+web+browser |
| → showCreate = false}> | :242 | `GET /api/v1/repos/{owner}/{name}/pulls` | `RepoRead` | rust+web+browser |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/pulls/[number]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.retry | :522 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:common.retry | :522 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:common.retry | :522 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:common.retry | :522 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:common.retry | :522 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:common.retry | :522 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:common.retry | :522 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.mark_ready | :543 | `PATCH /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoWrite` | rust+web+browser |
| × | :586 | `DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers/{username}` | `RepoWrite` | rust+browser |
| i18n:pulls.reviewers.request | :597 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoWrite` | rust+browser |
| i18n:pulls.reviewers.request | :597 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :610 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/ci-approval` | `RepoWrite` | web |
| i18n:pulls.fork_ci.approving | :610 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :610 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :610 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :610 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :610 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :610 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :610 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :627 | `DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/merge-queue` | `RepoWrite` | browser |
| i18n:pulls.merge.leave_queue | :627 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :627 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :627 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :627 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :627 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :627 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :627 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.merge.disable_auto | :638 | `DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/auto-merge` | `RepoWrite` | browser |
| i18n:pulls.merge.merging | :649 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/merge` | `RepoWrite` | rust+web |
| i18n:pulls.merge.merging | :649 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.merge.merging | :649 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.merge.merging | :649 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.merge.merging | :649 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.merge.merging | :649 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.merge.merging | :649 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.merge.merging | :649 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :652 | `PUT /api/v1/repos/{owner}/{name}/pulls/{number}/auto-merge` | `RepoWrite` | rust+browser |
| i18n:pulls.merge.enabling_auto | :652 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :652 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :652 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :652 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :652 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :652 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :652 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :655 | `PUT /api/v1/repos/{owner}/{name}/pulls/{number}/merge-queue` | `RepoWrite` | rust+browser |
| i18n:pulls.merge.joining_queue | :655 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :655 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :655 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :655 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :655 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :655 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :655 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :703 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/reviews/{id}/dismiss` | `RepoWrite` | rust+web |
| i18n:pulls.review.dismissing | :703 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :703 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :703 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :703 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :703 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :703 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :703 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :727 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/suggestions/apply` | `RepoWrite` | rust |
| i18n:pulls.suggestion.applying_selected | :727 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :727 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :727 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :727 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :727 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :727 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :727 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :769 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}/suggestion/apply` | `RepoWrite` | rust |
| i18n:pulls.suggestion.apply | :769 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :769 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :769 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :769 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :769 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :769 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :769 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.threads.reopen | :780 | `PATCH /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}/resolution` | `RepoWrite` | **—** |
| i18n:pulls.threads.reopen | :780 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :837 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}/suggestion/apply` | `RepoWrite` | rust |
| i18n:pulls.suggestion.apply | :837 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :837 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :837 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :837 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :837 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :837 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :837 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.threads.reopen | :846 | `PATCH /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}/resolution` | `RepoWrite` | browser |
| i18n:pulls.threads.reopen | :846 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.diff.submit_comment | :865 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoAuthRead` | rust+browser |
| i18n:pulls.diff.submit_comment | :865 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:common.retry | :877 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:common.retry | :877 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:common.retry | :877 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:common.retry | :877 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:common.retry | :877 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:common.retry | :877 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:common.retry | :877 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.review.submit | :904 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoAuthRead` | rust+browser |
| i18n:pulls.review.submit | :904 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web+browser |
| i18n:pulls.review.submit | :904 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web+browser |
| i18n:pulls.review.submit | :904 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web+browser |
| i18n:pulls.review.submit | :904 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web+browser |
| i18n:pulls.review.submit | :904 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web+browser |
| i18n:pulls.review.submit | :904 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web+browser |
| i18n:pulls.review.submit | :904 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web+browser |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| upload | `AttachmentPanel` | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/assets` | `RepoWrite` | rust |
| → item.browser_download_url | `AttachmentPanel` | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/assets/{attachment_id}` | `RepoRead` | rust |
| i18n:attachments.delete | `AttachmentPanel` | `DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/assets/{attachment_id}` | `RepoWrite` | rust |
| upload | `AttachmentPanel` | `POST /api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets` | `RepoWrite` | rust |
| → item.browser_download_url | `AttachmentPanel` | `GET /api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets/{attachment_id}` | `RepoRead` | rust |
| i18n:attachments.delete | `AttachmentPanel` | `DELETE /api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets/{attachment_id}` | `RepoWrite` | rust |
| upload | `AttachmentPanel` | `POST /api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets` | `RepoWrite` | rust |
| → item.browser_download_url | `AttachmentPanel` | `GET /api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets/{attachment_id}` | `RepoRead` | rust |
| i18n:attachments.delete | `AttachmentPanel` | `DELETE /api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets/{attachment_id}` | `RepoWrite` | rust |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/assets` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets` | `RepoRead` | rust+web |

### `/[owner]/[repo]/releases`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| (event) => handleAssetUpload(release.id, | :520 | `POST /api/v1/repos/{owner}/{name}/releases/{release_id}/assets` | `RepoWrite` | rust+web |
| · )} | :539 | `GET /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/download` | `RepoRead` | rust+web |
| i18n:releases.attestation.verifying | :581 | `POST /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation/verify` | `RepoRead` | rust+web |
| i18n:releases.attestation.signing | :592 | `POST /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation` | `RepoWrite` | rust+web |
| i18n:common.delete | :618 | `DELETE /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}` | `RepoWrite` | rust+web+browser |
| i18n:common.delete | :653 | `DELETE /api/v1/repos/{owner}/{name}/releases/{id}` | `RepoWrite` | rust+web |
| i18n:common.delete | :653 | `GET /api/v1/instance` | `Public` | rust+web+browser |
| i18n:common.delete | :653 | `GET /api/v1/repos/{owner}/{name}/releases` | `RepoRead` | rust+web |
| i18n:common.delete | :653 | `GET /api/v1/repos/{owner}/{name}/releases/{release_id}/assets` | `RepoRead` | rust+web |
| i18n:common.delete | :653 | `GET /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation` | `RepoRead` | rust+web |
| Previous | :668 | `GET /api/v1/instance` | `Public` | rust+web |
| Previous | :668 | `GET /api/v1/repos/{owner}/{name}/releases` | `RepoRead` | rust+web |
| Previous | :668 | `GET /api/v1/repos/{owner}/{name}/releases/{release_id}/assets` | `RepoRead` | rust+web |
| Previous | :668 | `GET /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation` | `RepoRead` | rust+web |
| Next | :676 | `GET /api/v1/instance` | `Public` | rust+web |
| Next | :676 | `GET /api/v1/repos/{owner}/{name}/releases` | `RepoRead` | rust+web |
| Next | :676 | `GET /api/v1/repos/{owner}/{name}/releases/{release_id}/assets` | `RepoRead` | rust+web |
| Next | :676 | `GET /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/releases/edit/[id]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Tag * | :194 | `PATCH /api/v1/repos/{owner}/{name}/releases/{id}` | `RepoWrite` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/releases/{id}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/releases/new`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| * Existing tags: tagName = tag} > + more * selectedTargetTyp | :145 | `POST /api/v1/repos/{owner}/{name}/releases` | `RepoWrite` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/tags` | `RepoRead` | web+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/settings`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.transfer.confirming | :221 | `POST /api/v1/repos/{owner}/{name}/transfer` | `RepoOwner` | rust+web+browser |
| i18n:settings.delete.confirming | :255 | `DELETE /api/v1/repos/{owner}/{name}` | `RepoOwner` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/settings/branches`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| )} removeRequiredStatusCheck(index)} disabled= aria-label= ) | :243 | `PATCH /api/v1/repos/{owner}/{name}/branches/protection/{id}` | `RepoAdmin` | web+browser |
| )} removeRequiredStatusCheck(index)} disabled= aria-label= ) | :243 | `POST /api/v1/repos/{owner}/{name}/branches/protection` | `RepoAdmin` | rust+web+browser |
| )} removeRequiredStatusCheck(index)} disabled= aria-label= ) | :243 | `GET /api/v1/repos/{owner}/{name}/branches/protection` | `RepoRead` | rust+web+browser |
| i18n:common.delete | :392 | `DELETE /api/v1/repos/{owner}/{name}/branches/protection/{id}` | `RepoAdmin` | web+browser |
| i18n:common.delete | :392 | `GET /api/v1/repos/{owner}/{name}/branches/protection` | `RepoRead` | rust+web |

### `/[owner]/[repo]/settings/ci-secrets`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Name Value Save secret | :91 | `PUT /api/v1/repos/{owner}/{name}/actions/secrets/{secret_name}` | `RepoAdmin` | rust+web+browser |
| Name Value Save secret | :91 | `GET /api/v1/repos/{owner}/{name}/actions/secrets` | `RepoAdmin` | rust+web+browser |
| Delete | :91 | `DELETE /api/v1/repos/{owner}/{name}/actions/secrets/{secret_name}` | `RepoAdmin` | rust+web+browser |
| Delete | :91 | `GET /api/v1/repos/{owner}/{name}/actions/secrets` | `RepoAdmin` | rust+web |

### `/[owner]/[repo]/settings/collaborators`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.collaborators.user_identifier | :209 | `POST /api/v1/repos/{owner}/{name}/collaborators` | `RepoAdmin` | rust+web+smoke+browser |
| i18n:settings.collaborators.user_identifier | :209 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust+web+browser |
| i18n:common.save | :272 | `PATCH /api/v1/repos/{owner}/{name}/collaborators/{id}` | `RepoAdmin` | rust+web+browser |
| i18n:common.save | :272 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust+web |
| i18n:common.delete | :280 | `DELETE /api/v1/repos/{owner}/{name}/collaborators/{id}` | `RepoAdmin` | rust+web+browser |
| i18n:common.delete | :280 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust+web |

### `/[owner]/[repo]/settings/deploy-keys`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.deploy_keys.name | :167 | `POST /api/v1/repos/{owner}/{name}/keys` | `RepoAdmin` | rust+web+browser |
| i18n:settings.deploy_keys.name | :167 | `GET /api/v1/repos/{owner}/{name}/keys` | `RepoAdmin` | rust+web+browser |
| i18n:common.delete | :192 | `DELETE /api/v1/repos/{owner}/{name}/keys/{id}` | `RepoAdmin` | rust+browser |
| i18n:common.delete | :192 | `GET /api/v1/repos/{owner}/{name}/keys` | `RepoAdmin` | rust+web |

### `/[owner]/[repo]/settings/environments`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Name Require approval Required approvals Allowed approvers ( | :100 | `POST /api/v1/repos/{owner}/{name}/actions/environments` | `RepoAdmin` | rust+web+browser |
| Name Require approval Required approvals Allowed approvers ( | :100 | `PUT /api/v1/repos/{owner}/{name}/actions/environments/{id}` | `RepoAdmin` | web+browser |
| Name Require approval Required approvals Allowed approvers ( | :100 | `GET /api/v1/repos/{owner}/{name}/actions/environments` | `RepoRead` | rust+web+browser |
| Delete | :107 | `DELETE /api/v1/repos/{owner}/{name}/actions/environments/{id}` | `RepoAdmin` | rust+web+browser |
| Delete | :107 | `GET /api/v1/repos/{owner}/{name}/actions/environments` | `RepoRead` | rust+web |

### `/[owner]/[repo]/settings/labels`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.save_label | :339 | `PATCH /api/v1/repos/{owner}/{name}/labels/{id}` | `RepoWrite` | rust+web+browser |
| i18n:settings.save_label | :339 | `POST /api/v1/repos/{owner}/{name}/labels` | `RepoWrite` | rust+web+browser |
| i18n:settings.save_label | :339 | `GET /api/v1/repos/{owner}/{name}/labels` | `RepoRead` | rust+web+browser |
| handleDelete | :365 | `DELETE /api/v1/repos/{owner}/{name}/labels/{id}` | `RepoWrite` | rust+web+browser |
| handleDelete | :365 | `GET /api/v1/repos/{owner}/{name}/labels` | `RepoRead` | rust+web |

### `/[owner]/[repo]/settings/lfs-locks`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.lfs_locks.force_unlock | :122 | `POST /api/v1/repos/{owner}/{name}/lfs/locks/{id}/unlock` | `Foreign:api/lfs_locks.rs` | web |
| i18n:settings.lfs_locks.more | :135 | `GET /api/v1/repos/{owner}/{name}/lfs/locks` | `Foreign:api/lfs_locks.rs` | rust+web |

### `/[owner]/[repo]/settings/mirror`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.mirror.url | :200 | `PATCH /api/v1/repos/{owner}/{name}/mirror` | `RepoWrite` | rust+web |
| i18n:settings.mirror.url | :200 | `POST /api/v1/repos/{owner}/{name}/mirror` | `RepoWrite` | rust+web |
| i18n:common.loading | :241 | `POST /api/v1/repos/{owner}/{name}/mirror/sync` | `RepoWrite` | rust |
| i18n:common.loading | :241 | `GET /api/v1/repos/{owner}/{name}/mirror` | `RepoWrite` | rust+web |
| i18n:common.loading | :244 | `DELETE /api/v1/repos/{owner}/{name}/mirror` | `RepoWrite` | rust+web |

### `/[owner]/[repo]/settings/retention`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Artifact retention (days) Cache retention after last access  | :120 | `PUT /api/v1/repos/{owner}/{name}/actions/retention` | `RepoAdmin` | web+browser |
| Clean expired storage now | :127 | `DELETE /api/v1/repos/{owner}/{name}/actions/retention/expired` | `RepoAdmin` | rust+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/actions/retention` | `RepoAdmin` | rust+web+browser |

### `/[owner]/[repo]/settings/tags`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Pattern (use * as the wildcard; ? , character classes, and + | :100 | `POST /api/v1/repos/{owner}/{name}/tags/protection` | `RepoAdmin` | rust+web+browser |
| Pattern (use * as the wildcard; ? , character classes, and + | :100 | `PATCH /api/v1/repos/{owner}/{name}/tags/protection/{id}` | `RepoAdmin` | rust+web+browser |
| Pattern (use * as the wildcard; ? , character classes, and + | :100 | `GET /api/v1/repos/{owner}/{name}/tags/protection` | `RepoRead` | rust+web+browser |
| Delete | :107 | `DELETE /api/v1/repos/{owner}/{name}/tags/protection/{id}` | `RepoAdmin` | rust+web+browser |
| Delete | :107 | `GET /api/v1/repos/{owner}/{name}/tags/protection` | `RepoRead` | rust+web |

### `/[owner]/[repo]/settings/webhooks`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| application/json application/x-www-form-urlencoded toggleEve | :376 | `POST /api/v1/repos/{owner}/{name}/hooks` | `RepoAdmin` | rust+web+browser |
| application/json application/x-www-form-urlencoded toggleEve | :376 | `GET /api/v1/repos/{owner}/{name}/hooks` | `RepoAdmin` | rust+web+browser |
| (e) => setActive(hook, e.currentTarget.c | :444 | `PATCH /api/v1/repos/{owner}/{name}/hooks/{id}` | `RepoAdmin` | rust+web+browser |
| (e) => setActive(hook, e.currentTarget.c | :444 | `GET /api/v1/repos/{owner}/{name}/hooks` | `RepoAdmin` | rust+web |
| i18n:settings.webhooks.hide_deliveries | :453 | `GET /api/v1/repos/{owner}/{name}/hooks/{id}` | `RepoAdmin` | rust+web+browser |
| i18n:settings.webhooks.hide_deliveries | :453 | `GET /api/v1/repos/{owner}/{name}/hooks/{id}/deliveries` | `RepoAdmin` | rust+web+browser |
| i18n:common.loading | :464 | `DELETE /api/v1/repos/{owner}/{name}/hooks/{id}` | `RepoAdmin` | rust+web+browser |
| i18n:common.loading | :464 | `GET /api/v1/repos/{owner}/{name}/hooks` | `RepoAdmin` | rust+web |
| i18n:common.loading | :482 | `GET /api/v1/repos/{owner}/{name}/hooks/{id}/deliveries` | `RepoAdmin` | rust+web |
| i18n:settings.webhooks.redelivering | :517 | `POST /api/v1/repos/{owner}/{name}/hooks/{id}/deliveries/{delivery_id}/redeliver` | `RepoAdmin` | rust+web+browser |
| i18n:settings.webhooks.redelivering | :517 | `GET /api/v1/repos/{owner}/{name}/hooks/{id}/deliveries` | `RepoAdmin` | rust+web |

### `/[owner]/[repo]/time_tracking`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| # | :296 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time` | `RepoRead` | rust+web+browser |
| # | :296 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time/total` | `RepoRead` | rust+web+browser |
| i18n:common.retry | :326 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time/total` | `RepoRead` | rust+web |
| handleAdd | :355 | `POST /api/v1/repos/{owner}/{name}/issues/{number}/time` | `RepoWrite` | rust+web+browser |
| handleAdd | :355 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time` | `RepoRead` | rust+web |
| handleAdd | :355 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time/total` | `RepoRead` | rust+web |
| Delete | :384 | `DELETE /api/v1/repos/{owner}/{name}/issues/{number}/time/{id}` | `RepoWrite` | rust+web+browser |
| Delete | :384 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time` | `RepoRead` | rust+web |
| Delete | :384 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time/total` | `RepoRead` | rust+web |
| Previous | :393 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time` | `RepoRead` | rust+web |
| Next | :396 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/issues` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/wiki`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| showCreate = false}> | :107 | `POST /api/v1/repos/{owner}/{name}/wiki` | `RepoWrite` | rust+web+browser |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/wiki` | `RepoRead` | rust+web+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/wiki/[title]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| History | :304 | `GET /api/v1/repos/{owner}/{name}/wiki/{title}/history` | `RepoRead` | rust+web+browser |
| i18n:wiki.delete | :306 | `DELETE /api/v1/repos/{owner}/{name}/wiki/{title}` | `RepoWrite` | rust+browser |
| v | :321 | `GET /api/v1/repos/{owner}/{name}/wiki/{title}/revisions/{rev_id}` | `RepoRead` | rust+web+browser |
| Restore this version | :330 | `PATCH /api/v1/repos/{owner}/{name}/wiki/{title}` | `RepoWrite` | rust |
| Restore this version | :330 | `GET /api/v1/repos/{owner}/{name}/wiki/{title}` | `RepoRead` | rust+web |
| Restore this version | :330 | `GET /api/v1/repos/{owner}/{name}/wiki` | `RepoRead` | rust+web |
| i18n:wiki.save | :344 | `PATCH /api/v1/repos/{owner}/{name}/wiki/{title}` | `RepoWrite` | rust+browser |
| i18n:wiki.save | :344 | `GET /api/v1/repos/{owner}/{name}/wiki/{title}` | `RepoRead` | rust+web+browser |
| i18n:wiki.save | :344 | `GET /api/v1/repos/{owner}/{name}/wiki` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/wiki/[title]/history`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.view | :166 | `GET /api/v1/repos/{owner}/{name}/wiki/{title}/revisions/{rev_id}` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/wiki/{title}/history` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/admin/audit` 🔒

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| applyFilter | :173 | `GET /api/v1/admin/audit/logs` | `InstanceAdmin` | rust+web+smoke+browser |
| : All User Repository Organization | :178 | `GET /api/v1/admin/audit/logs` | `InstanceAdmin` | rust+web+smoke |
| Clear filters | :185 | `GET /api/v1/admin/audit/logs` | `InstanceAdmin` | rust+web+smoke |
| i18n:admin.audit.fields.details | :241 | `GET /api/v1/admin/audit/logs/{id}` | `InstanceAdmin` | rust+web+smoke+browser |
| ← Prev | :254 | `GET /api/v1/admin/audit/logs` | `InstanceAdmin` | rust+web+smoke |
| Next → | :256 | `GET /api/v1/admin/audit/logs` | `InstanceAdmin` | rust+web+smoke |

### `/admin/orgs` 🔒

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| ← Prev | :158 | `GET /api/v1/admin/orgs` | `InstanceAdmin` | rust+web |
| Next → | :160 | `GET /api/v1/admin/orgs` | `InstanceAdmin` | rust+web |
| i18n:common.loading | :184 | `DELETE /api/v1/admin/orgs/{name}` | `InstanceAdmin` | rust+smoke+browser |
| i18n:common.loading | :184 | `GET /api/v1/admin/orgs` | `InstanceAdmin` | rust+web+browser |

### `/admin/runners` 🔒

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.loading | :244 | `POST /api/v1/runners/register` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:common.loading | :244 | `GET /api/v1/admin/runners` | `InstanceAdmin` | web+smoke+browser |
| i18n:common.previous | :300 | `GET /api/v1/admin/runners` | `InstanceAdmin` | web+smoke |
| i18n:common.next | :302 | `GET /api/v1/admin/runners` | `InstanceAdmin` | web+smoke |
| i18n:common.loading | :327 | `DELETE /api/v1/admin/runners/{id}` | `InstanceAdmin` | rust+smoke+browser |
| i18n:common.loading | :327 | `GET /api/v1/admin/runners` | `InstanceAdmin` | web+smoke+browser |

### `/admin/settings` 🔒

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| saveSettings | :449 | `PATCH /api/v1/admin/settings` | `InstanceAdmin` | rust+web+smoke+browser |
| () => testSsoProvider(provider) | :490 | `POST /api/v1/admin/sso/providers/{id}/test` | `InstanceAdmin` | rust+web+smoke+browser |
| () => toggleSsoProvider(provider) | :494 | `PATCH /api/v1/admin/sso/providers/{id}` | `InstanceAdmin` | rust+web+smoke+browser |
| () => toggleSsoProvider(provider) | :494 | `GET /api/v1/admin/sso/providers` | `InstanceAdmin` | rust+web+smoke+browser |
| Delete | :498 | `DELETE /api/v1/admin/sso/providers/{id}` | `InstanceAdmin` | rust+web+smoke+browser |
| Delete | :498 | `GET /api/v1/admin/sso/providers` | `InstanceAdmin` | rust+web+smoke+browser |
| saveSsoProvider | :616 | `PATCH /api/v1/admin/sso/providers/{id}` | `InstanceAdmin` | rust+web+smoke |
| saveSsoProvider | :616 | `POST /api/v1/admin/sso/providers` | `InstanceAdmin` | rust+smoke+browser |
| saveSsoProvider | :616 | `GET /api/v1/admin/sso/providers` | `InstanceAdmin` | rust+web+smoke+browser |
| () => loadLoginAttempts(loginAttemptsPag | :632 | `GET /api/v1/admin/login-attempts` | `InstanceAdmin` | rust+web+smoke+browser |
| Apply | :649 | `GET /api/v1/admin/login-attempts` | `InstanceAdmin` | rust+web+smoke |
| Previous | :668 | `GET /api/v1/admin/login-attempts` | `InstanceAdmin` | rust+web+smoke |
| Next | :670 | `GET /api/v1/admin/login-attempts` | `InstanceAdmin` | rust+web+smoke |
| _(загрузка страницы)_ | — | `GET /api/v1/admin/settings` | `InstanceAdmin` | rust+web+smoke+browser |

### `/admin/users` 🔒

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| () => handleUnlock(u) | :246 | `POST /api/v1/admin/users/{id}/unlock` | `InstanceAdmin` | rust+web+smoke+browser |
| () => handleUnlock(u) | :246 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+web+browser |
| ← Prev | :264 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+web |
| Next → | :266 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+web |
| i18n:common.loading | :310 | `PATCH /api/v1/admin/users/{id}` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:common.loading | :310 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+web+browser |
| i18n:common.loading | :337 | `DELETE /api/v1/admin/users/{id}` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:common.loading | :337 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+web+browser |

### `/dashboard`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| )} * / | :245 | `POST /api/v1/repos` | `User` | rust+web+smoke+browser |
| i18n:common.loading | :253 | `GET /api/v1/orgs` | `User` | rust+web+smoke+browser |
| i18n:common.loading | :310 | `GET /api/v1/repos/templates/gitignores` | `Public` | web |
| i18n:common.loading | :310 | `GET /api/v1/repos/templates/licenses` | `Public` | web |
| i18n:common.loading | :310 | `GET /api/v1/repos/templates/readmes` | `Public` | web |
| i18n:common.loading | :310 | `GET /api/v1/repos/templates/labels` | `Public` | web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}` | `PublicFiltered` | rust+web+browser |

### `/explore`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| ← | :82 | `GET /api/v1/repos/explore` | `PublicFiltered` | rust+web+smoke |
| → | :88 | `GET /api/v1/repos/explore` | `PublicFiltered` | rust+web+smoke |

### `/forgot-password`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Email | :54 | `POST /api/v1/users/forgot-password` | `Public` | rust |

### `/imports`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Refresh | :206 | `GET /api/v1/imports` | `User` | rust+web |
| Platform GitHub GitLab Gitea Git Source repository URL Targe | :218 | `POST /api/v1/imports` | `User` | rust+web |
| Platform GitHub GitLab Gitea Git Source repository URL Targe | :218 | `GET /api/v1/imports` | `User` | rust+web |
| () => deleteImport(task.id) | :309 | `DELETE /api/v1/imports/{id}` | `User` | rust+web |

### `/login`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:auth.login.sso_retry | :187 | `GET /api/v1/auth/sso/providers` | `Public` | rust+web |

### `/notifications`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| load | :135 | `GET /api/v1/notifications` | `User` | rust+web |
| load | :135 | `GET /api/v1/notifications/unread-count` | `User` | rust+web |
| i18n:notifications.mark_all_read | :139 | `POST /api/v1/notifications/mark-all-read` | `User` | rust+web |
| i18n:notifications.mark_all_read | :139 | `GET /api/v1/notifications` | `User` | rust+web |
| i18n:notifications.mark_all_read | :139 | `GET /api/v1/notifications/unread-count` | `User` | rust+web |
| i18n:notifications.mark_read | :172 | `POST /api/v1/notifications/{id}/read` | `User` | rust+web |
| i18n:notifications.mark_read | :172 | `GET /api/v1/notifications` | `User` | rust+web |
| i18n:notifications.mark_read | :172 | `GET /api/v1/notifications/unread-count` | `User` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/ws/notifications` | `Foreign:ws.rs` | rust+web |

### `/orgs`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| * | :115 | `GET /api/v1/orgs` | `User` | rust+web+smoke |

### `/orgs/[name]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.loading | :523 | `DELETE /api/v1/orgs/{name}` | `OrgAdmin` | rust+web+smoke+browser |
| editingOrg = false} disabled= > | :531 | `PATCH /api/v1/orgs/{name}` | `OrgAdmin` | rust+web+smoke+browser |
| i18n:common.loading | :580 | `POST /api/v1/repos` | `User` | rust+web+smoke |
| i18n:common.loading | :580 | `GET /api/v1/repos/{owner}` | `PublicFiltered` | rust+web |
| i18n:orgs.new_team | :616 | `POST /api/v1/orgs/{name}/teams` | `OrgAdmin` | rust+browser |
| i18n:orgs.new_team | :616 | `GET /api/v1/orgs/{name}/teams` | `OrgRead` | rust+web |
| i18n:orgs.hide_team_members | :641 | `GET /api/v1/orgs/{name}/teams/{team_id}/members` | `OrgRead` | rust+web |
| ` ? t('common.loading') : t('common.delete')} | :645 | `DELETE /api/v1/orgs/{name}/teams/{team_id}` | `OrgAdmin` | rust+web+smoke+browser |
| ` ? t('common.loading') : t('common.delete')} | :645 | `GET /api/v1/orgs/{name}/teams` | `OrgRead` | rust+web |
| ` ? t('common.loading') : t('common.add')} | :656 | `POST /api/v1/orgs/{name}/teams/{team_id}/members` | `OrgAdmin` | rust+web+browser |
| ` ? t('common.loading') : t('common.add')} | :656 | `GET /api/v1/orgs/{name}/teams/{team_id}/members` | `OrgRead` | rust+web |
| -$ ` ? t('common.loading') : t('common.delete')} | :684 | `DELETE /api/v1/orgs/{name}/teams/{team_id}/members/{user_id}` | `OrgAdmin` | rust+web+browser |
| -$ ` ? t('common.loading') : t('common.delete')} | :684 | `GET /api/v1/orgs/{name}/teams/{team_id}/members` | `OrgRead` | rust+web |
| i18n:orgs.member_placeholder | :709 | `POST /api/v1/orgs/{name}/members` | `OrgAdmin` | rust+web+smoke+browser |
| i18n:orgs.member_placeholder | :709 | `GET /api/v1/orgs/{name}/members` | `OrgRead` | rust+web |
| ` ? t('common.loading') : t('common.delete')} | :735 | `DELETE /api/v1/orgs/{name}/members/{user_id}` | `OrgAdmin` | rust+web+smoke+browser |
| ` ? t('common.loading') : t('common.delete')} | :735 | `GET /api/v1/orgs/{name}/members` | `OrgRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/orgs/{name}` | `OrgRead` | rust+web+smoke |

### `/reset-password`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| New Password Confirm Password | :99 | `POST /api/v1/users/reset-password` | `Public` | rust |

### `/search`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| _(загрузка страницы)_ | — | `GET /api/v1/search` | `PublicFiltered` | rust+web |

### `/settings/agents`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Username Display name | :249 | `POST /api/v1/users/bots` | `User` | rust+web |
| Username Display name | :249 | `GET /api/v1/users/bots` | `User` | rust+web |
| () => openTokens(bot) | :283 | `GET /api/v1/users/bots/{bot}/tokens` | `User` | rust+web |
| () => deleteBot(bot) | :286 | `DELETE /api/v1/users/bots/{bot}` | `User` | rust+web |
| () => deleteBot(bot) | :286 | `GET /api/v1/users/bots` | `User` | rust+web |
| Token name Expires Repositories (owner/name, one per line or | :310 | `POST /api/v1/users/bots/{bot}/tokens` | `User` | rust+web |
| Token name Expires Repositories (owner/name, one per line or | :310 | `GET /api/v1/users/bots/{bot}/tokens` | `User` | rust+web |
| () => revokeToken(token) | :364 | `DELETE /api/v1/users/bots/{bot}/tokens/{id}` | `User` | web |
| () => revokeToken(token) | :364 | `GET /api/v1/users/bots/{bot}/tokens` | `User` | rust+web |

### `/settings/security`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Retry reading MFA state | :325 | `GET /api/v1/users/mfa/backup` | `User` | rust+web |
| Current password | :340 | `POST /api/v1/users/mfa/backup/regenerate` | `User` | rust |
| Current password | :340 | `GET /api/v1/users/mfa/backup` | `User` | rust+web |
| Current password | :340 | `GET /api/v1/users/passkeys` | `User` | rust+web |
| Current password | :340 | `GET /api/v1/users/me/sso` | `User` | rust+web |
| Current password | :358 | `POST /api/v1/users/mfa/disable` | `User` | rust |
| Current password | :358 | `GET /api/v1/users/mfa/backup` | `User` | rust+web |
| Current password | :358 | `GET /api/v1/users/passkeys` | `User` | rust+web |
| Current password | :358 | `GET /api/v1/users/me/sso` | `User` | rust+web |
| startSetup | :369 | `POST /api/v1/users/mfa/setup` | `User` | rust+web |
| Retry reading passkeys | :400 | `GET /api/v1/users/passkeys` | `User` | rust+web |
| Remove | :414 | `DELETE /api/v1/users/passkeys/{id}` | `User` | rust |
| Retry reading linked accounts | :468 | `GET /api/v1/users/me/sso` | `User` | rust+web |
| Unlink | :483 | `DELETE /api/v1/auth/sso/{slug}/unlink` | `User` | rust+web |
| Authentication code | :507 | `POST /api/v1/users/mfa/enable` | `User` | rust |
| Authentication code | :507 | `GET /api/v1/users/mfa/backup` | `User` | rust+web |
| Authentication code | :507 | `GET /api/v1/users/passkeys` | `User` | rust+web |
| Authentication code | :507 | `GET /api/v1/users/me/sso` | `User` | rust+web |

### `/settings/ssh-keys`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:ssh_keys.name | :123 | `POST /api/v1/users/ssh-keys` | `User` | rust+web+smoke |
| i18n:ssh_keys.name | :123 | `GET /api/v1/users/ssh-keys` | `User` | rust+web |
| i18n:ssh_keys.deleting | :175 | `DELETE /api/v1/users/ssh-keys/{id}` | `User` | rust |
| i18n:ssh_keys.deleting | :175 | `GET /api/v1/users/ssh-keys` | `User` | rust+web |

### `/settings/tokens`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Name Scopes Expires | :176 | `POST /api/v1/users/tokens` | `User` | rust+web |
| Name Scopes Expires | :176 | `GET /api/v1/users/tokens` | `User` | rust+web |
| () => revokeToken(token) | :227 | `DELETE /api/v1/users/tokens/{id}` | `User` | rust |
| () => revokeToken(token) | :227 | `GET /api/v1/users/tokens` | `User` | rust+web |

## Роуты, недостижимые из браузера

Не дефект: git, OCI, LFS, CI-раннер и вебхуки — легитимные не-браузерные
клиенты. Это список того, что браузерный тест закрыть не может в принципе.

| Метод | URL | `Access` | тест |
|---|---|---|---|
| GET | `/v2/` | `Foreign:oci.rs` | rust |
| GET | `/v2` | `Foreign:oci.rs` | rust |
| GET | `/v2/auth/token` | `Public` | rust |
| GET | `/v2/{owner}/{repo}/tags/list` | `Foreign:oci.rs` | rust |
| GET | `/v2/{owner}/{repo}/manifests/{reference}` | `Foreign:oci.rs` | rust+smoke |
| HEAD | `/v2/{owner}/{repo}/manifests/{reference}` | `Foreign:oci.rs` | rust |
| PUT | `/v2/{owner}/{repo}/manifests/{reference}` | `Foreign:oci.rs` | rust |
| GET | `/v2/{owner}/{repo}/blobs/{digest}` | `Foreign:oci.rs` | rust |
| HEAD | `/v2/{owner}/{repo}/blobs/{digest}` | `Foreign:oci.rs` | rust |
| POST | `/v2/{owner}/{repo}/blobs/uploads/` | `Foreign:oci.rs` | rust |
| POST | `/v2/{owner}/{repo}/blobs/uploads` | `Foreign:oci.rs` | rust |
| PATCH | `/v2/{owner}/{repo}/blobs/uploads/{uuid}` | `Foreign:oci.rs` | rust |
| GET | `/v2/{owner}/{repo}/blobs/uploads/{uuid}` | `Foreign:oci.rs` | rust+smoke |
| PUT | `/v2/{owner}/{repo}/blobs/uploads/{uuid}` | `Foreign:oci.rs` | rust |
| GET | `/api-docs/openapi.json` | `User` | rust+smoke |
| GET | `/api-docs` | `User` | rust |
| GET | `/api-docs/` | `User` | rust+smoke |
| GET | `/api-docs/{*tail}` | `User` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/cargo/index/config.json` | `RepoRead` | rust |
| PUT | `/api/v1/repos/{owner}/{name}/packages/cargo/api/v1/crates/new` | `RepoWrite` | rust |
| DELETE | `/api/v1/repos/{owner}/{name}/packages/cargo/api/v1/crates/{crate_name}/{version}/yank` | `RepoWrite` | rust |
| PUT | `/api/v1/repos/{owner}/{name}/packages/cargo/api/v1/crates/{crate_name}/{version}/unyank` | `RepoWrite` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/rubygems/versions` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/rubygems/info/{gem_name}` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/rubygems/names` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/rubygems/gems/{filename}` | `RepoRead` | rust |
| POST | `/api/v1/repos/{owner}/{name}/packages/rubygems/api/v1/gems` | `RepoWrite` | rust |
| GET | `/git/{owner}/{repo}/info/refs` | `Foreign:git_http.rs` | rust |
| POST | `/git/{owner}/{repo}/git-upload-pack` | `Foreign:git_http.rs` | rust |
| POST | `/git/{owner}/{repo}/git-receive-pack` | `Foreign:git_http.rs` | rust |
| GET | `/{owner}/{repo}/info/refs` | `Foreign:git_http.rs` | rust |
| POST | `/{owner}/{repo}/git-upload-pack` | `Foreign:git_http.rs` | rust |
| POST | `/{owner}/{repo}/git-receive-pack` | `Foreign:git_http.rs` | rust |
| GET | `/metrics` | `Public` | rust+smoke |
| POST | `/api/v1/users/register` | `Public` | rust+smoke |
| POST | `/api/v1/users/login` | `Public` | rust+smoke |
| POST | `/api/v1/users/logout` | `User` | rust+web |
| GET | `/api/v1/users/me` | `User` | rust+web+smoke |
| POST | `/api/v1/users/mfa/verify` | `Public` | rust |
| POST | `/api/v1/users/passkeys/register/start` | `User` | rust |
| POST | `/api/v1/users/passkeys/register/finish` | `User` | rust |
| POST | `/api/v1/users/passkeys/login/start` | `Public` | rust |
| POST | `/api/v1/users/passkeys/login/finish` | `Public` | rust |
| GET | `/api/v1/auth/sso/{slug}` | `Public` | rust |
| GET | `/api/v1/auth/sso/{slug}/callback` | `Public` | rust |
| GET | `/api/v1/repos/{owner}/{name}/labels/{id}` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/issue_config/validate` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/issues/{number}/labels` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/pulls/{number}/reviews/{id}` | `RepoRead` | rust |
| POST | `/api/v1/repos/{owner}/{name}/lfs/objects/batch` | `Foreign:api/lfs.rs` | rust |
| GET | `/api/v1/repos/{owner}/{name}/lfs/objects/{oid}` | `Foreign:api/lfs.rs` | rust+smoke |
| PUT | `/api/v1/repos/{owner}/{name}/lfs/objects/{oid}` | `Foreign:api/lfs.rs` | rust |
| POST | `/api/v1/repos/{owner}/{name}/lfs/locks` | `Foreign:api/lfs_locks.rs` | **—** |
| POST | `/api/v1/repos/{owner}/{name}/lfs/locks/verify` | `Foreign:api/lfs_locks.rs` | **—** |
| GET | `/api/v1/ci/oidc/.well-known/openid-configuration` | `Public` | rust |
| GET | `/api/v1/ci/oidc/jwks` | `Public` | rust |
| GET | `/api/v1/ci/oidc/token` | `Foreign:api/ci_oidc.rs` | rust |
| GET | `/api/v1/repos/{owner}/{name}/branches/protection/{id}` | `RepoRead` | rust |
| GET | `/api/v1/imports/{id}` | `User` | rust |
| POST | `/api/v1/repos/{owner}/{name}/boards/{id}/columns` | `RepoWrite` | rust |
| PATCH | `/api/v1/repos/{owner}/{name}/boards/{id}/columns/{col_id}` | `RepoWrite` | rust |
| POST | `/api/v1/repos/{owner}/{name}/statuses/{sha}` | `RepoWrite` | rust |
| POST | `/api/v1/orgs` | `User` | rust+smoke |
| GET | `/api/v1/orgs/{name}/teams/{team_id}` | `OrgRead` | rust |
| DELETE | `/api/v1/notifications/{id}` | `User` | rust |
| GET | `/api/v1/repos/{owner}/{name}/releases/assets/{asset_id}` | `RepoRead` | rust |
| POST | `/api/v1/repos/{owner}/{name}/packages/npm/publish` | `RepoWrite` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/npm/list` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/npm/-/npm/v1/attestations/{package_spec}` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/npm/{pkg_name}` | `RepoRead` | rust+smoke |
| PUT | `/api/v1/repos/{owner}/{name}/packages/npm/{pkg_name}` | `RepoWrite` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/npm/-/package/{pkg_name}/dist-tags` | `RepoRead` | rust |
| PUT | `/api/v1/repos/{owner}/{name}/packages/npm/-/package/{pkg_name}/dist-tags/{tag}` | `RepoWrite` | rust |
| DELETE | `/api/v1/repos/{owner}/{name}/packages/npm/-/package/{pkg_name}/dist-tags/{tag}` | `RepoWrite` | rust |
| POST | `/api/v1/repos/{owner}/{name}/packages/pypi/legacy/` | `RepoWrite` | rust |
| POST | `/api/v1/repos/{owner}/{name}/packages/pypi/legacy` | `RepoWrite` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/pypi/simple/` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/pypi/simple` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/pypi/simple/{pkg_name}/` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/pypi/simple/{pkg_name}` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/nuget/index.json` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/nuget/registration/{id}/index.json` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/nuget/registration/{id}/{version}` | `RepoRead` | rust |
| HEAD | `/api/v1/repos/{owner}/{name}/packages/nuget/registration/{id}/{version}` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/nuget/query` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/nuget/autocomplete` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/nuget/package/{id}/index.json` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/nuget/package/{id}/{version}/{file}` | `RepoRead` | rust |
| POST | `/api/v1/repos/{owner}/{name}/packages/nuget/publish` | `RepoWrite` | rust |
| PUT | `/api/v1/repos/{owner}/{name}/packages/nuget/publish` | `RepoWrite` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/rubygems/api/v1/dependencies.json` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/rubygems/api/v1/gems/{gem_name}` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/helm/index.yaml` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/composer/packages.json` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}` | `RepoRead` | rust |
| POST | `/api/v1/runners/{id}/heartbeat` | `Foreign:RUNNER_AUTH_LAYER` | rust |
| POST | `/api/v1/runners/{id}/deregister` | `Foreign:RUNNER_AUTH_LAYER` | rust |
| GET | `/api/v1/runners/{id}/jobs/poll` | `Foreign:RUNNER_AUTH_LAYER` | rust |
| POST | `/api/v1/runners/{id}/jobs/{job_id}/start` | `Foreign:RUNNER_AUTH_LAYER` | rust |
| POST | `/api/v1/runners/{id}/jobs/{job_id}/log` | `Foreign:RUNNER_AUTH_LAYER` | rust |
| GET | `/api/v1/runners/{id}/jobs/{job_id}/workspace` | `Foreign:RUNNER_AUTH_LAYER` | rust |
| GET | `/api/v1/runners/{id}/jobs/{job_id}/cache` | `Foreign:RUNNER_AUTH_LAYER` | rust+smoke |
| PUT | `/api/v1/runners/{id}/jobs/{job_id}/cache` | `Foreign:RUNNER_AUTH_LAYER` | rust |
| POST | `/api/v1/runners/{id}/jobs/{job_id}/finish` | `Foreign:RUNNER_AUTH_LAYER` | rust |
| PUT | `/api/v1/runners/{id}/jobs/{job_id}/artifacts/staging` | `Foreign:RUNNER_AUTH_LAYER` | rust |
| POST | `/api/v1/runners/{id}/jobs/{job_id}/artifacts` | `Foreign:RUNNER_AUTH_LAYER` | rust |
| GET | `/api/v1/artifacts/{id}` | `RepoRead` | rust |
| GET | `/api/v1/admin/runners/{id}` | `InstanceAdmin` | rust |
| GET | `/api/v1/admin/users/{id}` | `InstanceAdmin` | rust |
| GET | `/api/v1/admin/orgs/{name}` | `InstanceAdmin` | rust |
| GET | `/api/v1/admin/sso/providers/{id}` | `InstanceAdmin` | rust |
| POST | `/api/v1/repos/{owner}/{name}/webhooks/external/ci` | `RepoWrite` | rust |
| GET | `/api/v1/ai/repos/{owner}/{name}/summary` | `RepoRead` | rust |
| GET | `/api/v1/ai/repos/{owner}/{name}/issues` | `RepoRead` | rust |
| GET | `/api/v1/ai/repos/{owner}/{name}/prs` | `RepoRead` | rust |
| GET | `/api/v1/ai/repos/{owner}/{name}/tree` | `RepoRead` | rust |
| GET | `/api/v1/ai/repos/{owner}/{name}/search/code` | `RepoRead` | rust |
| POST | `/api/v1/ai/repos/{owner}/{name}/index` | `RepoWrite` | rust |
| POST | `/api/v1/mcp` | `User` | rust |

