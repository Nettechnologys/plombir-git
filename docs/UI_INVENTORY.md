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
| Роутов в роутере (с объявленным `Access`) | 409 |
| Из них достижимы из браузера | 283 (69%) |
| Layout-модулей | 2 |
| Страниц | 69 |
| Интерактивных элементов | 993 |
| — из них дёргают API | 348 |
| — приходят из общих компонентов | 395 |
| Browser sweep: сценариев / записей инвентаря / роутов | 46 / 171 / 162 |
| **UI-роутов без единого web/smoke/browser-теста** | **26** |
| UI-роутов без corpus-hit и browser-сценария | 0 |

## По уровню доступа

| `Access` | роутов | достижимы из UI | нет фронт-теста | нет corpus/browser coverage |
|---|---:|---:|---:|---:|
| `RepoRead` | 106 | 68 | 4 | 0 |
| `RepoWrite` | 85 | 66 | 13 | 2 |
| `User` | 52 | 41 | 6 | 0 |
| `RepoAdmin` | 39 | 38 | 0 | 1 |
| `Public` | 25 | 10 | 2 | 0 |
| `InstanceAdmin` | 25 | 21 | 0 | 0 |
| `RepoAuthRead` | 18 | 18 | 1 | 0 |
| `Foreign:oci.rs` | 13 | 0 | 0 | 0 |
| `Foreign:RUNNER_AUTH_LAYER` | 12 | 0 | 0 | 0 |
| `OrgAdmin` | 8 | 8 | 0 | 0 |
| `Foreign:git_http.rs` | 6 | 0 | 0 | 0 |
| `OrgRead` | 5 | 4 | 0 | 0 |
| `Foreign:api/lfs_locks.rs` | 4 | 2 | 0 | 1 |
| `PublicFiltered` | 3 | 3 | 0 | 0 |
| `Foreign:api/lfs.rs` | 3 | 0 | 0 | 0 |
| `RepoOwner` | 2 | 2 | 0 | 0 |
| `Foreign:ws.rs` | 2 | 2 | 0 | 0 |
| `Foreign:api/ci_oidc.rs` | 1 | 0 | 0 | 0 |

## Страницы

| Страница | элементов | дёргают API | из компонентов |
|---|---:|---:|---:|
| `/[owner]/[repo]/pulls/[number]` | 67 | 35 | 24 |
| `/[owner]/[repo]/boards` | 42 | 14 | 15 |
| `/[owner]/[repo]/issues/[number]` | 40 | 17 | 23 |
| `/[owner]/[repo]/pulls` | 28 | 12 | 12 |
| `/[owner]/[repo]/releases` | 27 | 12 | 11 |
| `/[owner]/[repo]/issues` | 26 | 9 | 12 |
| `/orgs/[name]` | 26 | 10 | 2 |
| `/[owner]/[repo]/pipelines` | 25 | 11 | 14 |
| `/[owner]/[repo]` | 24 | 5 | 12 |
| `/[owner]/[repo]/wiki/[title]` | 24 | 9 | 13 |
| `/admin/users` 🔒 | 24 | 7 | 4 |
| `/[owner]/[repo]/blob/[...path]` | 23 | 7 | 11 |
| `/[owner]/[repo]/branches` | 22 | 6 | 12 |
| `/[owner]/[repo]/milestones` | 22 | 8 | 13 |
| `/[owner]/[repo]/releases/new` | 21 | 5 | 11 |
| `/[owner]/[repo]/settings` | 20 | 5 | 4 |
| `/[owner]/[repo]/time_tracking` | 20 | 10 | 13 |
| `/[owner]/[repo]/network` | 19 | 6 | 11 |
| `/admin/settings` 🔒 | 19 | 9 | 2 |
| `/[owner]/[repo]/packages` | 18 | 9 | 11 |
| `/[owner]/[repo]/releases/edit/[id]` | 18 | 5 | 11 |
| `/[owner]/[repo]/tags` | 18 | 5 | 12 |
| `/settings/security` | 18 | 10 | 2 |
| `/[owner]/[repo]/packages/[format]/[...name]` | 17 | 6 | 11 |
| `/[owner]/[repo]/packages/upload` | 17 | 5 | 11 |
| `/[owner]/[repo]/wiki` | 17 | 5 | 11 |
| `/[owner]/[repo]/commits` | 16 | 6 | 12 |
| `/imports` | 16 | 3 | 2 |
| `/[owner]/[repo]/settings/branches` | 15 | 2 | 2 |
| `/[owner]/[repo]/wiki/[title]/history` | 15 | 5 | 11 |
| `/settings/profile` | 15 | 8 | 2 |
| `/[owner]/[repo]/commits/[sha]` | 14 | 6 | 11 |
| `/dashboard` | 14 | 3 | 0 |
| `/[owner]/[repo]/compare/[...spec]` | 13 | 4 | 11 |
| `/[owner]/[repo]/packages/[format]` | 13 | 4 | 11 |
| `/[owner]/[repo]/settings/webhooks` | 13 | 6 | 2 |
| `/settings/agents` | 13 | 5 | 2 |
| `/login` | 12 | 1 | 0 |
| `/admin/runners` 🔒 | 11 | 4 | 1 |
| `/[owner]/[repo]/settings/labels` | 10 | 2 | 2 |
| `/admin/audit` 🔒 | 9 | 6 | 1 |
| `/[owner]/[repo]/edit/[...path]` | 8 | 0 | 8 |
| `/[owner]/[repo]/new` | 8 | 0 | 8 |
| `/[owner]/[repo]/settings/collaborators` | 8 | 3 | 2 |
| `/[owner]/[repo]/settings/environments` | 8 | 2 | 2 |
| `/admin/orgs` 🔒 | 8 | 3 | 1 |
| `/orgs` | 8 | 1 | 0 |
| `/search` | 8 | 0 | 0 |
| `/` | 7 | 1 | 0 |
| `/[owner]/[repo]/settings/deploy-keys` | 7 | 2 | 2 |
| `/[owner]/[repo]/settings/lfs-storage` | 7 | 4 | 2 |
| `/[owner]/[repo]/settings/mirror` | 7 | 3 | 2 |
| `/[owner]/[repo]/settings/tags` | 7 | 2 | 2 |
| `/settings/ssh-keys` | 6 | 2 | 2 |
| `/settings/tokens` | 6 | 2 | 2 |
| `/[owner]/[repo]/settings/ci-secrets` | 5 | 2 | 2 |
| `/admin` 🔒 | 5 | 0 | 0 |
| `/help` | 5 | 0 | 0 |
| `/reset-password` | 5 | 1 | 0 |
| `/[owner]` | 4 | 0 | 0 |
| `/[owner]/[repo]/settings/lfs-locks` | 4 | 2 | 2 |
| `/notifications` | 4 | 4 | 0 |
| `/[owner]/[repo]/settings/retention` | 3 | 2 | 0 |
| `/explore` | 3 | 2 | 0 |
| `/forgot-password` | 3 | 1 | 0 |
| `/register` | 3 | 0 | 0 |
| `/verify-email` | 3 | 1 | 0 |
| `/[owner]/[repo]/settings/runners` | 1 | 0 | 0 |
| `/settings/notifications` | 1 | 1 | 0 |

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
| i18n:common.retry | :156 | `GET /api/v1/repos/explore` | `PublicFiltered` | rust+web+smoke |

### `/[owner]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}` | `PublicFiltered` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/orgs/{name}` | `OrgRead` | rust+web+smoke |
| _(загрузка страницы)_ | — | `GET /api/v1/orgs` | `User` | rust+web+smoke |

### `/[owner]/[repo]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.retry | :426 | `GET /api/v1/repos/{owner}/{name}/blob/{*path}` | `RepoRead` | rust+web |
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
| i18n:repo.blob.download | :511 | `GET /api/v1/repos/{owner}/{name}/raw/{*path}` | `RepoRead` | web |
| i18n:repo.blob.deleting | :566 | `DELETE /api/v1/repos/{owner}/{name}/contents/{*path}` | `RepoWrite` | rust+web+browser |
| i18n:repo.blob.download | :587 | `GET /api/v1/repos/{owner}/{name}/raw/{*path}` | `RepoRead` | web |
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
| i18n:common.create | :487 | `POST /api/v1/repos/{owner}/{name}/boards` | `RepoWrite` | rust+web+browser |
| i18n:common.create | :487 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust+web+browser |
| i18n:common.save | :517 | `PATCH /api/v1/repos/{owner}/{name}/boards/{id}/cards/{card_id}` | `RepoWrite` | web+browser |
| i18n:common.save | :517 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust+web |
| &times; | :544 | `DELETE /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoWrite` | rust+web+browser |
| &times; | :544 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust+web |
| i18n:common.save | :586 | `PATCH /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoWrite` | web+browser |
| &times; | :623 | `DELETE /api/v1/repos/{owner}/{name}/boards/{id}/columns/{col_id}` | `RepoWrite` | rust+web+browser |
| ↑ | :634 | `POST /api/v1/repos/{owner}/{name}/boards/{id}/cards/reorder` | `RepoWrite` | rust+web |
| ↑ | :634 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust+web |
| ↓ | :641 | `POST /api/v1/repos/{owner}/{name}/boards/{id}/cards/reorder` | `RepoWrite` | rust+web+browser |
| ↓ | :641 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust+web |
| &times; | :649 | `DELETE /api/v1/repos/{owner}/{name}/boards/{id}/cards/{card_id}` | `RepoWrite` | rust+web+browser |
| i18n:board.moveTo | :660 | `POST /api/v1/repos/{owner}/{name}/boards/{id}/cards/{card_id}/move` | `RepoWrite` | rust+web+browser |
| i18n:board.moveTo | :660 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust+web |
| i18n:common.add | :679 | `POST /api/v1/repos/{owner}/{name}/boards/{id}/columns/{col_id}/cards` | `RepoWrite` | rust+web+browser |
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

### `/[owner]/[repo]/branches`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| (showCreate = false)} disabled= > | :186 | `POST /api/v1/repos/{owner}/{name}/branches` | `RepoWrite` | web |
| (showCreate = false)} disabled= > | :186 | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | web |
| (showCreate = false)} disabled= > | :186 | `GET /api/v1/repos/{owner}/{name}/branches/protection` | `RepoRead` | rust+web |
| i18n:repo.branches.deleting | :262 | `DELETE /api/v1/repos/{owner}/{name}/branches/{branch}` | `RepoWrite` | web |
| i18n:repo.branches.deleting | :262 | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | web |
| i18n:repo.branches.deleting | :262 | `GET /api/v1/repos/{owner}/{name}/branches/protection` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/commits`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.retry | :171 | `GET /api/v1/repos/{owner}/{name}/log` | `RepoRead` | rust+web |
| i18n:common.loading | :210 | `GET /api/v1/repos/{owner}/{name}/log` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/commits/[sha]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.retry | :175 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/status` | `RepoRead` | rust+web+browser |
| i18n:common.retry | :175 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/statuses` | `RepoRead` | rust+web+browser |
| i18n:common.retry | :175 | `GET /api/v1/repos/{owner}/{name}/log` | `RepoRead` | rust+web |
| i18n:common.retry | :175 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/signature` | `RepoRead` | rust+web+browser |
| i18n:common.retry | :229 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/status` | `RepoRead` | rust+web |
| i18n:common.retry | :229 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/statuses` | `RepoRead` | rust+web |
| i18n:common.retry | :229 | `GET /api/v1/repos/{owner}/{name}/log` | `RepoRead` | rust+web |
| i18n:common.retry | :229 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/signature` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/compare/[...spec]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/compare` | `RepoRead` | rust+web |

### `/[owner]/[repo]/edit/[...path]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
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
| i18n:common.loading | :398 | `PATCH /api/v1/repos/{owner}/{name}/issues/{number}` | `RepoWrite` | rust+web |
| i18n:common.saving | :440 | `PATCH /api/v1/repos/{owner}/{name}/issues/comments/{comment_id}` | `RepoAdmin` | web+browser |
| i18n:issues.comment_placeholder | :457 | `POST /api/v1/repos/{owner}/{name}/issues/{number}/comments` | `RepoAuthRead` | rust+browser |
| i18n:issues.comment_placeholder | :457 | `GET /api/v1/repos/{owner}/{name}/issues/{number}` | `RepoRead` | rust+web+browser |
| i18n:issues.comment_placeholder | :457 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/comments` | `RepoRead` | rust+web+browser |
| i18n:issues.comment_placeholder | :457 | `GET /api/v1/repos/{owner}/{name}/milestones` | `RepoRead` | rust+web+browser |
| i18n:issues.comment_placeholder | :457 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust+web+browser |
| i18n:issues.comment_placeholder | :457 | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
| i18n:issues.close_issue | :462 | `PATCH /api/v1/repos/{owner}/{name}/issues/{number}` | `RepoWrite` | rust+web+browser |
| i18n:issues.close_issue | :462 | `GET /api/v1/repos/{owner}/{name}/issues/{number}` | `RepoRead` | rust+web |
| i18n:issues.close_issue | :462 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/comments` | `RepoRead` | rust+web |
| i18n:issues.close_issue | :462 | `GET /api/v1/repos/{owner}/{name}/milestones` | `RepoRead` | rust+web |
| i18n:issues.close_issue | :462 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust+web |
| i18n:issues.close_issue | :462 | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
| i18n:common.deleting | :491 | `DELETE /api/v1/repos/{owner}/{name}/issues/comments/{comment_id}` | `RepoAdmin` | rust+web+browser |
| i18n:common.deleting | :507 | `DELETE /api/v1/repos/{owner}/{name}/issues/{number}` | `RepoAdmin` | web+browser |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| i18n:subscription.unsubscribe | `ThreadSubscription` | `DELETE /api/v1/repos/{owner}/{name}/issues/{number}/subscription` | `RepoAuthRead` | web |
| i18n:subscription.unsubscribe | `ThreadSubscription` | `DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/subscription` | `RepoAuthRead` | web |
| i18n:subscription.unsubscribe | `ThreadSubscription` | `PUT /api/v1/repos/{owner}/{name}/issues/{number}/subscription` | `RepoAuthRead` | web |
| i18n:subscription.unsubscribe | `ThreadSubscription` | `PUT /api/v1/repos/{owner}/{name}/pulls/{number}/subscription` | `RepoAuthRead` | web |
| upload | `AttachmentPanel` | `POST /api/v1/repos/{owner}/{name}/issues/{number}/assets` | `RepoWrite` | rust |
| → item.browser_download_url | `AttachmentPanel` | `GET /api/v1/repos/{owner}/{name}/issues/{number}/assets/{attachment_id}` | `RepoRead` | rust |
| i18n:attachments.delete | `AttachmentPanel` | `DELETE /api/v1/repos/{owner}/{name}/issues/{number}/assets/{attachment_id}` | `RepoWrite` | rust |
| upload | `AttachmentPanel` | `POST /api/v1/repos/{owner}/{name}/issues/comments/{comment_id}/assets` | `RepoWrite` | rust |
| → item.browser_download_url | `AttachmentPanel` | `GET /api/v1/repos/{owner}/{name}/issues/comments/{comment_id}/assets/{attachment_id}` | `RepoRead` | rust |
| i18n:attachments.delete | `AttachmentPanel` | `DELETE /api/v1/repos/{owner}/{name}/issues/comments/{comment_id}/assets/{attachment_id}` | `RepoWrite` | rust |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/issues/{number}/subscription` | `RepoAuthRead` | web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/subscription` | `RepoAuthRead` | web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/issues/{number}/assets` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/issues/comments/{comment_id}/assets` | `RepoRead` | rust+web |

### `/[owner]/[repo]/milestones`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:milestones.name | :242 | `POST /api/v1/repos/{owner}/{name}/milestones` | `RepoWrite` | rust+web+browser |
| i18n:milestones.name | :242 | `PATCH /api/v1/repos/{owner}/{name}/milestones/{id}` | `RepoWrite` | rust+web+browser |
| i18n:milestones.name | :242 | `GET /api/v1/repos/{owner}/{name}/milestones` | `RepoRead` | rust+web+browser |
| i18n:common.edit | :304 | `GET /api/v1/repos/{owner}/{name}/milestones/{id}` | `RepoRead` | rust+web+browser |
| i18n:milestones.close | :305 | `PATCH /api/v1/repos/{owner}/{name}/milestones/{id}` | `RepoWrite` | rust+web |
| i18n:milestones.close | :305 | `GET /api/v1/repos/{owner}/{name}/milestones` | `RepoRead` | rust+web |
| i18n:common.delete | :308 | `DELETE /api/v1/repos/{owner}/{name}/milestones/{id}` | `RepoWrite` | rust+browser |
| i18n:common.delete | :308 | `GET /api/v1/repos/{owner}/{name}/milestones` | `RepoRead` | rust+web |
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
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `POST /api/v1/repos/{owner}/{name}/contents/{*path}` | `RepoWrite` | rust+web+browser |

### `/[owner]/[repo]/packages`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.retry | :167 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust+web |
| i18n:common.retry | :167 | `GET /api/v1/repos/{owner}/{name}/packages` | `RepoRead` | rust+web |
| i18n:common.all | :176 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust+web |
| i18n:common.all | :176 | `GET /api/v1/repos/{owner}/{name}/packages` | `RepoRead` | rust+web |
| i18n:common.search | :191 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust+web |
| i18n:common.search | :191 | `GET /api/v1/repos/{owner}/{name}/packages` | `RepoRead` | rust+web |
| i18n:common.previous | :230 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust+web |
| i18n:common.previous | :230 | `GET /api/v1/repos/{owner}/{name}/packages` | `RepoRead` | rust+web |
| i18n:common.next | :238 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust+web |
| i18n:common.next | :238 | `GET /api/v1/repos/{owner}/{name}/packages` | `RepoRead` | rust+web |
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
| i18n:packages.unyank | :269 | `PATCH /api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/yank` | `RepoWrite` | rust+web |
| i18n:packages.unyank | :269 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/versions` | `RepoRead` | rust+web+browser |
| i18n:common.delete | :300 | `DELETE /api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}` | `RepoWrite` | rust+web |
| i18n:common.delete | :300 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}` | `RepoRead` | rust+web+browser |
| i18n:common.delete | :300 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/versions` | `RepoRead` | rust+web |
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
| i18n:packages.format | :141 | `POST /api/v1/repos/{owner}/{name}/packages/{pkg_type}/publish` | `RepoWrite` | rust+web |
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
| updateTriggerRef(event.currentTarget.value)} onchange= place | :739 | `POST /api/v1/repos/{owner}/{name}/pipelines` | `RepoWrite` | rust+web+browser |
| updateTriggerRef(event.currentTarget.value)} onchange= place | :739 | `GET /api/v1/repos/{owner}/{name}/pipelines` | `RepoRead` | rust+web+browser |
| updateTriggerRef(event.currentTarget.value)} onchange= place | :739 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}` | `RepoRead` | rust+web+browser |
| updateTriggerRef(event.currentTarget.value)} onchange= place | :739 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}/artifacts` | `RepoRead` | rust+web+browser |
| i18n:pipeline.retry | :868 | `POST /api/v1/repos/{owner}/{name}/pipelines/{id}/retry` | `RepoWrite` | rust+web |
| i18n:pipeline.retry | :868 | `GET /api/v1/repos/{owner}/{name}/pipelines` | `RepoRead` | rust+web |
| i18n:pipeline.retry | :868 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}` | `RepoRead` | rust+web |
| i18n:pipeline.retry | :868 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}/artifacts` | `RepoRead` | rust+web |
| i18n:pipeline.cancel | :871 | `POST /api/v1/repos/{owner}/{name}/pipelines/{id}/cancel` | `RepoWrite` | rust |
| i18n:pipeline.play_manual | :931 | `POST /api/v1/repos/{owner}/{name}/pipelines/{id}/jobs/{job_id}/play` | `RepoWrite` | rust |
| i18n:pipeline.play_manual | :931 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}` | `RepoRead` | rust+web |
| i18n:pipeline.play_manual | :931 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}/artifacts` | `RepoRead` | rust+web |
| i18n:pipeline.approval_recorded | :934 | `POST /api/v1/repos/{owner}/{name}/pipelines/{pipeline_id}/jobs/{job_id}/approve` | `RepoAuthRead` | rust |
| i18n:pipeline.approval_recorded | :934 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}` | `RepoRead` | rust+web |
| i18n:pipeline.approval_recorded | :934 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}/artifacts` | `RepoRead` | rust+web |
| i18n:pipeline.artifact_downloading | :970 | `GET /api/v1/artifacts/{id}/download` | `RepoRead` | rust+web |
| i18n:pipeline.artifact_deleting | :976 | `DELETE /api/v1/artifacts/{id}` | `RepoWrite` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/pipelines/workflow-dispatch` | `RepoRead` | rust+web+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}/jobs/{job_id}` | `RepoRead` | rust+web+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/ws/job/{job_id}` | `Foreign:ws.rs` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/pulls`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:pulls.tabs.open | :349 | `GET /api/v1/repos/{owner}/{name}/pulls` | `RepoRead` | rust+web |
| i18n:pulls.tabs.closed | :356 | `GET /api/v1/repos/{owner}/{name}/pulls` | `RepoRead` | rust+web |
| i18n:pulls.tabs.merged | :363 | `GET /api/v1/repos/{owner}/{name}/pulls` | `RepoRead` | rust+web |
| i18n:pulls.new | :371 | `GET /api/v1/repos/{owner}/{name}/pull_request_template` | `RepoRead` | rust+web+browser |
| i18n:common.retry | :382 | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | web |
| selectHeadRepo(event.currentTarget.value)} disabled= > / / ` | :392 | `POST /api/v1/repos/{owner}/{name}/pulls` | `RepoAuthRead` | rust+web+browser |
| selectHeadRepo(event.currentTarget.value)} disabled= > / / ` | :392 | `GET /api/v1/repos/{owner}/{name}/pulls` | `RepoRead` | rust+web+browser |
| / / ` : ''} | :396 | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | web |
| i18n:common.retry | :417 | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/forks` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/pulls/[number]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.retry | :642 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:common.retry | :642 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:common.retry | :642 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:common.retry | :642 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:common.retry | :642 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:common.retry | :642 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:common.retry | :642 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.mark_ready | :663 | `PATCH /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoWrite` | rust+web+browser |
| × | :708 | `DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers/{username}` | `RepoWrite` | rust+browser |
| i18n:pulls.reviewers.request | :721 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoWrite` | rust+browser |
| i18n:pulls.reviewers.request | :721 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :736 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/ci-approval` | `RepoWrite` | web |
| i18n:pulls.fork_ci.approving | :736 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :736 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :736 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :736 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :736 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :736 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :736 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :768 | `DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/merge-queue` | `RepoWrite` | browser |
| i18n:pulls.merge.leave_queue | :768 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :768 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :768 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :768 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :768 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :768 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :768 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.merge.disable_auto | :781 | `DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/auto-merge` | `RepoWrite` | browser |
| i18n:pulls.merge.merging | :793 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/merge` | `RepoWrite` | rust+web |
| i18n:pulls.merge.merging | :793 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.merge.merging | :793 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.merge.merging | :793 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.merge.merging | :793 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.merge.merging | :793 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.merge.merging | :793 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.merge.merging | :793 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :796 | `PUT /api/v1/repos/{owner}/{name}/pulls/{number}/auto-merge` | `RepoWrite` | rust+browser |
| i18n:pulls.merge.enabling_auto | :796 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :796 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :796 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :796 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :796 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :796 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :796 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :799 | `PUT /api/v1/repos/{owner}/{name}/pulls/{number}/merge-queue` | `RepoWrite` | rust+browser |
| i18n:pulls.merge.joining_queue | :799 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :799 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :799 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :799 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :799 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :799 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :799 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :851 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/reviews/{id}/dismiss` | `RepoWrite` | rust+web |
| i18n:pulls.review.dismissing | :851 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :851 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :851 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :851 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :851 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :851 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :851 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :875 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/suggestions/apply` | `RepoWrite` | rust |
| i18n:pulls.suggestion.applying_selected | :875 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :875 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :875 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :875 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :875 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :875 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :875 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :917 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}/suggestion/apply` | `RepoWrite` | rust |
| i18n:pulls.suggestion.apply | :917 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :917 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :917 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :917 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :917 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :917 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :917 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.threads.reopen | :929 | `PATCH /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}/resolution` | `RepoWrite` | **—** |
| i18n:pulls.threads.reopen | :929 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :987 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}/suggestion/apply` | `RepoWrite` | rust |
| i18n:pulls.suggestion.apply | :987 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :987 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :987 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :987 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :987 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :987 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :987 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.threads.reopen | :997 | `PATCH /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}/resolution` | `RepoWrite` | browser |
| i18n:pulls.threads.reopen | :997 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.diff.submit_comment | :1017 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoAuthRead` | rust+browser |
| i18n:pulls.diff.submit_comment | :1017 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:common.retry | :1029 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:common.retry | :1029 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:common.retry | :1029 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:common.retry | :1029 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:common.retry | :1029 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:common.retry | :1029 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:common.retry | :1029 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.review.submit | :1056 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoAuthRead` | rust+browser |
| i18n:pulls.review.submit | :1056 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web+browser |
| i18n:pulls.review.submit | :1056 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web+browser |
| i18n:pulls.review.submit | :1056 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web+browser |
| i18n:pulls.review.submit | :1056 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web+browser |
| i18n:pulls.review.submit | :1056 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web+browser |
| i18n:pulls.review.submit | :1056 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web+browser |
| i18n:pulls.review.submit | :1056 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web+browser |
| i18n:common.saving | :1073 | `PATCH /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}` | `RepoAdmin` | web+browser |
| i18n:common.deleting | :1101 | `DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}` | `RepoAdmin` | web+browser |
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
| i18n:subscription.unsubscribe | `ThreadSubscription` | `DELETE /api/v1/repos/{owner}/{name}/issues/{number}/subscription` | `RepoAuthRead` | web |
| i18n:subscription.unsubscribe | `ThreadSubscription` | `DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/subscription` | `RepoAuthRead` | web |
| i18n:subscription.unsubscribe | `ThreadSubscription` | `PUT /api/v1/repos/{owner}/{name}/issues/{number}/subscription` | `RepoAuthRead` | web |
| i18n:subscription.unsubscribe | `ThreadSubscription` | `PUT /api/v1/repos/{owner}/{name}/pulls/{number}/subscription` | `RepoAuthRead` | web |
| upload | `AttachmentPanel` | `POST /api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets` | `RepoWrite` | rust |
| → item.browser_download_url | `AttachmentPanel` | `GET /api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets/{attachment_id}` | `RepoRead` | rust |
| i18n:attachments.delete | `AttachmentPanel` | `DELETE /api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets/{attachment_id}` | `RepoWrite` | rust |
| upload | `AttachmentPanel` | `POST /api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets` | `RepoWrite` | rust |
| → item.browser_download_url | `AttachmentPanel` | `GET /api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets/{attachment_id}` | `RepoRead` | rust |
| i18n:attachments.delete | `AttachmentPanel` | `DELETE /api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets/{attachment_id}` | `RepoWrite` | rust |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/assets` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/issues/{number}/subscription` | `RepoAuthRead` | web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/subscription` | `RepoAuthRead` | web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets` | `RepoRead` | rust+web |

### `/[owner]/[repo]/releases`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| (event) => handleAssetUpload(release.id, | :536 | `POST /api/v1/repos/{owner}/{name}/releases/{release_id}/assets` | `RepoWrite` | rust+web |
| · )} | :556 | `GET /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/download` | `RepoRead` | rust+web |
| i18n:releases.attestation.verifying | :598 | `POST /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation/verify` | `RepoRead` | rust+web |
| i18n:releases.attestation.signing | :609 | `POST /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation` | `RepoWrite` | rust+web |
| i18n:common.delete | :635 | `DELETE /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}` | `RepoWrite` | rust+web+browser |
| i18n:common.delete | :672 | `DELETE /api/v1/repos/{owner}/{name}/releases/{id}` | `RepoWrite` | rust+web |
| i18n:common.delete | :672 | `GET /api/v1/instance` | `Public` | rust+web+browser |
| i18n:common.delete | :672 | `GET /api/v1/repos/{owner}/{name}/releases` | `RepoRead` | rust+web |
| i18n:common.delete | :672 | `GET /api/v1/repos/{owner}/{name}/releases/{release_id}/assets` | `RepoRead` | rust+web |
| i18n:common.delete | :672 | `GET /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation` | `RepoRead` | rust+web |
| i18n:common.previous | :687 | `GET /api/v1/instance` | `Public` | rust+web |
| i18n:common.previous | :687 | `GET /api/v1/repos/{owner}/{name}/releases` | `RepoRead` | rust+web |
| i18n:common.previous | :687 | `GET /api/v1/repos/{owner}/{name}/releases/{release_id}/assets` | `RepoRead` | rust+web |
| i18n:common.previous | :687 | `GET /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation` | `RepoRead` | rust+web |
| i18n:common.next | :695 | `GET /api/v1/instance` | `Public` | rust+web |
| i18n:common.next | :695 | `GET /api/v1/repos/{owner}/{name}/releases` | `RepoRead` | rust+web |
| i18n:common.next | :695 | `GET /api/v1/repos/{owner}/{name}/releases/{release_id}/assets` | `RepoRead` | rust+web |
| i18n:common.next | :695 | `GET /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation` | `RepoRead` | rust+web |
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
| * | :205 | `PATCH /api/v1/repos/{owner}/{name}/releases/{id}` | `RepoWrite` | rust+web |
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
| * tagName = tag} > )} * selectedTargetType = 'tag'} > select | :155 | `POST /api/v1/repos/{owner}/{name}/releases` | `RepoWrite` | rust+web |
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
| i18n:settings.repository_info.description | :342 | `PATCH /api/v1/repos/{owner}/{name}` | `RepoAdmin` | web+browser |
| i18n:settings.transfer.confirm | :514 | `POST /api/v1/repos/{owner}/{name}/transfer` | `RepoOwner` | rust+web+browser |
| i18n:settings.delete.confirm_button | :528 | `DELETE /api/v1/repos/{owner}/{name}` | `RepoOwner` | rust+web |
| i18n:common.saving | :546 | `PATCH /api/v1/repos/{owner}/{name}` | `RepoAdmin` | web |
| i18n:settings.rename.renaming | :563 | `PATCH /api/v1/repos/{owner}/{name}` | `RepoAdmin` | web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | web |

### `/[owner]/[repo]/settings/branches`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| )} removeRequiredStatusCheck(index)} disabled= aria-label= ) | :254 | `PATCH /api/v1/repos/{owner}/{name}/branches/protection/{id}` | `RepoAdmin` | web+browser |
| )} removeRequiredStatusCheck(index)} disabled= aria-label= ) | :254 | `POST /api/v1/repos/{owner}/{name}/branches/protection` | `RepoAdmin` | rust+web+browser |
| )} removeRequiredStatusCheck(index)} disabled= aria-label= ) | :254 | `GET /api/v1/repos/{owner}/{name}/branches/protection` | `RepoRead` | rust+web+browser |
| i18n:common.delete | :403 | `DELETE /api/v1/repos/{owner}/{name}/branches/protection/{id}` | `RepoAdmin` | web+browser |
| i18n:common.delete | :403 | `GET /api/v1/repos/{owner}/{name}/branches/protection` | `RepoRead` | rust+web |

### `/[owner]/[repo]/settings/ci-secrets`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.ci_secrets.name | :101 | `PUT /api/v1/repos/{owner}/{name}/actions/secrets/{secret_name}` | `RepoAdmin` | rust+web+browser |
| i18n:settings.ci_secrets.name | :101 | `GET /api/v1/repos/{owner}/{name}/actions/secrets` | `RepoAdmin` | rust+web+browser |
| i18n:common.delete | :101 | `DELETE /api/v1/repos/{owner}/{name}/actions/secrets/{secret_name}` | `RepoAdmin` | rust+web+browser |
| i18n:common.delete | :101 | `GET /api/v1/repos/{owner}/{name}/actions/secrets` | `RepoAdmin` | rust+web |

### `/[owner]/[repo]/settings/collaborators`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.collaborators.user_identifier | :222 | `POST /api/v1/repos/{owner}/{name}/collaborators` | `RepoAdmin` | rust+web+smoke+browser |
| i18n:settings.collaborators.user_identifier | :222 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust+web+browser |
| i18n:common.save | :285 | `PATCH /api/v1/repos/{owner}/{name}/collaborators/{id}` | `RepoAdmin` | rust+web+browser |
| i18n:common.save | :285 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust+web |
| i18n:common.delete | :293 | `DELETE /api/v1/repos/{owner}/{name}/collaborators/{id}` | `RepoAdmin` | rust+web+browser |
| i18n:common.delete | :293 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust+web |

### `/[owner]/[repo]/settings/deploy-keys`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.deploy_keys.name | :174 | `POST /api/v1/repos/{owner}/{name}/keys` | `RepoAdmin` | rust+web+browser |
| i18n:settings.deploy_keys.name | :174 | `GET /api/v1/repos/{owner}/{name}/keys` | `RepoAdmin` | rust+web+browser |
| i18n:common.delete | :199 | `DELETE /api/v1/repos/{owner}/{name}/keys/{id}` | `RepoAdmin` | rust+browser |
| i18n:common.delete | :199 | `GET /api/v1/repos/{owner}/{name}/keys` | `RepoAdmin` | rust+web |

### `/[owner]/[repo]/settings/environments`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.environments.name | :110 | `POST /api/v1/repos/{owner}/{name}/actions/environments` | `RepoAdmin` | rust+web+browser |
| i18n:settings.environments.name | :110 | `PUT /api/v1/repos/{owner}/{name}/actions/environments/{id}` | `RepoAdmin` | web+browser |
| i18n:settings.environments.name | :110 | `GET /api/v1/repos/{owner}/{name}/actions/environments` | `RepoRead` | rust+web+browser |
| i18n:common.delete | :117 | `DELETE /api/v1/repos/{owner}/{name}/actions/environments/{id}` | `RepoAdmin` | rust+web+browser |
| i18n:common.delete | :117 | `GET /api/v1/repos/{owner}/{name}/actions/environments` | `RepoRead` | rust+web |

### `/[owner]/[repo]/settings/labels`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.saving | :331 | `PATCH /api/v1/repos/{owner}/{name}/labels/{id}` | `RepoWrite` | rust+web+browser |
| i18n:common.saving | :331 | `POST /api/v1/repos/{owner}/{name}/labels` | `RepoWrite` | rust+web+browser |
| i18n:common.saving | :331 | `GET /api/v1/repos/{owner}/{name}/labels` | `RepoRead` | rust+web+browser |
| i18n:common.deleting | :351 | `DELETE /api/v1/repos/{owner}/{name}/labels/{id}` | `RepoWrite` | rust+web+browser |
| i18n:common.deleting | :351 | `GET /api/v1/repos/{owner}/{name}/labels` | `RepoRead` | rust+web |

### `/[owner]/[repo]/settings/lfs-locks`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.lfs_locks.force_unlock | :129 | `POST /api/v1/repos/{owner}/{name}/lfs/locks/{id}/unlock` | `Foreign:api/lfs_locks.rs` | rust+web |
| i18n:settings.lfs_locks.more | :142 | `GET /api/v1/repos/{owner}/{name}/lfs/locks` | `Foreign:api/lfs_locks.rs` | rust+web+smoke |

### `/[owner]/[repo]/settings/lfs-storage`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.lfs_storage.scanning | :187 | `GET /api/v1/repos/{owner}/{name}/lfs/orphans` | `RepoAdmin` | web+browser |
| i18n:settings.lfs_storage.remove_selected | :213 | `POST /api/v1/repos/{owner}/{name}/lfs/orphans/prune` | `RepoAdmin` | web |
| i18n:settings.lfs_storage.remove_selected | :213 | `GET /api/v1/repos/{owner}/{name}/lfs/usage` | `RepoAdmin` | web |
| i18n:settings.lfs_storage.remove_selected | :213 | `GET /api/v1/repos/{owner}/{name}/lfs/objects` | `RepoAdmin` | web |
| i18n:settings.lfs_storage.remove | :250 | `POST /api/v1/repos/{owner}/{name}/lfs/orphans/prune` | `RepoAdmin` | web+browser |
| i18n:settings.lfs_storage.remove | :250 | `GET /api/v1/repos/{owner}/{name}/lfs/usage` | `RepoAdmin` | web+browser |
| i18n:settings.lfs_storage.remove | :250 | `GET /api/v1/repos/{owner}/{name}/lfs/objects` | `RepoAdmin` | web+browser |
| i18n:settings.lfs_storage.more | :262 | `GET /api/v1/repos/{owner}/{name}/lfs/usage` | `RepoAdmin` | web |
| i18n:settings.lfs_storage.more | :262 | `GET /api/v1/repos/{owner}/{name}/lfs/objects` | `RepoAdmin` | web |

### `/[owner]/[repo]/settings/mirror`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.mirror.url | :211 | `PATCH /api/v1/repos/{owner}/{name}/mirror` | `RepoWrite` | rust+web |
| i18n:settings.mirror.url | :211 | `POST /api/v1/repos/{owner}/{name}/mirror` | `RepoWrite` | rust+web |
| i18n:common.loading | :252 | `POST /api/v1/repos/{owner}/{name}/mirror/sync` | `RepoWrite` | rust |
| i18n:common.loading | :252 | `GET /api/v1/repos/{owner}/{name}/mirror` | `RepoWrite` | rust+web |
| i18n:common.loading | :255 | `DELETE /api/v1/repos/{owner}/{name}/mirror` | `RepoWrite` | rust+web |

### `/[owner]/[repo]/settings/retention`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.retention.artifact_days | :126 | `PUT /api/v1/repos/{owner}/{name}/actions/retention` | `RepoAdmin` | web+browser |
| i18n:settings.retention.cleanup | :133 | `DELETE /api/v1/repos/{owner}/{name}/actions/retention/expired` | `RepoAdmin` | rust+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/actions/retention` | `RepoAdmin` | rust+web+browser |

### `/[owner]/[repo]/settings/tags`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| ) as part} | :122 | `POST /api/v1/repos/{owner}/{name}/tags/protection` | `RepoAdmin` | rust+web+browser |
| ) as part} | :122 | `PATCH /api/v1/repos/{owner}/{name}/tags/protection/{id}` | `RepoAdmin` | rust+web+browser |
| ) as part} | :122 | `GET /api/v1/repos/{owner}/{name}/tags/protection` | `RepoRead` | rust+web+browser |
| i18n:common.delete | :129 | `DELETE /api/v1/repos/{owner}/{name}/tags/protection/{id}` | `RepoAdmin` | rust+web+browser |
| i18n:common.delete | :129 | `GET /api/v1/repos/{owner}/{name}/tags/protection` | `RepoRead` | rust+web |

### `/[owner]/[repo]/settings/webhooks`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| application/json application/x-www-form-urlencoded toggleEve | :387 | `POST /api/v1/repos/{owner}/{name}/hooks` | `RepoAdmin` | rust+web+browser |
| application/json application/x-www-form-urlencoded toggleEve | :387 | `GET /api/v1/repos/{owner}/{name}/hooks` | `RepoAdmin` | rust+web+browser |
| (e) => setActive(hook, e.currentTarget.c | :455 | `PATCH /api/v1/repos/{owner}/{name}/hooks/{id}` | `RepoAdmin` | rust+web+browser |
| (e) => setActive(hook, e.currentTarget.c | :455 | `GET /api/v1/repos/{owner}/{name}/hooks` | `RepoAdmin` | rust+web |
| i18n:settings.webhooks.hide_deliveries | :464 | `GET /api/v1/repos/{owner}/{name}/hooks/{id}` | `RepoAdmin` | rust+web+browser |
| i18n:settings.webhooks.hide_deliveries | :464 | `GET /api/v1/repos/{owner}/{name}/hooks/{id}/deliveries` | `RepoAdmin` | rust+web+browser |
| i18n:common.loading | :475 | `DELETE /api/v1/repos/{owner}/{name}/hooks/{id}` | `RepoAdmin` | rust+web+browser |
| i18n:common.loading | :475 | `GET /api/v1/repos/{owner}/{name}/hooks` | `RepoAdmin` | rust+web |
| i18n:common.loading | :493 | `GET /api/v1/repos/{owner}/{name}/hooks/{id}/deliveries` | `RepoAdmin` | rust+web |
| i18n:settings.webhooks.redelivering | :528 | `POST /api/v1/repos/{owner}/{name}/hooks/{id}/deliveries/{delivery_id}/redeliver` | `RepoAdmin` | rust+web+browser |
| i18n:settings.webhooks.redelivering | :528 | `GET /api/v1/repos/{owner}/{name}/hooks/{id}/deliveries` | `RepoAdmin` | rust+web |

### `/[owner]/[repo]/tags`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:repo.tags.deleting | :169 | `DELETE /api/v1/repos/{owner}/{name}/tags/{tag}` | `RepoWrite` | web |
| i18n:repo.tags.deleting | :169 | `GET /api/v1/repos/{owner}/{name}/tags` | `RepoRead` | web |
| i18n:repo.tags.deleting | :169 | `GET /api/v1/repos/{owner}/{name}/tags/protection` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/time_tracking`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| # | :309 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time` | `RepoRead` | rust+web+browser |
| # | :309 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time/total` | `RepoRead` | rust+web+browser |
| i18n:common.retry | :339 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time/total` | `RepoRead` | rust+web |
| i18n:common.add | :369 | `POST /api/v1/repos/{owner}/{name}/issues/{number}/time` | `RepoWrite` | rust+web+browser |
| i18n:common.add | :369 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time` | `RepoRead` | rust+web |
| i18n:common.add | :369 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time/total` | `RepoRead` | rust+web |
| i18n:common.delete | :399 | `DELETE /api/v1/repos/{owner}/{name}/issues/{number}/time/{id}` | `RepoWrite` | rust+web+browser |
| i18n:common.delete | :399 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time` | `RepoRead` | rust+web |
| i18n:common.delete | :399 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time/total` | `RepoRead` | rust+web |
| i18n:common.previous | :408 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time` | `RepoRead` | rust+web |
| i18n:common.next | :411 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time` | `RepoRead` | rust+web |
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
| showCreate = false}> | :113 | `POST /api/v1/repos/{owner}/{name}/wiki` | `RepoWrite` | rust+web+browser |
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
| i18n:wiki.history | :321 | `GET /api/v1/repos/{owner}/{name}/wiki/{title}/history` | `RepoRead` | rust+web+browser |
| i18n:wiki.delete | :324 | `DELETE /api/v1/repos/{owner}/{name}/wiki/{title}` | `RepoWrite` | rust+browser |
| v | :340 | `GET /api/v1/repos/{owner}/{name}/wiki/{title}/revisions/{rev_id}` | `RepoRead` | rust+web+browser |
| i18n:wiki.restore_version | :350 | `PATCH /api/v1/repos/{owner}/{name}/wiki/{title}` | `RepoWrite` | rust |
| i18n:wiki.restore_version | :350 | `GET /api/v1/repos/{owner}/{name}/wiki/{title}` | `RepoRead` | rust+web |
| i18n:wiki.restore_version | :350 | `GET /api/v1/repos/{owner}/{name}/wiki` | `RepoRead` | rust+web |
| i18n:wiki.save | :365 | `PATCH /api/v1/repos/{owner}/{name}/wiki/{title}` | `RepoWrite` | rust+browser |
| i18n:wiki.save | :365 | `GET /api/v1/repos/{owner}/{name}/wiki/{title}` | `RepoRead` | rust+web+browser |
| i18n:wiki.save | :365 | `GET /api/v1/repos/{owner}/{name}/wiki` | `RepoRead` | rust+web |
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
| i18n:common.view | :170 | `GET /api/v1/repos/{owner}/{name}/wiki/{title}/revisions/{rev_id}` | `RepoRead` | rust+web |
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
| )} | :178 | `GET /api/v1/admin/audit/logs` | `InstanceAdmin` | rust+web+smoke |
| i18n:admin.audit.clear_filters | :185 | `GET /api/v1/admin/audit/logs` | `InstanceAdmin` | rust+web+smoke |
| i18n:admin.audit.fields.details | :241 | `GET /api/v1/admin/audit/logs/{id}` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:common.prev_arrow | :254 | `GET /api/v1/admin/audit/logs` | `InstanceAdmin` | rust+web+smoke |
| i18n:common.next_arrow | :256 | `GET /api/v1/admin/audit/logs` | `InstanceAdmin` | rust+web+smoke |

### `/admin/orgs` 🔒

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.prev_arrow | :157 | `GET /api/v1/admin/orgs` | `InstanceAdmin` | rust+web |
| i18n:common.next_arrow | :159 | `GET /api/v1/admin/orgs` | `InstanceAdmin` | rust+web |
| i18n:common.loading | :177 | `DELETE /api/v1/admin/orgs/{name}` | `InstanceAdmin` | rust+smoke+browser |
| i18n:common.loading | :177 | `GET /api/v1/admin/orgs` | `InstanceAdmin` | rust+web+browser |

### `/admin/runners` 🔒

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.loading | :239 | `POST /api/v1/runners/register` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:common.loading | :239 | `GET /api/v1/admin/runners` | `InstanceAdmin` | web+smoke+browser |
| i18n:common.previous | :295 | `GET /api/v1/admin/runners` | `InstanceAdmin` | web+smoke |
| i18n:common.next | :297 | `GET /api/v1/admin/runners` | `InstanceAdmin` | web+smoke |
| i18n:common.loading | :309 | `DELETE /api/v1/admin/runners/{id}` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:common.loading | :309 | `GET /api/v1/admin/runners` | `InstanceAdmin` | web+smoke+browser |

### `/admin/settings` 🔒

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.saving | :459 | `PATCH /api/v1/admin/settings` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:admin.settings.sso.testing | :500 | `POST /api/v1/admin/sso/providers/{id}/test` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:admin.settings.sso.disable | :504 | `PATCH /api/v1/admin/sso/providers/{id}` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:admin.settings.sso.disable | :504 | `GET /api/v1/admin/sso/providers` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:common.delete | :508 | `DELETE /api/v1/admin/sso/providers/{id}` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:common.delete | :508 | `GET /api/v1/admin/sso/providers` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:common.saving | :621 | `PATCH /api/v1/admin/sso/providers/{id}` | `InstanceAdmin` | rust+web+smoke |
| i18n:common.saving | :621 | `POST /api/v1/admin/sso/providers` | `InstanceAdmin` | rust+smoke+browser |
| i18n:common.saving | :621 | `GET /api/v1/admin/sso/providers` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:common.loading | :637 | `GET /api/v1/admin/login-attempts` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:admin.settings.login.apply | :654 | `GET /api/v1/admin/login-attempts` | `InstanceAdmin` | rust+web+smoke |
| i18n:common.previous | :673 | `GET /api/v1/admin/login-attempts` | `InstanceAdmin` | rust+web+smoke |
| i18n:common.next | :675 | `GET /api/v1/admin/login-attempts` | `InstanceAdmin` | rust+web+smoke |
| _(загрузка страницы)_ | — | `GET /api/v1/admin/settings` | `InstanceAdmin` | rust+web+smoke+browser |

### `/admin/users` 🔒

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| (showCreate = false)}> | :262 | `POST /api/v1/admin/users` | `InstanceAdmin` | rust+web+smoke+browser |
| (showCreate = false)}> | :262 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+web+browser |
| i18n:admin.users.working | :323 | `POST /api/v1/admin/users/{id}/unlock` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:admin.users.working | :323 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+web+browser |
| i18n:admin.users.reset_password | :329 | `POST /api/v1/admin/users/{id}/password-reset` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:common.prev_arrow | :344 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+web |
| i18n:common.next_arrow | :346 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+web |
| i18n:common.loading | :384 | `PATCH /api/v1/admin/users/{id}` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:common.loading | :384 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+web+browser |
| i18n:common.loading | :405 | `DELETE /api/v1/admin/users/{id}` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:common.loading | :405 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+web+browser |

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
| i18n:auth.forgot_password.email | :56 | `POST /api/v1/users/forgot-password` | `Public` | rust |

### `/imports`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:imports.refresh | :215 | `GET /api/v1/imports` | `User` | rust+web |
| GitHub GitLab Gitea Git | :227 | `POST /api/v1/imports` | `User` | rust+web |
| GitHub GitLab Gitea Git | :227 | `GET /api/v1/imports` | `User` | rust+web |
| i18n:imports.deleting | :318 | `DELETE /api/v1/imports/{id}` | `User` | rust+web |

### `/login`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:auth.login.sso_retry | :259 | `GET /api/v1/auth/sso/providers` | `Public` | rust+web |

### `/notifications`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| load | :168 | `GET /api/v1/notifications` | `User` | rust+web |
| load | :168 | `GET /api/v1/notifications/unread-count` | `User` | rust+web |
| i18n:notifications.mark_all_read | :172 | `POST /api/v1/notifications/mark-all-read` | `User` | rust+web |
| i18n:notifications.mark_all_read | :172 | `GET /api/v1/notifications` | `User` | rust+web |
| i18n:notifications.mark_all_read | :172 | `GET /api/v1/notifications/unread-count` | `User` | rust+web |
| → internalLink(notif) | :198 | `POST /api/v1/notifications/{id}/read` | `User` | rust+web |
| i18n:notifications.mark_read | :214 | `POST /api/v1/notifications/{id}/read` | `User` | rust+web |
| i18n:notifications.mark_read | :214 | `GET /api/v1/notifications` | `User` | rust+web |
| i18n:notifications.mark_read | :214 | `GET /api/v1/notifications/unread-count` | `User` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/ws/notifications` | `Foreign:ws.rs` | rust+web |

### `/orgs`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| * | :119 | `GET /api/v1/orgs` | `User` | rust+web+smoke |

### `/orgs/[name]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.loading | :546 | `DELETE /api/v1/orgs/{name}` | `OrgAdmin` | rust+web+smoke+browser |
| editingOrg = false} disabled= > | :554 | `PATCH /api/v1/orgs/{name}` | `OrgAdmin` | rust+web+smoke+browser |
| i18n:common.loading | :603 | `POST /api/v1/repos` | `User` | rust+web+smoke |
| i18n:common.loading | :603 | `GET /api/v1/repos/{owner}` | `PublicFiltered` | rust+web |
| i18n:orgs.new_team | :639 | `POST /api/v1/orgs/{name}/teams` | `OrgAdmin` | rust+browser |
| i18n:orgs.new_team | :639 | `GET /api/v1/orgs/{name}/teams` | `OrgRead` | rust+web |
| i18n:orgs.hide_team_members | :664 | `GET /api/v1/orgs/{name}/teams/{team_id}/members` | `OrgRead` | rust+web |
| ` ? t('common.loading') : t('common.delete')} | :668 | `DELETE /api/v1/orgs/{name}/teams/{team_id}` | `OrgAdmin` | rust+web+smoke+browser |
| ` ? t('common.loading') : t('common.delete')} | :668 | `GET /api/v1/orgs/{name}/teams` | `OrgRead` | rust+web |
| ` ? t('common.loading') : t('common.add')} | :679 | `POST /api/v1/orgs/{name}/teams/{team_id}/members` | `OrgAdmin` | rust+web+browser |
| ` ? t('common.loading') : t('common.add')} | :679 | `GET /api/v1/orgs/{name}/teams/{team_id}/members` | `OrgRead` | rust+web |
| -$ ` ? t('common.loading') : t('common.delete')} | :707 | `DELETE /api/v1/orgs/{name}/teams/{team_id}/members/{user_id}` | `OrgAdmin` | rust+web+browser |
| -$ ` ? t('common.loading') : t('common.delete')} | :707 | `GET /api/v1/orgs/{name}/teams/{team_id}/members` | `OrgRead` | rust+web |
| i18n:orgs.member_placeholder | :732 | `POST /api/v1/orgs/{name}/members` | `OrgAdmin` | rust+web+smoke+browser |
| i18n:orgs.member_placeholder | :732 | `GET /api/v1/orgs/{name}/members` | `OrgRead` | rust+web |
| ` ? t('common.loading') : t('common.delete')} | :758 | `DELETE /api/v1/orgs/{name}/members/{user_id}` | `OrgAdmin` | rust+web+smoke+browser |
| ` ? t('common.loading') : t('common.delete')} | :758 | `GET /api/v1/orgs/{name}/members` | `OrgRead` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/orgs/{name}` | `OrgRead` | rust+web+smoke |

### `/reset-password`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:auth.reset_password.new_password | :107 | `POST /api/v1/users/reset-password` | `Public` | rust |

### `/search`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| _(загрузка страницы)_ | — | `GET /api/v1/search` | `PublicFiltered` | rust+web |

### `/settings/agents`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:agents.username | :260 | `POST /api/v1/users/bots` | `User` | rust+web |
| i18n:agents.username | :260 | `GET /api/v1/users/bots` | `User` | rust+web |
| i18n:common.close | :294 | `GET /api/v1/users/bots/{bot}/tokens` | `User` | rust+web |
| i18n:agents.deleting | :297 | `DELETE /api/v1/users/bots/{bot}` | `User` | rust+web |
| i18n:agents.deleting | :297 | `GET /api/v1/users/bots` | `User` | rust+web |
| i18n:agents.token_name | :321 | `POST /api/v1/users/bots/{bot}/tokens` | `User` | rust+web |
| i18n:agents.token_name | :321 | `GET /api/v1/users/bots/{bot}/tokens` | `User` | rust+web |
| i18n:agents.revoking | :375 | `DELETE /api/v1/users/bots/{bot}/tokens/{id}` | `User` | web |
| i18n:agents.revoking | :375 | `GET /api/v1/users/bots/{bot}/tokens` | `User` | rust+web |

### `/settings/notifications`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| (event) => toggle(key, event) | :95 | `PUT /api/v1/users/me/notification-settings` | `User` | rust+web |
| _(загрузка страницы)_ | — | `GET /api/v1/users/me/notification-settings` | `User` | web |

### `/settings/profile`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.profile.retry | :232 | `GET /api/v1/users/me` | `User` | rust+web+smoke |
| i18n:settings.profile.retry | :232 | `GET /api/v1/instance` | `Public` | rust+web |
| i18n:settings.profile.display_name | :239 | `PATCH /api/v1/users/me` | `User` | rust+web |
| uploadAvatar | :264 | `PUT /api/v1/users/me/avatar` | `User` | rust+web |
| i18n:settings.profile.remove_avatar | :273 | `DELETE /api/v1/users/me/avatar` | `User` | rust+web |
| )} | :285 | `PUT /api/v1/users/me/password` | `User` | rust+web |
| i18n:settings.profile.confirm_current_email | :323 | `POST /api/v1/users/me/email/verify` | `User` | rust+web |
| i18n:settings.profile.new_email | :337 | `POST /api/v1/users/me/email` | `User` | rust+web |
| i18n:settings.profile.delete_password | :372 | `DELETE /api/v1/users/me` | `User` | rust+web |

### `/settings/security`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:account_security.mfa.retry | :401 | `GET /api/v1/users/mfa/backup` | `User` | rust+web |
| i18n:account_security.mfa.current_password | :416 | `POST /api/v1/users/mfa/backup/regenerate` | `User` | rust |
| i18n:account_security.mfa.current_password | :416 | `GET /api/v1/users/mfa/backup` | `User` | rust+web |
| i18n:account_security.mfa.current_password | :416 | `GET /api/v1/users/passkeys` | `User` | rust+web |
| i18n:account_security.mfa.current_password | :416 | `GET /api/v1/users/me/sso` | `User` | rust+web |
| i18n:account_security.mfa.current_password | :416 | `GET /api/v1/auth/sso/providers` | `Public` | rust+web |
| i18n:account_security.mfa.current_password | :434 | `POST /api/v1/users/mfa/disable` | `User` | rust |
| i18n:account_security.mfa.current_password | :434 | `GET /api/v1/users/mfa/backup` | `User` | rust+web |
| i18n:account_security.mfa.current_password | :434 | `GET /api/v1/users/passkeys` | `User` | rust+web |
| i18n:account_security.mfa.current_password | :434 | `GET /api/v1/users/me/sso` | `User` | rust+web |
| i18n:account_security.mfa.current_password | :434 | `GET /api/v1/auth/sso/providers` | `Public` | rust+web |
| i18n:account_security.mfa.starting | :445 | `POST /api/v1/users/mfa/setup` | `User` | rust+web |
| i18n:account_security.passkeys.retry | :475 | `GET /api/v1/users/passkeys` | `User` | rust+web |
| i18n:account_security.passkeys.remove | :489 | `DELETE /api/v1/users/passkeys/{id}` | `User` | rust |
| i18n:account_security.sso.retry | :542 | `GET /api/v1/users/me/sso` | `User` | rust+web |
| i18n:account_security.sso.unlink | :557 | `DELETE /api/v1/auth/sso/{slug}/unlink` | `User` | rust+web |
| )} | :575 | `POST /api/v1/auth/sso/{slug}/link` | `User` | rust+web |
| i18n:account_security.setup.code | :598 | `POST /api/v1/users/mfa/enable` | `User` | rust |
| i18n:account_security.setup.code | :598 | `GET /api/v1/users/mfa/backup` | `User` | rust+web |
| i18n:account_security.setup.code | :598 | `GET /api/v1/users/passkeys` | `User` | rust+web |
| i18n:account_security.setup.code | :598 | `GET /api/v1/users/me/sso` | `User` | rust+web |
| i18n:account_security.setup.code | :598 | `GET /api/v1/auth/sso/providers` | `Public` | rust+web |

### `/settings/ssh-keys`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:ssh_keys.name | :130 | `POST /api/v1/users/ssh-keys` | `User` | rust+web+smoke |
| i18n:ssh_keys.name | :130 | `GET /api/v1/users/ssh-keys` | `User` | rust+web |
| i18n:ssh_keys.deleting | :182 | `DELETE /api/v1/users/ssh-keys/{id}` | `User` | rust |
| i18n:ssh_keys.deleting | :182 | `GET /api/v1/users/ssh-keys` | `User` | rust+web |

### `/settings/tokens`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:access_tokens.name | :191 | `POST /api/v1/users/tokens` | `User` | rust+web |
| i18n:access_tokens.name | :191 | `GET /api/v1/users/tokens` | `User` | rust+web |
| i18n:access_tokens.revoking | :242 | `DELETE /api/v1/users/tokens/{id}` | `User` | rust |
| i18n:access_tokens.revoking | :242 | `GET /api/v1/users/tokens` | `User` | rust+web |

### `/verify-email`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:auth.verify_email.working | :67 | `POST /api/v1/users/verify-email` | `Public` | rust+web |

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
| GET | `/livez` | `Public` | rust |
| GET | `/readyz` | `Public` | rust |
| GET | `/metrics` | `Public` | rust+smoke |
| POST | `/api/v1/users/register` | `Public` | rust+web+smoke |
| POST | `/api/v1/users/login` | `Public` | rust+web+smoke |
| POST | `/api/v1/users/logout` | `User` | rust+web |
| POST | `/api/v1/users/password/initial` | `Public` | rust+web |
| GET | `/api/v1/avatars/{username}` | `Public` | rust |
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
| DELETE | `/api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoAdmin` | **—** |
| GET | `/api/v1/repos/{owner}/{name}/pulls/{number}/reviews/{id}` | `RepoRead` | rust |
| POST | `/api/v1/repos/{owner}/{name}/lfs/objects/batch` | `Foreign:api/lfs.rs` | rust |
| GET | `/api/v1/repos/{owner}/{name}/lfs/objects/{oid}` | `Foreign:api/lfs.rs` | rust+smoke |
| PUT | `/api/v1/repos/{owner}/{name}/lfs/objects/{oid}` | `Foreign:api/lfs.rs` | rust |
| POST | `/api/v1/repos/{owner}/{name}/lfs/locks` | `Foreign:api/lfs_locks.rs` | rust |
| POST | `/api/v1/repos/{owner}/{name}/lfs/locks/verify` | `Foreign:api/lfs_locks.rs` | **—** |
| GET | `/api/v1/ci/oidc/.well-known/openid-configuration` | `Public` | rust |
| GET | `/api/v1/ci/oidc/jwks` | `Public` | rust |
| GET | `/api/v1/ci/oidc/token` | `Foreign:api/ci_oidc.rs` | rust |
| GET | `/api/v1/repos/{owner}/{name}/branches/protection/{id}` | `RepoRead` | rust |
| DELETE | `/api/v1/repos/{owner}/{name}/branches/protection` | `RepoWrite` | **—** |
| DELETE | `/api/v1/repos/{owner}/{name}/tags/protection` | `RepoWrite` | **—** |
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
| GET | `/api/v1/runners/{id}/jobs/{job_id}/status` | `Foreign:RUNNER_AUTH_LAYER` | rust |
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

