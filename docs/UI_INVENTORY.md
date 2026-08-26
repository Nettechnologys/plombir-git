# ForgeKeep — UI Inventory (generated)

> **Сгенерировано.** Не править руками — перегенерировать:
> `node scripts/ui-inventory.mjs`
>
> Выводится из исходников: роутер (`crates/rg-http/src/routes.rs`), клиент
> (`web/src/lib/api`), страницы (`web/src/routes`) и общие компоненты
> (`web/src/lib/components`). Ручной близнец — `docs/FEATURE_INVENTORY.md`.
>
> **Что значит «покрыт».** `rust` / `web` / `smoke` пока отвечают на слабый
> вопрос — *называет ли исполняемый тестовый код метод и полный URL роута*.
> Комментарии и тот же URL под другим HTTP-методом coverage не создают. `browser` сильнее:
> manifest называет ровно один живой control/passive call, а runtime проводит
> его через owner + outsider и сверяет фактический статус с `Access`.
> Текстовое упоминание само по себе всё ещё НЕ означает полезного теста.

## Сводка

| | |
|---|---|
| Роутов в роутере (с объявленным `Access`) | 356 |
| Из них достижимы из браузера | 213 (60%) |
| Страниц | 61 |
| Интерактивных элементов | 788 |
| — из них дёргают API | 252 |
| — приходят из общих компонентов | 296 |
| Browser sweep: сценариев / записей инвентаря / роутов | 39 / 157 / 149 |
| **UI-роутов без единого web/smoke/browser-теста** | **52** |
| UI-роутов без corpus-hit и browser-сценария | 7 |

## По уровню доступа

| `Access` | роутов | достижимы из UI | нет фронт-теста | нет corpus/browser coverage |
|---|---:|---:|---:|---:|
| `RepoRead` | 104 | 54 | 3 | 6 |
| `RepoWrite` | 80 | 53 | 16 | 17 |
| `User` | 35 | 24 | 19 | 3 |
| `RepoAdmin` | 28 | 28 | 0 | 0 |
| `InstanceAdmin` | 23 | 19 | 0 | 0 |
| `Public` | 20 | 8 | 7 | 4 |
| `Foreign:oci.rs` | 13 | 0 | 0 | 3 |
| `RepoAuthRead` | 12 | 10 | 1 | 0 |
| `Foreign:RUNNER_AUTH_LAYER` | 11 | 0 | 0 | 2 |
| `OrgAdmin` | 8 | 8 | 0 | 0 |
| `Foreign:git_http.rs` | 6 | 0 | 0 | 4 |
| `OrgRead` | 5 | 4 | 3 | 0 |
| `PublicFiltered` | 3 | 3 | 1 | 0 |
| `Foreign:api/lfs.rs` | 3 | 0 | 0 | 0 |
| `RepoOwner` | 2 | 2 | 2 | 0 |
| `Foreign:ws.rs` | 2 | 0 | 0 | 0 |
| `Foreign:api/ci_oidc.rs` | 1 | 0 | 0 | 0 |

## Страницы

| Страница | элементов | дёргают API | из компонентов |
|---|---:|---:|---:|
| `/[owner]/[repo]/pulls/[number]` | 53 | 23 | 20 |
| `/[owner]/[repo]/boards` | 40 | 13 | 11 |
| `/[owner]/[repo]/releases` | 27 | 9 | 11 |
| `/[owner]/[repo]/issues/board` | 26 | 9 | 11 |
| `/[owner]/[repo]/issues` | 24 | 8 | 11 |
| `/[owner]/[repo]/issues/[number]` | 24 | 8 | 17 |
| `/orgs/[name]` | 24 | 10 | 0 |
| `/[owner]/[repo]/pipelines` | 23 | 9 | 11 |
| `/[owner]/[repo]/pulls` | 23 | 8 | 11 |
| `/[owner]/[repo]/wiki/[title]` | 22 | 8 | 11 |
| `/[owner]/[repo]/releases/new` | 21 | 4 | 11 |
| `/[owner]/[repo]/blob/[...path]` | 20 | 4 | 11 |
| `/[owner]/[repo]/milestones` | 20 | 7 | 11 |
| `/[owner]/[repo]` | 19 | 3 | 12 |
| `/[owner]/[repo]/network` | 19 | 5 | 11 |
| `/[owner]/[repo]/releases/edit/[id]` | 18 | 4 | 11 |
| `/[owner]/[repo]/packages` | 17 | 7 | 11 |
| `/[owner]/[repo]/packages/[format]/[...name]` | 17 | 5 | 11 |
| `/[owner]/[repo]/packages/upload` | 17 | 4 | 11 |
| `/[owner]/[repo]/time_tracking` | 17 | 8 | 11 |
| `/[owner]/[repo]/wiki` | 17 | 4 | 11 |
| `/admin/settings` 🔒 | 17 | 9 | 0 |
| `/[owner]/[repo]/wiki/[title]/history` | 15 | 4 | 11 |
| `/imports` | 14 | 3 | 0 |
| `/[owner]/[repo]/commits` | 13 | 3 | 11 |
| `/[owner]/[repo]/commits/[sha]` | 13 | 4 | 11 |
| `/[owner]/[repo]/packages/[format]` | 13 | 3 | 11 |
| `/admin/users` 🔒 | 13 | 5 | 0 |
| `/dashboard` | 12 | 1 | 0 |
| `/settings/security` | 12 | 6 | 0 |
| `/` | 11 | 1 | 0 |
| `/[owner]/[repo]/settings/webhooks` | 11 | 6 | 0 |
| `/[owner]/[repo]/settings/branches` | 10 | 2 | 0 |
| `/login` | 9 | 0 | 0 |
| `/[owner]/[repo]/edit/[...path]` | 8 | 0 | 8 |
| `/[owner]/[repo]/new` | 8 | 0 | 8 |
| `/[owner]/[repo]/settings/labels` | 8 | 2 | 0 |
| `/admin/audit` 🔒 | 8 | 6 | 0 |
| `/admin/runners` 🔒 | 8 | 4 | 0 |
| `/orgs` | 8 | 1 | 0 |
| `/search` | 8 | 0 | 0 |
| `/admin/orgs` 🔒 | 7 | 3 | 0 |
| `/[owner]/[repo]/settings/collaborators` | 6 | 3 | 0 |
| `/[owner]/[repo]/settings/environments` | 6 | 2 | 0 |
| `/[owner]/[repo]/settings/deploy-keys` | 5 | 2 | 0 |
| `/[owner]/[repo]/settings/mirror` | 5 | 3 | 0 |
| `/[owner]/[repo]/settings/tags` | 5 | 2 | 0 |
| `/admin` 🔒 | 5 | 0 | 0 |
| `/reset-password` | 5 | 1 | 0 |
| `/[owner]` | 4 | 0 | 0 |
| `/help` | 4 | 0 | 0 |
| `/settings/ssh-keys` | 4 | 2 | 0 |
| `/settings/tokens` | 4 | 2 | 0 |
| `/[owner]/[repo]/settings/ci-secrets` | 3 | 2 | 0 |
| `/[owner]/[repo]/settings/retention` | 3 | 2 | 0 |
| `/explore` | 3 | 2 | 0 |
| `/forgot-password` | 3 | 1 | 0 |
| `/notifications` | 3 | 3 | 0 |
| `/register` | 3 | 0 | 0 |
| `/[owner]/[repo]/settings` | 2 | 2 | 0 |
| `/[owner]/[repo]/settings/runners` | 1 | 0 | 0 |

## План тестов: элемент → роут → уровень доступа

Каждая строка — один сценарий e2e. `Access` говорит, какая персона обязана
пройти и какая обязана получить отказ.

### `/`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Retry | :186 | `GET /api/v1/repos/explore` | `PublicFiltered` | rust+smoke |

### `/[owner]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}` | `PublicFiltered` | rust+smoke |
| _(загрузка страницы)_ | — | `GET /api/v1/orgs/{name}` | `OrgRead` | rust+web+smoke |
| _(загрузка страницы)_ | — | `GET /api/v1/orgs` | `User` | rust+smoke |

### `/[owner]/[repo]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+browser |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+browser |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+browser |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/tree` | `RepoRead` | rust+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/log` | `RepoRead` | rust+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+smoke+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/blob/{*path}` | `RepoRead` | rust |

### `/[owner]/[repo]/blob/[...path]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:repo.blob.deleting | :308 | `DELETE /api/v1/repos/{owner}/{name}/contents/{*path}` | `RepoWrite` | rust+browser |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/blob/{*path}` | `RepoRead` | rust+browser |

### `/[owner]/[repo]/boards`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.create | :341 | `POST /api/v1/repos/{owner}/{name}/boards` | `RepoWrite` | rust+browser |
| i18n:common.create | :341 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust+browser |
| i18n:common.save | :377 | `PATCH /api/v1/repos/{owner}/{name}/boards/{id}/cards/{card_id}` | `RepoWrite` | browser |
| i18n:common.save | :377 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust |
| &times; | :401 | `DELETE /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoWrite` | rust+browser |
| &times; | :401 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust |
| i18n:common.save | :436 | `PATCH /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoWrite` | browser |
| &times; | :469 | `DELETE /api/v1/repos/{owner}/{name}/boards/{id}/columns/{col_id}` | `RepoWrite` | rust+browser |
| ↑ | :478 | `POST /api/v1/repos/{owner}/{name}/boards/{id}/cards/reorder` | `RepoWrite` | rust |
| ↓ | :485 | `POST /api/v1/repos/{owner}/{name}/boards/{id}/cards/reorder` | `RepoWrite` | rust+browser |
| &times; | :493 | `DELETE /api/v1/repos/{owner}/{name}/boards/{id}/cards/{card_id}` | `RepoWrite` | rust+browser |
| i18n:board.moveTo | :502 | `POST /api/v1/repos/{owner}/{name}/boards/{id}/cards/{card_id}/move` | `RepoWrite` | rust+browser |
| i18n:board.moveTo | :502 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust |
| i18n:common.add | :520 | `POST /api/v1/repos/{owner}/{name}/boards/{id}/columns/{col_id}/cards` | `RepoWrite` | rust+browser |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/boards` | `RepoRead` | rust+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/issues` | `RepoRead` | rust+browser |

### `/[owner]/[repo]/commits`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/log` | `RepoRead` | rust |

### `/[owner]/[repo]/commits/[sha]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Retry | :146 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/status` | `RepoRead` | rust+browser |
| Retry | :146 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/statuses` | `RepoRead` | rust+browser |
| Retry | :146 | `GET /api/v1/repos/{owner}/{name}/log` | `RepoRead` | rust |
| Retry | :146 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/signature` | `RepoRead` | rust+browser |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |

### `/[owner]/[repo]/edit/[...path]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/blob/{*path}` | `RepoRead` | rust |
| _(загрузка страницы)_ | — | `POST /api/v1/repos/{owner}/{name}/contents/{*path}` | `RepoWrite` | rust |

### `/[owner]/[repo]/issues`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:issues.tabs.open | :101 | `GET /api/v1/repos/{owner}/{name}/issues` | `RepoRead` | rust |
| i18n:issues.tabs.closed | :108 | `GET /api/v1/repos/{owner}/{name}/issues` | `RepoRead` | rust |
| i18n:issues.tabs.all | :115 | `GET /api/v1/repos/{owner}/{name}/issues` | `RepoRead` | rust |
| i18n:issues.new | :123 | `GET /api/v1/repos/{owner}/{name}/issue_templates` | `RepoRead` | rust+browser |
| i18n:issues.new | :123 | `GET /api/v1/repos/{owner}/{name}/issue_config` | `RepoRead` | rust+browser |
| }> | :171 | `POST /api/v1/repos/{owner}/{name}/issues` | `RepoAuthRead` | rust+browser |
| }> | :171 | `GET /api/v1/repos/{owner}/{name}/issues` | `RepoRead` | rust+browser |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |

### `/[owner]/[repo]/issues/[number]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.loading | :188 | `PATCH /api/v1/repos/{owner}/{name}/issues/{number}` | `RepoWrite` | rust |
| i18n:issues.comment_placeholder | :217 | `POST /api/v1/repos/{owner}/{name}/issues/{number}/comments` | `RepoAuthRead` | rust+browser |
| i18n:issues.comment_placeholder | :217 | `GET /api/v1/repos/{owner}/{name}/issues/{number}` | `RepoRead` | rust+browser |
| i18n:issues.comment_placeholder | :217 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/comments` | `RepoRead` | rust+browser |
| i18n:issues.comment_placeholder | :217 | `GET /api/v1/repos/{owner}/{name}/milestones` | `RepoRead` | rust+browser |
| i18n:issues.comment_placeholder | :217 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust+browser |
| i18n:issues.comment_placeholder | :217 | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+smoke |
| i18n:issues.close_issue | :221 | `PATCH /api/v1/repos/{owner}/{name}/issues/{number}` | `RepoWrite` | rust+browser |
| i18n:issues.close_issue | :221 | `GET /api/v1/repos/{owner}/{name}/issues/{number}` | `RepoRead` | rust |
| i18n:issues.close_issue | :221 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/comments` | `RepoRead` | rust |
| i18n:issues.close_issue | :221 | `GET /api/v1/repos/{owner}/{name}/milestones` | `RepoRead` | rust |
| i18n:issues.close_issue | :221 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust |
| i18n:issues.close_issue | :221 | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+smoke |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:attachments.delete | `AttachmentPanel` | `DELETE /api/v1/{__opaque__}/{id}` | `?` | **—** |
| i18n:attachments.delete | `AttachmentPanel` | `DELETE /api/v1/{__opaque__}/{id}` | `?` | **—** |

### `/[owner]/[repo]/issues/board`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| async () => { activeBoardId = b.id; awai | :241 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust |
| handleCreateBoard | :261 | `POST /api/v1/repos/{owner}/{name}/boards` | `RepoWrite` | rust |
| handleCreateBoard | :261 | `GET /api/v1/repos/{owner}/{name}/boards` | `RepoRead` | rust |
| handleCreateBoard | :261 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust |
| Add | :276 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust |
| ✕ | :298 | `DELETE /api/v1/repos/{owner}/{name}/boards/{id}/columns/{col_id}` | `RepoWrite` | rust |
| ✕ | :298 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust |
| ✕ | :326 | `DELETE /api/v1/repos/{owner}/{name}/boards/{id}/cards/{card_id}` | `RepoWrite` | rust |
| ✕ | :326 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust |
| Add | :347 | `POST /api/v1/repos/{owner}/{name}/boards/{id}/columns/{col_id}/cards` | `RepoWrite` | rust |
| Add | :347 | `GET /api/v1/repos/{owner}/{name}/boards/{id}` | `RepoRead` | rust |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| _(загрузка страницы)_ | — | `POST /api/v1/repos/{owner}/{name}/boards/{id}/cards/reorder` | `RepoWrite` | rust |
| _(загрузка страницы)_ | — | `POST /api/v1/repos/{owner}/{name}/boards/{id}/cards/{card_id}/move` | `RepoWrite` | rust |

### `/[owner]/[repo]/milestones`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:milestones.name | :145 | `POST /api/v1/repos/{owner}/{name}/milestones` | `RepoWrite` | rust+browser |
| i18n:milestones.name | :145 | `PATCH /api/v1/repos/{owner}/{name}/milestones/{id}` | `RepoWrite` | rust+browser |
| i18n:milestones.name | :145 | `GET /api/v1/repos/{owner}/{name}/milestones` | `RepoRead` | rust+browser |
| i18n:common.edit | :205 | `GET /api/v1/repos/{owner}/{name}/milestones/{id}` | `RepoRead` | rust+browser |
| i18n:milestones.close | :206 | `PATCH /api/v1/repos/{owner}/{name}/milestones/{id}` | `RepoWrite` | rust |
| i18n:milestones.close | :206 | `GET /api/v1/repos/{owner}/{name}/milestones` | `RepoRead` | rust |
| i18n:common.delete | :209 | `DELETE /api/v1/repos/{owner}/{name}/milestones/{id}` | `RepoWrite` | rust+browser |
| i18n:common.delete | :209 | `GET /api/v1/repos/{owner}/{name}/milestones` | `RepoRead` | rust |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |

### `/[owner]/[repo]/network`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.retry | :100 | `GET /api/v1/repos/{owner}/{name}/stargazers` | `RepoRead` | rust+web+browser |
| i18n:common.retry | :154 | `GET /api/v1/repos/{owner}/{name}/forks` | `RepoRead` | rust+web+browser |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |

### `/[owner]/[repo]/new`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| _(загрузка страницы)_ | — | `POST /api/v1/repos/{owner}/{name}/contents/{*path}` | `RepoWrite` | rust+browser |

### `/[owner]/[repo]/packages`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.all | :89 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust |
| i18n:common.all | :89 | `GET /api/v1/repos/{owner}/{name}/packages` | `RepoRead` | rust |
| i18n:common.all | :89 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust |
| i18n:common.search | :104 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust |
| i18n:common.search | :104 | `GET /api/v1/repos/{owner}/{name}/packages` | `RepoRead` | rust |
| i18n:common.search | :104 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust |
| i18n:common.previous | :143 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust |
| i18n:common.previous | :143 | `GET /api/v1/repos/{owner}/{name}/packages` | `RepoRead` | rust |
| i18n:common.previous | :143 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust |
| i18n:common.next | :151 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust |
| i18n:common.next | :151 | `GET /api/v1/repos/{owner}/{name}/packages` | `RepoRead` | rust |
| i18n:common.next | :151 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |

### `/[owner]/[repo]/packages/[format]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust+browser |

### `/[owner]/[repo]/packages/[format]/[...name]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:packages.unyank | :186 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/versions` | `RepoRead` | rust+browser |
| i18n:common.delete | :216 | `DELETE /api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}` | `RepoWrite` | rust |
| i18n:common.delete | :216 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}` | `RepoRead` | rust+smoke+browser |
| i18n:common.delete | :216 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/versions` | `RepoRead` | rust |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |

### `/[owner]/[repo]/packages/upload`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Name Homepage Repository URL Semver | :96 | `POST /api/v1/repos/{owner}/{name}/packages/{pkg_type}/publish` | `RepoWrite` | rust |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |

### `/[owner]/[repo]/pipelines`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| updateTriggerRef(event.currentTarget.value)} onchange= place | :429 | `POST /api/v1/repos/{owner}/{name}/pipelines` | `RepoWrite` | rust+web+browser |
| updateTriggerRef(event.currentTarget.value)} onchange= place | :429 | `GET /api/v1/repos/{owner}/{name}/pipelines` | `RepoRead` | rust+browser |
| updateTriggerRef(event.currentTarget.value)} onchange= place | :429 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}` | `RepoRead` | rust+web+browser |
| updateTriggerRef(event.currentTarget.value)} onchange= place | :429 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}/artifacts` | `RepoRead` | rust+web+browser |
| i18n:pipeline.retry | :557 | `POST /api/v1/repos/{owner}/{name}/pipelines/{id}/retry` | `RepoWrite` | rust |
| i18n:pipeline.retry | :557 | `GET /api/v1/repos/{owner}/{name}/pipelines` | `RepoRead` | rust |
| i18n:pipeline.retry | :557 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}` | `RepoRead` | rust+web |
| i18n:pipeline.retry | :557 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}/artifacts` | `RepoRead` | rust+web |
| i18n:pipeline.cancel | :560 | `POST /api/v1/repos/{owner}/{name}/pipelines/{id}/cancel` | `RepoWrite` | rust |
| i18n:pipeline.play_manual | :620 | `POST /api/v1/repos/{owner}/{name}/pipelines/{id}/jobs/{job_id}/play` | `RepoWrite` | rust |
| i18n:pipeline.play_manual | :620 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}` | `RepoRead` | rust+web |
| i18n:pipeline.approval_recorded | :623 | `POST /api/v1/repos/{owner}/{name}/pipelines/{pipeline_id}/jobs/{job_id}/approve` | `RepoAuthRead` | rust |
| i18n:pipeline.approval_recorded | :623 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}` | `RepoRead` | rust+web |
| i18n:pipeline.artifact_deleting | :664 | `DELETE /api/v1/artifacts/{id}` | `RepoWrite` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | **—** |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/pipelines/workflow-dispatch` | `RepoRead` | rust+web+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}/jobs/{job_id}` | `RepoRead` | rust+browser |

### `/[owner]/[repo]/pulls`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:pulls.tabs.open | :93 | `GET /api/v1/repos/{owner}/{name}/pulls` | `RepoRead` | rust |
| i18n:pulls.tabs.closed | :100 | `GET /api/v1/repos/{owner}/{name}/pulls` | `RepoRead` | rust |
| i18n:pulls.tabs.merged | :107 | `GET /api/v1/repos/{owner}/{name}/pulls` | `RepoRead` | rust |
| i18n:pulls.new | :115 | `GET /api/v1/repos/{owner}/{name}/pull_request_template` | `RepoRead` | rust+browser |
| → showCreate = false}> | :123 | `POST /api/v1/repos/{owner}/{name}/pulls` | `RepoAuthRead` | rust+browser |
| → showCreate = false}> | :123 | `GET /api/v1/repos/{owner}/{name}/pulls` | `RepoRead` | rust+browser |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | **—** |

### `/[owner]/[repo]/pulls/[number]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:pulls.mark_ready | :406 | `PATCH /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoWrite` | rust+browser |
| × | :449 | `DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers/{username}` | `RepoWrite` | rust+browser |
| i18n:pulls.reviewers.request | :460 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoWrite` | rust+browser |
| i18n:pulls.reviewers.request | :460 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust |
| i18n:pulls.fork_ci.approving | :473 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/ci-approval` | `RepoWrite` | web |
| i18n:pulls.fork_ci.approving | :473 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust |
| i18n:pulls.merge.leave_queue | :490 | `DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/merge-queue` | `RepoWrite` | browser |
| i18n:pulls.merge.leave_queue | :490 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust |
| i18n:pulls.merge.leave_queue | :490 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust |
| i18n:pulls.merge.leave_queue | :490 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust |
| i18n:pulls.merge.leave_queue | :490 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust |
| i18n:pulls.merge.leave_queue | :490 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust |
| i18n:pulls.merge.leave_queue | :490 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust |
| i18n:pulls.merge.leave_queue | :490 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust |
| i18n:pulls.merge.disable_auto | :501 | `DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/auto-merge` | `RepoWrite` | browser |
| i18n:pulls.merge.merging | :512 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/merge` | `RepoWrite` | rust |
| i18n:pulls.merge.merging | :512 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust |
| i18n:pulls.merge.merging | :512 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust |
| i18n:pulls.merge.merging | :512 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust |
| i18n:pulls.merge.merging | :512 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust |
| i18n:pulls.merge.merging | :512 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust |
| i18n:pulls.merge.merging | :512 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust |
| i18n:pulls.merge.merging | :512 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust |
| i18n:pulls.merge.enabling_auto | :515 | `PUT /api/v1/repos/{owner}/{name}/pulls/{number}/auto-merge` | `RepoWrite` | rust+browser |
| i18n:pulls.merge.enabling_auto | :515 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust |
| i18n:pulls.merge.enabling_auto | :515 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust |
| i18n:pulls.merge.enabling_auto | :515 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust |
| i18n:pulls.merge.enabling_auto | :515 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust |
| i18n:pulls.merge.enabling_auto | :515 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust |
| i18n:pulls.merge.enabling_auto | :515 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust |
| i18n:pulls.merge.enabling_auto | :515 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust |
| i18n:pulls.merge.joining_queue | :518 | `PUT /api/v1/repos/{owner}/{name}/pulls/{number}/merge-queue` | `RepoWrite` | rust+browser |
| i18n:pulls.merge.joining_queue | :518 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust |
| i18n:pulls.merge.joining_queue | :518 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust |
| i18n:pulls.merge.joining_queue | :518 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust |
| i18n:pulls.merge.joining_queue | :518 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust |
| i18n:pulls.merge.joining_queue | :518 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust |
| i18n:pulls.merge.joining_queue | :518 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust |
| i18n:pulls.merge.joining_queue | :518 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust |
| i18n:pulls.review.dismissing | :566 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/reviews/{id}/dismiss` | `RepoWrite` | rust+web |
| i18n:pulls.review.dismissing | :566 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust |
| i18n:pulls.review.dismissing | :566 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust |
| i18n:pulls.review.dismissing | :566 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust |
| i18n:pulls.review.dismissing | :566 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust |
| i18n:pulls.review.dismissing | :566 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust |
| i18n:pulls.review.dismissing | :566 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust |
| i18n:pulls.review.dismissing | :566 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust |
| i18n:pulls.suggestion.applying_selected | :590 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/suggestions/apply` | `RepoWrite` | rust |
| i18n:pulls.suggestion.applying_selected | :590 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust |
| i18n:pulls.suggestion.applying_selected | :590 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust |
| i18n:pulls.suggestion.applying_selected | :590 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust |
| i18n:pulls.suggestion.applying_selected | :590 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust |
| i18n:pulls.suggestion.applying_selected | :590 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust |
| i18n:pulls.suggestion.applying_selected | :590 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust |
| i18n:pulls.suggestion.applying_selected | :590 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust |
| i18n:pulls.suggestion.apply | :631 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}/suggestion/apply` | `RepoWrite` | rust |
| i18n:pulls.suggestion.apply | :631 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust |
| i18n:pulls.suggestion.apply | :631 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust |
| i18n:pulls.suggestion.apply | :631 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust |
| i18n:pulls.suggestion.apply | :631 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust |
| i18n:pulls.suggestion.apply | :631 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust |
| i18n:pulls.suggestion.apply | :631 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust |
| i18n:pulls.suggestion.apply | :631 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust |
| i18n:pulls.threads.reopen | :642 | `PATCH /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}/resolution` | `RepoWrite` | **—** |
| i18n:pulls.threads.reopen | :642 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust |
| i18n:pulls.suggestion.apply | :699 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}/suggestion/apply` | `RepoWrite` | rust |
| i18n:pulls.suggestion.apply | :699 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust |
| i18n:pulls.suggestion.apply | :699 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust |
| i18n:pulls.suggestion.apply | :699 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust |
| i18n:pulls.suggestion.apply | :699 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust |
| i18n:pulls.suggestion.apply | :699 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust |
| i18n:pulls.suggestion.apply | :699 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust |
| i18n:pulls.suggestion.apply | :699 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust |
| i18n:pulls.threads.reopen | :708 | `PATCH /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}/resolution` | `RepoWrite` | browser |
| i18n:pulls.threads.reopen | :708 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust |
| i18n:pulls.diff.submit_comment | :727 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoAuthRead` | rust+browser |
| i18n:pulls.diff.submit_comment | :727 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust |
| i18n:pulls.review.submit | :761 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoAuthRead` | rust+browser |
| i18n:pulls.review.submit | :761 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+browser |
| i18n:pulls.review.submit | :761 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+browser |
| i18n:pulls.review.submit | :761 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+browser |
| i18n:pulls.review.submit | :761 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+browser |
| i18n:pulls.review.submit | :761 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+browser |
| i18n:pulls.review.submit | :761 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+browser |
| i18n:pulls.review.submit | :761 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+browser |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:attachments.delete | `AttachmentPanel` | `DELETE /api/v1/{__opaque__}/{id}` | `?` | **—** |
| i18n:attachments.delete | `AttachmentPanel` | `DELETE /api/v1/{__opaque__}/{id}` | `?` | **—** |
| i18n:attachments.delete | `AttachmentPanel` | `DELETE /api/v1/{__opaque__}/{id}` | `?` | **—** |

### `/[owner]/[repo]/releases`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:releases.attestation.verifying | :364 | `POST /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation/verify` | `RepoRead` | rust+web |
| i18n:releases.attestation.signing | :375 | `POST /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation` | `RepoWrite` | rust+web |
| i18n:common.delete | :401 | `DELETE /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}` | `RepoWrite` | rust+browser |
| i18n:common.delete | :435 | `DELETE /api/v1/repos/{owner}/{name}/releases/{id}` | `RepoWrite` | rust |
| i18n:common.delete | :435 | `GET /api/v1/instance` | `Public` | rust+browser |
| i18n:common.delete | :435 | `GET /api/v1/repos/{owner}/{name}/releases` | `RepoRead` | rust |
| i18n:common.delete | :435 | `GET /api/v1/repos/{owner}/{name}/releases/{release_id}/assets` | `RepoRead` | rust+web |
| i18n:common.delete | :435 | `GET /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation` | `RepoRead` | rust+web |
| Previous | :450 | `GET /api/v1/instance` | `Public` | rust |
| Previous | :450 | `GET /api/v1/repos/{owner}/{name}/releases` | `RepoRead` | rust |
| Previous | :450 | `GET /api/v1/repos/{owner}/{name}/releases/{release_id}/assets` | `RepoRead` | rust+web |
| Previous | :450 | `GET /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation` | `RepoRead` | rust+web |
| Next | :458 | `GET /api/v1/instance` | `Public` | rust |
| Next | :458 | `GET /api/v1/repos/{owner}/{name}/releases` | `RepoRead` | rust |
| Next | :458 | `GET /api/v1/repos/{owner}/{name}/releases/{release_id}/assets` | `RepoRead` | rust+web |
| Next | :458 | `GET /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |

### `/[owner]/[repo]/releases/edit/[id]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Tag * | :105 | `PATCH /api/v1/repos/{owner}/{name}/releases/{id}` | `RepoWrite` | rust |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/releases/{id}` | `RepoRead` | rust |

### `/[owner]/[repo]/releases/new`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| * Existing tags: tagName = tag} > + more * selectedTargetTyp | :98 | `POST /api/v1/repos/{owner}/{name}/releases` | `RepoWrite` | rust |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | **—** |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/tags` | `RepoRead` | browser |

### `/[owner]/[repo]/settings`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.transfer.confirming | :165 | `POST /api/v1/repos/{owner}/{name}/transfer` | `RepoOwner` | rust |
| i18n:settings.delete.confirming | :199 | `DELETE /api/v1/repos/{owner}/{name}` | `RepoOwner` | rust |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+smoke |

### `/[owner]/[repo]/settings/branches`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.branch_protection.branch | :160 | `PATCH /api/v1/repos/{owner}/{name}/branches/protection/{id}` | `RepoAdmin` | browser |
| i18n:settings.branch_protection.branch | :160 | `POST /api/v1/repos/{owner}/{name}/branches/protection` | `RepoAdmin` | rust+browser |
| i18n:settings.branch_protection.branch | :160 | `GET /api/v1/repos/{owner}/{name}/branches/protection` | `RepoRead` | rust+browser |
| i18n:common.delete | :257 | `DELETE /api/v1/repos/{owner}/{name}/branches/protection/{id}` | `RepoAdmin` | browser |
| i18n:common.delete | :257 | `GET /api/v1/repos/{owner}/{name}/branches/protection` | `RepoRead` | rust |

### `/[owner]/[repo]/settings/ci-secrets`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Name Value Save secret | :91 | `PUT /api/v1/repos/{owner}/{name}/actions/secrets/{secret_name}` | `RepoAdmin` | rust+browser |
| Name Value Save secret | :91 | `GET /api/v1/repos/{owner}/{name}/actions/secrets` | `RepoAdmin` | rust+browser |
| Delete | :91 | `DELETE /api/v1/repos/{owner}/{name}/actions/secrets/{secret_name}` | `RepoAdmin` | rust+browser |
| Delete | :91 | `GET /api/v1/repos/{owner}/{name}/actions/secrets` | `RepoAdmin` | rust |

### `/[owner]/[repo]/settings/collaborators`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.collaborators.user_identifier | :132 | `POST /api/v1/repos/{owner}/{name}/collaborators` | `RepoAdmin` | rust+smoke+browser |
| i18n:settings.collaborators.user_identifier | :132 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust+browser |
| i18n:common.save | :195 | `PATCH /api/v1/repos/{owner}/{name}/collaborators/{id}` | `RepoAdmin` | rust+browser |
| i18n:common.save | :195 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust |
| i18n:common.delete | :202 | `DELETE /api/v1/repos/{owner}/{name}/collaborators/{id}` | `RepoAdmin` | rust+browser |
| i18n:common.delete | :202 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust |

### `/[owner]/[repo]/settings/deploy-keys`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.deploy_keys.name | :91 | `POST /api/v1/repos/{owner}/{name}/keys` | `RepoAdmin` | rust+browser |
| i18n:settings.deploy_keys.name | :91 | `GET /api/v1/repos/{owner}/{name}/keys` | `RepoAdmin` | rust+browser |
| i18n:common.delete | :116 | `DELETE /api/v1/repos/{owner}/{name}/keys/{id}` | `RepoAdmin` | rust+browser |
| i18n:common.delete | :116 | `GET /api/v1/repos/{owner}/{name}/keys` | `RepoAdmin` | rust |

### `/[owner]/[repo]/settings/environments`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Name Require approval Required approvals Allowed approvers ( | :100 | `POST /api/v1/repos/{owner}/{name}/actions/environments` | `RepoAdmin` | rust+browser |
| Name Require approval Required approvals Allowed approvers ( | :100 | `PUT /api/v1/repos/{owner}/{name}/actions/environments/{id}` | `RepoAdmin` | browser |
| Name Require approval Required approvals Allowed approvers ( | :100 | `GET /api/v1/repos/{owner}/{name}/actions/environments` | `RepoRead` | rust+browser |
| Delete | :107 | `DELETE /api/v1/repos/{owner}/{name}/actions/environments/{id}` | `RepoAdmin` | rust+browser |
| Delete | :107 | `GET /api/v1/repos/{owner}/{name}/actions/environments` | `RepoRead` | rust |

### `/[owner]/[repo]/settings/labels`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.save_label | :251 | `PATCH /api/v1/repos/{owner}/{name}/labels/{id}` | `RepoWrite` | rust+browser |
| i18n:settings.save_label | :251 | `POST /api/v1/repos/{owner}/{name}/labels` | `RepoWrite` | rust+browser |
| i18n:settings.save_label | :251 | `GET /api/v1/repos/{owner}/{name}/labels` | `RepoRead` | rust+browser |
| handleDelete | :277 | `DELETE /api/v1/repos/{owner}/{name}/labels/{id}` | `RepoWrite` | rust+browser |
| handleDelete | :277 | `GET /api/v1/repos/{owner}/{name}/labels` | `RepoRead` | rust |

### `/[owner]/[repo]/settings/mirror`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:settings.mirror.url | :145 | `PATCH /api/v1/repos/{owner}/{name}/mirror` | `RepoWrite` | **—** |
| i18n:settings.mirror.url | :145 | `POST /api/v1/repos/{owner}/{name}/mirror` | `RepoWrite` | rust |
| i18n:common.loading | :186 | `POST /api/v1/repos/{owner}/{name}/mirror/sync` | `RepoWrite` | **—** |
| i18n:common.loading | :186 | `GET /api/v1/repos/{owner}/{name}/mirror` | `RepoWrite` | rust |
| i18n:common.loading | :189 | `DELETE /api/v1/repos/{owner}/{name}/mirror` | `RepoWrite` | rust |

### `/[owner]/[repo]/settings/retention`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Artifact retention (days) Cache retention after last access  | :15 | `PUT /api/v1/repos/{owner}/{name}/actions/retention` | `RepoAdmin` | browser |
| Clean expired storage now | :15 | `DELETE /api/v1/repos/{owner}/{name}/actions/retention/expired` | `RepoAdmin` | rust+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/actions/retention` | `RepoAdmin` | rust+browser |

### `/[owner]/[repo]/settings/tags`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Pattern (use * as the wildcard; ? , character classes, and + | :100 | `POST /api/v1/repos/{owner}/{name}/tags/protection` | `RepoAdmin` | rust+browser |
| Pattern (use * as the wildcard; ? , character classes, and + | :100 | `PATCH /api/v1/repos/{owner}/{name}/tags/protection/{id}` | `RepoAdmin` | rust+browser |
| Pattern (use * as the wildcard; ? , character classes, and + | :100 | `GET /api/v1/repos/{owner}/{name}/tags/protection` | `RepoRead` | rust+browser |
| Delete | :107 | `DELETE /api/v1/repos/{owner}/{name}/tags/protection/{id}` | `RepoAdmin` | rust+browser |
| Delete | :107 | `GET /api/v1/repos/{owner}/{name}/tags/protection` | `RepoRead` | rust |

### `/[owner]/[repo]/settings/webhooks`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| application/json application/x-www-form-urlencoded toggleEve | :237 | `POST /api/v1/repos/{owner}/{name}/hooks` | `RepoAdmin` | rust+browser |
| application/json application/x-www-form-urlencoded toggleEve | :237 | `GET /api/v1/repos/{owner}/{name}/hooks` | `RepoAdmin` | rust+browser |
| (e) => setActive(hook, e.currentTarget.c | :305 | `PATCH /api/v1/repos/{owner}/{name}/hooks/{id}` | `RepoAdmin` | rust+browser |
| i18n:settings.webhooks.hide_deliveries | :308 | `GET /api/v1/repos/{owner}/{name}/hooks/{id}` | `RepoAdmin` | rust+browser |
| i18n:settings.webhooks.hide_deliveries | :308 | `GET /api/v1/repos/{owner}/{name}/hooks/{id}/deliveries` | `RepoAdmin` | browser |
| i18n:common.loading | :319 | `DELETE /api/v1/repos/{owner}/{name}/hooks/{id}` | `RepoAdmin` | browser |
| i18n:common.loading | :337 | `GET /api/v1/repos/{owner}/{name}/hooks/{id}/deliveries` | `RepoAdmin` | **—** |
| i18n:settings.webhooks.redelivering | :372 | `POST /api/v1/repos/{owner}/{name}/hooks/{id}/deliveries/{delivery_id}/redeliver` | `RepoAdmin` | rust+browser |
| i18n:settings.webhooks.redelivering | :372 | `GET /api/v1/repos/{owner}/{name}/hooks/{id}/deliveries` | `RepoAdmin` | **—** |

### `/[owner]/[repo]/time_tracking`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| # | :147 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time` | `RepoRead` | rust+browser |
| # | :147 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time/total` | `RepoRead` | rust+browser |
| handleAdd | :191 | `POST /api/v1/repos/{owner}/{name}/issues/{number}/time` | `RepoWrite` | rust+browser |
| handleAdd | :191 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time` | `RepoRead` | rust |
| handleAdd | :191 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time/total` | `RepoRead` | rust |
| Delete | :220 | `DELETE /api/v1/repos/{owner}/{name}/issues/{number}/time/{id}` | `RepoWrite` | rust+browser |
| Delete | :220 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time` | `RepoRead` | rust |
| Delete | :220 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time/total` | `RepoRead` | rust |
| Previous | :229 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time` | `RepoRead` | rust |
| Next | :232 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/time` | `RepoRead` | rust |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/issues` | `RepoRead` | rust |

### `/[owner]/[repo]/wiki`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| showCreate = false}> | :61 | `POST /api/v1/repos/{owner}/{name}/wiki` | `RepoWrite` | rust+browser |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/wiki` | `RepoRead` | rust+browser |

### `/[owner]/[repo]/wiki/[title]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| History | :200 | `GET /api/v1/repos/{owner}/{name}/wiki/{title}/history` | `RepoRead` | rust+browser |
| i18n:wiki.delete | :202 | `DELETE /api/v1/repos/{owner}/{name}/wiki/{title}` | `RepoWrite` | rust+browser |
| v | :217 | `GET /api/v1/repos/{owner}/{name}/wiki/{title}/revisions/{rev_id}` | `RepoRead` | rust+browser |
| Restore this version | :226 | `PATCH /api/v1/repos/{owner}/{name}/wiki/{title}` | `RepoWrite` | rust |
| Restore this version | :226 | `GET /api/v1/repos/{owner}/{name}/wiki/{title}` | `RepoRead` | rust |
| Restore this version | :226 | `GET /api/v1/repos/{owner}/{name}/wiki` | `RepoRead` | rust |
| i18n:wiki.save | :240 | `PATCH /api/v1/repos/{owner}/{name}/wiki/{title}` | `RepoWrite` | rust+browser |
| i18n:wiki.save | :240 | `GET /api/v1/repos/{owner}/{name}/wiki/{title}` | `RepoRead` | rust+browser |
| i18n:wiki.save | :240 | `GET /api/v1/repos/{owner}/{name}/wiki` | `RepoRead` | rust |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |

### `/[owner]/[repo]/wiki/[title]/history`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.view | :103 | `GET /api/v1/repos/{owner}/{name}/wiki/{title}/revisions/{rev_id}` | `RepoRead` | rust |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}/{name}/wiki/{title}/history` | `RepoRead` | rust |

### `/admin/audit` 🔒

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| applyFilter | :148 | `GET /api/v1/admin/audit/logs` | `InstanceAdmin` | rust+smoke+browser |
| : All User Repository Organization | :153 | `GET /api/v1/admin/audit/logs` | `InstanceAdmin` | rust+smoke |
| Clear filters | :160 | `GET /api/v1/admin/audit/logs` | `InstanceAdmin` | rust+smoke |
| i18n:admin.audit.fields.details | :216 | `GET /api/v1/admin/audit/logs/{id}` | `InstanceAdmin` | rust+smoke+browser |
| ← Prev | :229 | `GET /api/v1/admin/audit/logs` | `InstanceAdmin` | rust+smoke |
| Next → | :231 | `GET /api/v1/admin/audit/logs` | `InstanceAdmin` | rust+smoke |

### `/admin/orgs` 🔒

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| ← Prev | :138 | `GET /api/v1/admin/orgs` | `InstanceAdmin` | rust |
| Next → | :140 | `GET /api/v1/admin/orgs` | `InstanceAdmin` | rust |
| i18n:common.loading | :164 | `DELETE /api/v1/admin/orgs/{name}` | `InstanceAdmin` | rust+smoke+browser |
| i18n:common.loading | :164 | `GET /api/v1/admin/orgs` | `InstanceAdmin` | rust+browser |

### `/admin/runners` 🔒

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.loading | :160 | `POST /api/v1/runners/register` | `InstanceAdmin` | rust+smoke+browser |
| i18n:common.loading | :160 | `GET /api/v1/admin/runners` | `InstanceAdmin` | smoke+browser |
| i18n:common.previous | :210 | `GET /api/v1/admin/runners` | `InstanceAdmin` | smoke |
| i18n:common.next | :212 | `GET /api/v1/admin/runners` | `InstanceAdmin` | smoke |
| i18n:common.loading | :237 | `DELETE /api/v1/admin/runners/{id}` | `InstanceAdmin` | rust+smoke+browser |
| i18n:common.loading | :237 | `GET /api/v1/admin/runners` | `InstanceAdmin` | smoke+browser |

### `/admin/settings` 🔒

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| saveSettings | :331 | `PATCH /api/v1/admin/settings` | `InstanceAdmin` | rust+smoke+browser |
| () => testSsoProvider(provider) | :361 | `POST /api/v1/admin/sso/providers/{id}/test` | `InstanceAdmin` | rust+smoke+browser |
| () => toggleSsoProvider(provider) | :365 | `PATCH /api/v1/admin/sso/providers/{id}` | `InstanceAdmin` | rust+smoke+browser |
| () => toggleSsoProvider(provider) | :365 | `GET /api/v1/admin/sso/providers` | `InstanceAdmin` | rust+smoke+browser |
| Delete | :369 | `DELETE /api/v1/admin/sso/providers/{id}` | `InstanceAdmin` | rust+smoke+browser |
| Delete | :369 | `GET /api/v1/admin/sso/providers` | `InstanceAdmin` | rust+smoke+browser |
| saveSsoProvider | :487 | `PATCH /api/v1/admin/sso/providers/{id}` | `InstanceAdmin` | rust+smoke |
| saveSsoProvider | :487 | `POST /api/v1/admin/sso/providers` | `InstanceAdmin` | rust+smoke+browser |
| saveSsoProvider | :487 | `GET /api/v1/admin/sso/providers` | `InstanceAdmin` | rust+smoke+browser |
| () => loadLoginAttempts(loginAttemptsPag | :503 | `GET /api/v1/admin/login-attempts` | `InstanceAdmin` | rust+smoke+browser |
| Apply | :517 | `GET /api/v1/admin/login-attempts` | `InstanceAdmin` | rust+smoke |
| Previous | :536 | `GET /api/v1/admin/login-attempts` | `InstanceAdmin` | rust+smoke |
| Next | :538 | `GET /api/v1/admin/login-attempts` | `InstanceAdmin` | rust+smoke |
| _(загрузка страницы)_ | — | `GET /api/v1/admin/settings` | `InstanceAdmin` | rust+smoke+browser |

### `/admin/users` 🔒

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| () => handleUnlock(u) | :203 | `POST /api/v1/admin/users/{id}/unlock` | `InstanceAdmin` | rust+smoke+browser |
| () => handleUnlock(u) | :203 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+browser |
| ← Prev | :221 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust |
| Next → | :223 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust |
| i18n:common.loading | :267 | `PATCH /api/v1/admin/users/{id}` | `InstanceAdmin` | rust+smoke+browser |
| i18n:common.loading | :267 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+browser |
| i18n:common.loading | :294 | `DELETE /api/v1/admin/users/{id}` | `InstanceAdmin` | rust+smoke+browser |
| i18n:common.loading | :294 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+browser |

### `/dashboard`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| * / | :165 | `POST /api/v1/repos` | `User` | rust+smoke+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/{owner}` | `PublicFiltered` | rust+smoke+browser |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/templates/gitignores` | `Public` | **—** |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/templates/licenses` | `Public` | **—** |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/templates/readmes` | `Public` | **—** |
| _(загрузка страницы)_ | — | `GET /api/v1/repos/templates/labels` | `Public` | **—** |
| _(загрузка страницы)_ | — | `GET /api/v1/orgs` | `User` | rust+smoke+browser |

### `/explore`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| ← | :77 | `GET /api/v1/repos/explore` | `PublicFiltered` | rust+smoke |
| → | :83 | `GET /api/v1/repos/explore` | `PublicFiltered` | rust+smoke |

### `/forgot-password`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Email | :54 | `POST /api/v1/users/forgot-password` | `Public` | rust |

### `/imports`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Refresh | :145 | `GET /api/v1/imports` | `User` | rust |
| Platform GitHub GitLab Gitea Git Source repository URL Targe | :157 | `POST /api/v1/imports` | `User` | rust |
| Platform GitHub GitLab Gitea Git Source repository URL Targe | :157 | `GET /api/v1/imports` | `User` | rust |
| Delete | :248 | `DELETE /api/v1/imports/{id}` | `User` | rust |

### `/login`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| _(загрузка страницы)_ | — | `GET /api/v1/auth/sso/providers` | `Public` | rust |

### `/notifications`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| load | :91 | `GET /api/v1/notifications` | `User` | rust |
| load | :91 | `GET /api/v1/notifications/unread-count` | `User` | rust |
| i18n:notifications.mark_all_read | :95 | `POST /api/v1/notifications/mark-all-read` | `User` | rust |
| i18n:notifications.mark_all_read | :95 | `GET /api/v1/notifications` | `User` | rust |
| i18n:notifications.mark_all_read | :95 | `GET /api/v1/notifications/unread-count` | `User` | rust |
| i18n:notifications.mark_read | :124 | `POST /api/v1/notifications/{id}/read` | `User` | rust |
| i18n:notifications.mark_read | :124 | `GET /api/v1/notifications` | `User` | rust |
| i18n:notifications.mark_read | :124 | `GET /api/v1/notifications/unread-count` | `User` | rust |

### `/orgs`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| * | :99 | `GET /api/v1/orgs` | `User` | rust+smoke |

### `/orgs/[name]`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:common.loading | :328 | `DELETE /api/v1/orgs/{name}` | `OrgAdmin` | rust+smoke+browser |
| editingOrg = false} disabled= > | :336 | `PATCH /api/v1/orgs/{name}` | `OrgAdmin` | rust+smoke+browser |
| i18n:orgs.create_repo | :380 | `POST /api/v1/repos` | `User` | rust+smoke |
| i18n:orgs.create_repo | :380 | `GET /api/v1/repos/{owner}` | `PublicFiltered` | rust+smoke |
| i18n:orgs.new_team | :409 | `POST /api/v1/orgs/{name}/teams` | `OrgAdmin` | rust+browser |
| i18n:orgs.new_team | :409 | `GET /api/v1/orgs/{name}/teams` | `OrgRead` | rust |
| i18n:orgs.hide_team_members | :434 | `GET /api/v1/orgs/{name}/teams/{team_id}/members` | `OrgRead` | rust |
| ` ? t('common.loading') : t('common.delete')} | :438 | `DELETE /api/v1/orgs/{name}/teams/{team_id}` | `OrgAdmin` | rust+smoke+browser |
| ` ? t('common.loading') : t('common.delete')} | :438 | `GET /api/v1/orgs/{name}/teams` | `OrgRead` | rust |
| ` ? t('common.loading') : t('common.add')} | :449 | `POST /api/v1/orgs/{name}/teams/{team_id}/members` | `OrgAdmin` | rust+browser |
| ` ? t('common.loading') : t('common.add')} | :449 | `GET /api/v1/orgs/{name}/teams/{team_id}/members` | `OrgRead` | rust |
| -$ ` ? t('common.loading') : t('common.delete')} | :477 | `DELETE /api/v1/orgs/{name}/teams/{team_id}/members/{user_id}` | `OrgAdmin` | rust+browser |
| -$ ` ? t('common.loading') : t('common.delete')} | :477 | `GET /api/v1/orgs/{name}/teams/{team_id}/members` | `OrgRead` | rust |
| i18n:orgs.member_placeholder | :502 | `POST /api/v1/orgs/{name}/members` | `OrgAdmin` | rust+smoke+browser |
| i18n:orgs.member_placeholder | :502 | `GET /api/v1/orgs/{name}/members` | `OrgRead` | rust |
| ` ? t('common.loading') : t('common.delete')} | :528 | `DELETE /api/v1/orgs/{name}/members/{user_id}` | `OrgAdmin` | rust+smoke+browser |
| ` ? t('common.loading') : t('common.delete')} | :528 | `GET /api/v1/orgs/{name}/members` | `OrgRead` | rust |
| _(загрузка страницы)_ | — | `GET /api/v1/orgs/{name}` | `OrgRead` | rust+web+smoke |

### `/reset-password`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| New Password Confirm Password | :99 | `POST /api/v1/users/reset-password` | `Public` | rust |

### `/search`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| _(загрузка страницы)_ | — | `GET /api/v1/search` | `PublicFiltered` | rust |

### `/settings/security`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Current password | :252 | `POST /api/v1/users/mfa/backup/regenerate` | `User` | rust |
| Current password | :252 | `GET /api/v1/users/mfa/backup` | `User` | **—** |
| Current password | :252 | `GET /api/v1/users/passkeys` | `User` | rust |
| Current password | :252 | `GET /api/v1/users/me/sso` | `User` | rust+web |
| Current password | :270 | `POST /api/v1/users/mfa/disable` | `User` | rust |
| Current password | :270 | `GET /api/v1/users/mfa/backup` | `User` | **—** |
| Current password | :270 | `GET /api/v1/users/passkeys` | `User` | rust |
| Current password | :270 | `GET /api/v1/users/me/sso` | `User` | rust+web |
| startSetup | :281 | `POST /api/v1/users/mfa/setup` | `User` | rust |
| Remove | :314 | `DELETE /api/v1/users/passkeys/{id}` | `User` | rust |
| Unlink | :371 | `DELETE /api/v1/auth/sso/{slug}/unlink` | `User` | rust+web |
| Authentication code | :395 | `POST /api/v1/users/mfa/enable` | `User` | rust |
| Authentication code | :395 | `GET /api/v1/users/mfa/backup` | `User` | **—** |
| Authentication code | :395 | `GET /api/v1/users/passkeys` | `User` | rust |
| Authentication code | :395 | `GET /api/v1/users/me/sso` | `User` | rust+web |

### `/settings/ssh-keys`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| i18n:ssh_keys.name | :103 | `POST /api/v1/users/ssh-keys` | `User` | rust+smoke |
| i18n:ssh_keys.name | :103 | `GET /api/v1/users/ssh-keys` | `User` | rust |
| i18n:ssh_keys.deleting | :155 | `DELETE /api/v1/users/ssh-keys/{id}` | `User` | rust |
| i18n:ssh_keys.deleting | :155 | `GET /api/v1/users/ssh-keys` | `User` | rust |

### `/settings/tokens`

| Элемент | Откуда | Вызов | `Access` | тест |
|---|---|---|---|---|
| Name Scopes Expires | :143 | `POST /api/v1/users/tokens` | `User` | rust |
| Name Scopes Expires | :143 | `GET /api/v1/users/tokens` | `User` | rust |
| () => revokeToken(token) | :191 | `DELETE /api/v1/users/tokens/{id}` | `User` | rust |
| () => revokeToken(token) | :191 | `GET /api/v1/users/tokens` | `User` | rust |

## Роуты, недостижимые из браузера

Не дефект: git, OCI, LFS, CI-раннер и вебхуки — легитимные не-браузерные
клиенты. Это список того, что браузерный тест закрыть не может в принципе.

| Метод | URL | `Access` | тест |
|---|---|---|---|
| GET | `/v2` | `Foreign:oci.rs` | rust |
| GET | `/v2` | `Foreign:oci.rs` | rust |
| GET | `/v2/auth/token` | `Public` | rust |
| GET | `/v2/{owner}/{repo}/tags/list` | `Foreign:oci.rs` | rust |
| GET | `/v2/{owner}/{repo}/manifests/{reference}` | `Foreign:oci.rs` | rust+smoke |
| HEAD | `/v2/{owner}/{repo}/manifests/{reference}` | `Foreign:oci.rs` | **—** |
| PUT | `/v2/{owner}/{repo}/manifests/{reference}` | `Foreign:oci.rs` | rust |
| GET | `/v2/{owner}/{repo}/blobs/{digest}` | `Foreign:oci.rs` | rust+smoke |
| HEAD | `/v2/{owner}/{repo}/blobs/{digest}` | `Foreign:oci.rs` | rust |
| POST | `/v2/{owner}/{repo}/blobs/uploads` | `Foreign:oci.rs` | **—** |
| POST | `/v2/{owner}/{repo}/blobs/uploads` | `Foreign:oci.rs` | **—** |
| PATCH | `/v2/{owner}/{repo}/blobs/uploads/{uuid}` | `Foreign:oci.rs` | rust |
| GET | `/v2/{owner}/{repo}/blobs/uploads/{uuid}` | `Foreign:oci.rs` | rust+smoke |
| PUT | `/v2/{owner}/{repo}/blobs/uploads/{uuid}` | `Foreign:oci.rs` | rust |
| GET | `/api-docs/openapi.json` | `User` | rust+smoke |
| GET | `/api-docs` | `User` | **—** |
| GET | `/api-docs` | `User` | **—** |
| GET | `/api-docs/{*tail}` | `User` | rust+smoke |
| GET | `/api/v1/repos/{owner}/{name}/packages/cargo/index/config.json` | `RepoRead` | rust |
| PUT | `/api/v1/repos/{owner}/{name}/packages/cargo/api/v1/crates/new` | `RepoWrite` | **—** |
| DELETE | `/api/v1/repos/{owner}/{name}/packages/cargo/api/v1/crates/{crate_name}/{version}/yank` | `RepoWrite` | **—** |
| PUT | `/api/v1/repos/{owner}/{name}/packages/cargo/api/v1/crates/{crate_name}/{version}/unyank` | `RepoWrite` | **—** |
| GET | `/api/v1/repos/{owner}/{name}/packages/rubygems/versions` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/rubygems/info/{gem_name}` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/rubygems/names` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/rubygems/gems/{filename}` | `RepoRead` | rust |
| POST | `/api/v1/repos/{owner}/{name}/packages/rubygems/api/v1/gems` | `RepoWrite` | **—** |
| GET | `/git/{owner}/{repo}/info/refs` | `Foreign:git_http.rs` | rust |
| POST | `/git/{owner}/{repo}/git-upload-pack` | `Foreign:git_http.rs` | **—** |
| POST | `/git/{owner}/{repo}/git-receive-pack` | `Foreign:git_http.rs` | **—** |
| GET | `/{owner}/{repo}/info/refs` | `Foreign:git_http.rs` | rust |
| POST | `/{owner}/{repo}/git-upload-pack` | `Foreign:git_http.rs` | **—** |
| POST | `/{owner}/{repo}/git-receive-pack` | `Foreign:git_http.rs` | **—** |
| GET | `/health` | `Public` | rust+smoke |
| GET | `/metrics` | `Public` | rust+smoke |
| POST | `/api/v1/users/register` | `Public` | rust+smoke |
| POST | `/api/v1/users/login` | `Public` | rust+smoke |
| POST | `/api/v1/users/logout` | `User` | rust |
| GET | `/api/v1/users/me` | `User` | rust+smoke |
| POST | `/api/v1/users/mfa/verify` | `Public` | rust |
| POST | `/api/v1/users/passkeys/register/start` | `User` | rust |
| POST | `/api/v1/users/passkeys/register/finish` | `User` | rust |
| POST | `/api/v1/users/passkeys/login/start` | `Public` | rust |
| POST | `/api/v1/users/passkeys/login/finish` | `Public` | rust |
| GET | `/api/v1/auth/sso/{slug}` | `Public` | rust+smoke |
| GET | `/api/v1/auth/sso/{slug}/callback` | `Public` | rust |
| GET | `/api/v1/repos/{owner}/{name}/labels/{id}` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/issue_config/validate` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/issues/{number}/labels` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/issues/{number}/assets` | `RepoRead` | rust |
| POST | `/api/v1/repos/{owner}/{name}/issues/{number}/assets` | `RepoWrite` | rust |
| GET | `/api/v1/repos/{owner}/{name}/issues/{number}/assets/{attachment_id}` | `RepoRead` | rust |
| DELETE | `/api/v1/repos/{owner}/{name}/issues/{number}/assets/{attachment_id}` | `RepoWrite` | rust |
| GET | `/api/v1/repos/{owner}/{name}/issues/comments/{comment_id}/assets` | `RepoRead` | **—** |
| POST | `/api/v1/repos/{owner}/{name}/issues/comments/{comment_id}/assets` | `RepoWrite` | rust |
| GET | `/api/v1/repos/{owner}/{name}/issues/comments/{comment_id}/assets/{attachment_id}` | `RepoRead` | rust |
| DELETE | `/api/v1/repos/{owner}/{name}/issues/comments/{comment_id}/assets/{attachment_id}` | `RepoWrite` | **—** |
| GET | `/api/v1/repos/{owner}/{name}/pulls/{number}/assets` | `RepoRead` | **—** |
| POST | `/api/v1/repos/{owner}/{name}/pulls/{number}/assets` | `RepoWrite` | rust |
| GET | `/api/v1/repos/{owner}/{name}/pulls/{number}/assets/{attachment_id}` | `RepoRead` | rust |
| DELETE | `/api/v1/repos/{owner}/{name}/pulls/{number}/assets/{attachment_id}` | `RepoWrite` | **—** |
| GET | `/api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets` | `RepoRead` | **—** |
| POST | `/api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets` | `RepoWrite` | rust |
| GET | `/api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets/{attachment_id}` | `RepoRead` | rust |
| DELETE | `/api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets/{attachment_id}` | `RepoWrite` | rust |
| GET | `/api/v1/repos/{owner}/{name}/pulls/{number}/reviews/{id}` | `RepoRead` | rust |
| POST | `/api/v1/repos/{owner}/{name}/lfs/objects/batch` | `Foreign:api/lfs.rs` | rust |
| GET | `/api/v1/repos/{owner}/{name}/lfs/objects/{oid}` | `Foreign:api/lfs.rs` | rust+smoke |
| PUT | `/api/v1/repos/{owner}/{name}/lfs/objects/{oid}` | `Foreign:api/lfs.rs` | rust |
| GET | `/api/v1/ci/oidc/.well-known/openid-configuration` | `Public` | rust |
| GET | `/api/v1/ci/oidc/jwks` | `Public` | rust |
| GET | `/api/v1/ci/oidc/token` | `Foreign:api/ci_oidc.rs` | rust |
| GET | `/api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/branches/protection/{id}` | `RepoRead` | rust |
| GET | `/api/v1/imports/{id}` | `User` | rust |
| POST | `/api/v1/repos/{owner}/{name}/boards/{id}/columns` | `RepoWrite` | rust |
| PATCH | `/api/v1/repos/{owner}/{name}/boards/{id}/columns/{col_id}` | `RepoWrite` | **—** |
| POST | `/api/v1/repos/{owner}/{name}/statuses/{sha}` | `RepoWrite` | rust |
| POST | `/api/v1/orgs` | `User` | rust+smoke |
| GET | `/api/v1/orgs/{name}/teams/{team_id}` | `OrgRead` | rust |
| DELETE | `/api/v1/notifications/{id}` | `User` | rust |
| GET | `/api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust |
| POST | `/api/v1/repos/{owner}/{name}/releases/{release_id}/assets` | `RepoWrite` | rust |
| GET | `/api/v1/repos/{owner}/{name}/releases/assets/{asset_id}` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/download` | `RepoRead` | rust |
| POST | `/api/v1/repos/{owner}/{name}/packages/npm/publish` | `RepoWrite` | **—** |
| GET | `/api/v1/repos/{owner}/{name}/packages/npm/list` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/npm/-/npm/v1/attestations/{package_spec}` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/npm/{pkg_name}` | `RepoRead` | rust+smoke |
| PUT | `/api/v1/repos/{owner}/{name}/packages/npm/{pkg_name}` | `RepoWrite` | **—** |
| GET | `/api/v1/repos/{owner}/{name}/packages/npm/-/package/{pkg_name}/dist-tags` | `RepoRead` | rust |
| PUT | `/api/v1/repos/{owner}/{name}/packages/npm/-/package/{pkg_name}/dist-tags/{tag}` | `RepoWrite` | **—** |
| DELETE | `/api/v1/repos/{owner}/{name}/packages/npm/-/package/{pkg_name}/dist-tags/{tag}` | `RepoWrite` | **—** |
| POST | `/api/v1/repos/{owner}/{name}/packages/pypi/legacy` | `RepoWrite` | **—** |
| POST | `/api/v1/repos/{owner}/{name}/packages/pypi/legacy` | `RepoWrite` | **—** |
| GET | `/api/v1/repos/{owner}/{name}/packages/pypi/simple` | `RepoRead` | **—** |
| GET | `/api/v1/repos/{owner}/{name}/packages/pypi/simple` | `RepoRead` | **—** |
| GET | `/api/v1/repos/{owner}/{name}/packages/pypi/simple/{pkg_name}` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/pypi/simple/{pkg_name}` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/nuget/index.json` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/nuget/registration/{id}/index.json` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/nuget/registration/{id}/{version}` | `RepoRead` | rust |
| HEAD | `/api/v1/repos/{owner}/{name}/packages/nuget/registration/{id}/{version}` | `RepoRead` | **—** |
| GET | `/api/v1/repos/{owner}/{name}/packages/nuget/query` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/nuget/autocomplete` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/nuget/package/{id}/index.json` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/nuget/package/{id}/{version}/{file}` | `RepoRead` | rust |
| POST | `/api/v1/repos/{owner}/{name}/packages/nuget/publish` | `RepoWrite` | **—** |
| PUT | `/api/v1/repos/{owner}/{name}/packages/nuget/publish` | `RepoWrite` | **—** |
| GET | `/api/v1/repos/{owner}/{name}/packages/rubygems/api/v1/dependencies.json` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/rubygems/api/v1/gems/{gem_name}` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/helm/index.yaml` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/composer/packages.json` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}` | `RepoRead` | rust |
| PATCH | `/api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/yank` | `RepoWrite` | rust+web |
| GET | `/api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/{*file}` | `RepoRead` | rust+web+smoke |
| POST | `/api/v1/runners/{id}/heartbeat` | `Foreign:RUNNER_AUTH_LAYER` | rust |
| POST | `/api/v1/runners/{id}/deregister` | `Foreign:RUNNER_AUTH_LAYER` | rust |
| GET | `/api/v1/runners/{id}/jobs/poll` | `Foreign:RUNNER_AUTH_LAYER` | rust |
| POST | `/api/v1/runners/{id}/jobs/{job_id}/start` | `Foreign:RUNNER_AUTH_LAYER` | rust |
| POST | `/api/v1/runners/{id}/jobs/{job_id}/log` | `Foreign:RUNNER_AUTH_LAYER` | **—** |
| GET | `/api/v1/runners/{id}/jobs/{job_id}/workspace` | `Foreign:RUNNER_AUTH_LAYER` | rust |
| GET | `/api/v1/runners/{id}/jobs/{job_id}/cache` | `Foreign:RUNNER_AUTH_LAYER` | rust+smoke |
| PUT | `/api/v1/runners/{id}/jobs/{job_id}/cache` | `Foreign:RUNNER_AUTH_LAYER` | rust |
| POST | `/api/v1/runners/{id}/jobs/{job_id}/finish` | `Foreign:RUNNER_AUTH_LAYER` | rust |
| PUT | `/api/v1/runners/{id}/jobs/{job_id}/artifacts/staging` | `Foreign:RUNNER_AUTH_LAYER` | **—** |
| POST | `/api/v1/runners/{id}/jobs/{job_id}/artifacts` | `Foreign:RUNNER_AUTH_LAYER` | rust |
| GET | `/api/v1/artifacts/{id}` | `RepoRead` | rust |
| GET | `/api/v1/artifacts/{id}/download` | `RepoRead` | rust+web |
| GET | `/api/v1/admin/runners/{id}` | `InstanceAdmin` | rust |
| GET | `/api/v1/admin/users/{id}` | `InstanceAdmin` | rust+web+smoke |
| GET | `/api/v1/admin/orgs/{name}` | `InstanceAdmin` | rust |
| GET | `/api/v1/admin/sso/providers/{id}` | `InstanceAdmin` | rust |
| POST | `/api/v1/repos/{owner}/{name}/webhooks/external/ci` | `RepoWrite` | rust |
| GET | `/api/v1/ai/repos/{owner}/{name}/summary` | `RepoRead` | rust |
| GET | `/api/v1/ai/repos/{owner}/{name}/issues` | `RepoRead` | rust |
| GET | `/api/v1/ai/repos/{owner}/{name}/prs` | `RepoRead` | rust |
| GET | `/api/v1/ai/repos/{owner}/{name}/tree` | `RepoRead` | rust |
| GET | `/api/v1/ai/repos/{owner}/{name}/search/code` | `RepoRead` | rust |
| POST | `/api/v1/ai/repos/{owner}/{name}/index` | `RepoWrite` | rust |
| GET | `/api/v1/ws/notifications` | `Foreign:ws.rs` | rust |
| GET | `/api/v1/ws/job/{job_id}` | `Foreign:ws.rs` | rust |

