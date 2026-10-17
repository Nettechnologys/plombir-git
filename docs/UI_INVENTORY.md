# Plombir Git — UI Inventory (generated)

> **Generated file.** Regenerate instead of editing by hand:
> `node scripts/ui-inventory.mjs`
>
> Built from the router (`crates/rg-http/src/routes.rs`), API client
> (`web/src/lib/api`), pages (`web/src/routes`), and shared components
> (`web/src/lib/components`). See also `docs/FEATURE_INVENTORY.md`.
>
> **Coverage meaning.** `rust` and `smoke` only prove that executable
> tests name the route method and full URL. Comments and another method
> on the same URL do not count. `web` requires an executable API client
> call or a URL next to its transport (`request`, `downloadApiFile`,
> `fetch`, or `WebSocket`); navigation alone does not count. `browser`
> names a live control or passive call and checks its actual response
> against `Access` for both an owner and an outsider.

## Summary

| | |
|---|---|
| Router routes (with declared `Access`) | 436 |
| Reachable from the browser | 308 (71%) |
| Layout modules | 2 |
| Pages | 73 |
| Interactive controls | 1052 |
| Controls calling an API | 371 |
| Controls from shared components | 425 |
| Browser sweep: scenarios / inventory entries / routes | 52 / 196 / 186 |
| **UI routes without a web/smoke/browser test** | **24** |
| UI routes without corpus or browser coverage | 0 |

## By access level

| `Access` | routes | reachable from UI | no frontend test | no corpus/browser coverage |
|---|---:|---:|---:|---:|
| `RepoRead` | 109 | 70 | 4 | 0 |
| `RepoWrite` | 81 | 62 | 12 | 2 |
| `User` | 54 | 44 | 5 | 0 |
| `RepoAdmin` | 49 | 49 | 0 | 0 |
| `InstanceAdmin` | 33 | 28 | 0 | 1 |
| `Public` | 25 | 10 | 2 | 0 |
| `RepoAuthRead` | 18 | 18 | 1 | 0 |
| `OrgAdmin` | 15 | 14 | 0 | 0 |
| `Foreign:oci.rs` | 14 | 0 | 0 | 0 |
| `Foreign:RUNNER_AUTH_LAYER` | 12 | 0 | 0 | 0 |
| `Foreign:git_http.rs` | 6 | 0 | 0 | 0 |
| `OrgRead` | 5 | 4 | 0 | 0 |
| `Foreign:api/lfs_locks.rs` | 4 | 2 | 0 | 1 |
| `PublicFiltered` | 3 | 3 | 0 | 0 |
| `Foreign:api/lfs.rs` | 3 | 0 | 0 | 0 |
| `RepoOwner` | 2 | 2 | 0 | 0 |
| `Foreign:ws.rs` | 2 | 2 | 0 | 0 |
| `Foreign:api/ci_oidc.rs` | 1 | 0 | 0 | 0 |

## Pages

| Page | controls | call an API | from components |
|---|---:|---:|---:|
| `/[owner]/[repo]/pulls/[number]` | 71 | 36 | 25 |
| `/[owner]/[repo]/boards` | 42 | 14 | 15 |
| `/[owner]/[repo]/issues/[number]` | 40 | 17 | 23 |
| `/[owner]/[repo]/pulls` | 28 | 12 | 12 |
| `/[owner]/[repo]/releases` | 27 | 12 | 11 |
| `/orgs/[name]` | 27 | 10 | 2 |
| `/[owner]/[repo]/issues` | 26 | 9 | 12 |
| `/[owner]/[repo]` | 25 | 5 | 12 |
| `/[owner]/[repo]/blob/[...path]` | 25 | 8 | 11 |
| `/[owner]/[repo]/pipelines` | 25 | 11 | 14 |
| `/admin/users` 🔒 | 25 | 8 | 4 |
| `/[owner]/[repo]/wiki/[title]` | 24 | 9 | 13 |
| `/[owner]/[repo]/packages` | 23 | 11 | 12 |
| `/[owner]/[repo]/branches` | 22 | 6 | 12 |
| `/[owner]/[repo]/milestones` | 22 | 8 | 13 |
| `/[owner]/[repo]/releases/new` | 21 | 5 | 11 |
| `/[owner]/[repo]/settings` | 20 | 5 | 4 |
| `/[owner]/[repo]/time_tracking` | 20 | 10 | 13 |
| `/[owner]/[repo]/network` | 19 | 6 | 11 |
| `/admin/settings` 🔒 | 19 | 9 | 2 |
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
| `/admin/hooks` 🔒 | 12 | 6 | 12 |
| `/login` | 12 | 1 | 0 |
| `/orgs/[name]/hooks` | 12 | 6 | 12 |
| `/admin/runners` 🔒 | 11 | 4 | 1 |
| `/search` | 11 | 0 | 0 |
| `/[owner]/[repo]/settings/labels` | 10 | 2 | 2 |
| `/[owner]/[repo]/settings/push-mirrors` | 9 | 4 | 2 |
| `/admin/audit` 🔒 | 9 | 6 | 1 |
| `/[owner]/[repo]/edit/[...path]` | 8 | 0 | 8 |
| `/[owner]/[repo]/new` | 8 | 0 | 8 |
| `/[owner]/[repo]/settings/collaborators` | 8 | 3 | 2 |
| `/[owner]/[repo]/settings/environments` | 8 | 2 | 2 |
| `/admin/orgs` 🔒 | 8 | 3 | 1 |
| `/orgs` | 8 | 1 | 0 |
| `/` | 7 | 1 | 0 |
| `/[owner]/[repo]/settings/deploy-keys` | 7 | 2 | 2 |
| `/[owner]/[repo]/settings/lfs-storage` | 7 | 4 | 2 |
| `/[owner]/[repo]/settings/mirror` | 7 | 3 | 2 |
| `/[owner]/[repo]/settings/tags` | 7 | 2 | 2 |
| `/settings/signing-keys` | 7 | 2 | 2 |
| `/[owner]/[repo]/settings/ci-secrets` | 6 | 2 | 2 |
| `/admin` 🔒 | 6 | 0 | 0 |
| `/settings/ssh-keys` | 6 | 2 | 2 |
| `/settings/tokens` | 6 | 2 | 2 |
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

## Test plan: control → route → access level

Each row is an end-to-end scenario. `Access` defines which actor may
succeed and which must be denied.

### Global layout requests

| Scope | Source | Call | `Access` | test |
|---|---|---|---|---|
| `/` | `instance.get` | `GET /api/v1/instance` | `Public` | rust+web |
| `/` | `web/src/routes/+layout.svelte#checkBackendReadiness` | `GET /health` | `Public` | rust+smoke |

### `/`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:common.retry | :156 | `GET /api/v1/repos/explore` | `PublicFiltered` | rust+web+smoke |

### `/[owner]`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| _(page load)_ | — | `GET /api/v1/repos/{owner}` | `PublicFiltered` | rust+web |
| _(page load)_ | — | `GET /api/v1/orgs/{name}` | `OrgRead` | rust+web+smoke |
| _(page load)_ | — | `GET /api/v1/orgs` | `User` | rust+web+smoke |

### `/[owner]/[repo]`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:common.retry | :441 | `GET /api/v1/repos/{owner}/{name}/blob/{*path}` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke+browser |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web+browser |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web+browser |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke+browser |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/tree` | `RepoRead` | rust+web+browser |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | web+browser |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/log` | `RepoRead` | rust+web+browser |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web+browser |

### `/[owner]/[repo]/blob/[...path]`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:repo.blob.hide_blame | :535 | `GET /api/v1/repos/{owner}/{name}/blame/{*path}` | `RepoRead` | web |
| i18n:repo.blob.download | :555 | `GET /api/v1/repos/{owner}/{name}/raw/{*path}` | `RepoRead` | web |
| i18n:repo.blob.deleting | :612 | `DELETE /api/v1/repos/{owner}/{name}/contents/{*path}` | `RepoWrite` | rust+web+browser |
| i18n:repo.blob.download | :633 | `GET /api/v1/repos/{owner}/{name}/raw/{*path}` | `RepoRead` | web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/blob/{*path}` | `RepoRead` | rust+web+browser |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/raw/{*path}` | `RepoRead` | web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/boards`

| Control | Source | Call | `Access` | test |
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
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/boards` | `RepoRead` | rust+web+browser |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/issues` | `RepoRead` | rust+web+browser |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/branches`

| Control | Source | Call | `Access` | test |
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
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/commits`

| Control | Source | Call | `Access` | test |
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
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/commits/[sha]`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:common.retry | :177 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/status` | `RepoRead` | rust+web+browser |
| i18n:common.retry | :177 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/statuses` | `RepoRead` | rust+web+browser |
| i18n:common.retry | :177 | `GET /api/v1/repos/{owner}/{name}/log` | `RepoRead` | rust+web |
| i18n:common.retry | :177 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/signature` | `RepoRead` | rust+web+browser |
| i18n:common.retry | :231 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/status` | `RepoRead` | rust+web |
| i18n:common.retry | :231 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/statuses` | `RepoRead` | rust+web |
| i18n:common.retry | :231 | `GET /api/v1/repos/{owner}/{name}/log` | `RepoRead` | rust+web |
| i18n:common.retry | :231 | `GET /api/v1/repos/{owner}/{name}/commits/{sha}/signature` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/compare/[...spec]`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/compare` | `RepoRead` | rust+web |

### `/[owner]/[repo]/edit/[...path]`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/blob/{*path}` | `RepoRead` | rust+web |
| _(page load)_ | — | `POST /api/v1/repos/{owner}/{name}/contents/{*path}` | `RepoWrite` | rust+web |

### `/[owner]/[repo]/issues`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:issues.tabs.open | :210 | `GET /api/v1/repos/{owner}/{name}/issues` | `RepoRead` | rust+web |
| i18n:issues.tabs.closed | :217 | `GET /api/v1/repos/{owner}/{name}/issues` | `RepoRead` | rust+web |
| i18n:issues.tabs.all | :224 | `GET /api/v1/repos/{owner}/{name}/issues` | `RepoRead` | rust+web |
| i18n:issues.new | :232 | `GET /api/v1/repos/{owner}/{name}/issue_templates` | `RepoRead` | rust+web+browser |
| i18n:issues.new | :232 | `GET /api/v1/repos/{owner}/{name}/issue_config` | `RepoRead` | rust+web+browser |
| toggleLabel(label.name, event.currentTarget.checked)} /> }> | :280 | `POST /api/v1/repos/{owner}/{name}/issues` | `RepoAuthRead` | rust+web+browser |
| toggleLabel(label.name, event.currentTarget.checked)} /> }> | :280 | `GET /api/v1/repos/{owner}/{name}/issues` | `RepoRead` | rust+web+browser |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/labels` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/issues/[number]`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:common.loading | :401 | `PATCH /api/v1/repos/{owner}/{name}/issues/{number}` | `RepoWrite` | rust+web |
| i18n:common.saving | :443 | `PATCH /api/v1/repos/{owner}/{name}/issues/comments/{comment_id}` | `RepoAdmin` | web+browser |
| i18n:issues.comment_placeholder | :460 | `POST /api/v1/repos/{owner}/{name}/issues/{number}/comments` | `RepoAuthRead` | rust+browser |
| i18n:issues.comment_placeholder | :460 | `GET /api/v1/repos/{owner}/{name}/issues/{number}` | `RepoRead` | rust+web+browser |
| i18n:issues.comment_placeholder | :460 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/comments` | `RepoRead` | rust+web+browser |
| i18n:issues.comment_placeholder | :460 | `GET /api/v1/repos/{owner}/{name}/milestones` | `RepoRead` | rust+web+browser |
| i18n:issues.comment_placeholder | :460 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust+web+browser |
| i18n:issues.comment_placeholder | :460 | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
| i18n:issues.close_issue | :465 | `PATCH /api/v1/repos/{owner}/{name}/issues/{number}` | `RepoWrite` | rust+web+browser |
| i18n:issues.close_issue | :465 | `GET /api/v1/repos/{owner}/{name}/issues/{number}` | `RepoRead` | rust+web |
| i18n:issues.close_issue | :465 | `GET /api/v1/repos/{owner}/{name}/issues/{number}/comments` | `RepoRead` | rust+web |
| i18n:issues.close_issue | :465 | `GET /api/v1/repos/{owner}/{name}/milestones` | `RepoRead` | rust+web |
| i18n:issues.close_issue | :465 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust+web |
| i18n:issues.close_issue | :465 | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
| i18n:common.deleting | :494 | `DELETE /api/v1/repos/{owner}/{name}/issues/comments/{comment_id}` | `RepoAdmin` | rust+web+browser |
| i18n:common.deleting | :510 | `DELETE /api/v1/repos/{owner}/{name}/issues/{number}` | `RepoAdmin` | web+browser |
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
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/issues/{number}/subscription` | `RepoAuthRead` | web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/subscription` | `RepoAuthRead` | web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/issues/{number}/assets` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/issues/comments/{comment_id}/assets` | `RepoRead` | rust+web |

### `/[owner]/[repo]/milestones`

| Control | Source | Call | `Access` | test |
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
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/network`

| Control | Source | Call | `Access` | test |
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
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/new`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
| _(page load)_ | — | `POST /api/v1/repos/{owner}/{name}/contents/{*path}` | `RepoWrite` | rust+web+browser |

### `/[owner]/[repo]/packages`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:common.next | :248 | `GET /api/v1/repos/{owner}/{name}/packages/oci/images` | `RepoRead` | rust+web |
| i18n:common.delete | :262 | `DELETE /api/v1/repos/{owner}/{name}/packages/oci/images/{digest}` | `RepoWrite` | rust+web |
| i18n:common.delete | :262 | `GET /api/v1/repos/{owner}/{name}/packages/oci/images` | `RepoRead` | rust+web |
| i18n:common.retry | :274 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust+web |
| i18n:common.retry | :274 | `GET /api/v1/repos/{owner}/{name}/packages` | `RepoRead` | rust+web |
| i18n:common.all | :283 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust+web |
| i18n:common.all | :283 | `GET /api/v1/repos/{owner}/{name}/packages` | `RepoRead` | rust+web |
| i18n:common.search | :298 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust+web |
| i18n:common.search | :298 | `GET /api/v1/repos/{owner}/{name}/packages` | `RepoRead` | rust+web |
| i18n:common.previous | :337 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust+web |
| i18n:common.previous | :337 | `GET /api/v1/repos/{owner}/{name}/packages` | `RepoRead` | rust+web |
| i18n:common.next | :345 | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust+web |
| i18n:common.next | :345 | `GET /api/v1/repos/{owner}/{name}/packages` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/packages/[format]`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/list` | `RepoRead` | rust+web+browser |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/packages/[format]/[...name]`

| Control | Source | Call | `Access` | test |
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
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/{*file}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/packages/upload`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:packages.format | :141 | `POST /api/v1/repos/{owner}/{name}/packages/{pkg_type}/publish` | `RepoWrite` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/pipelines`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| updateTriggerRef(event.currentTarget.value)} onchange= place | :799 | `POST /api/v1/repos/{owner}/{name}/pipelines` | `RepoWrite` | rust+web+browser |
| updateTriggerRef(event.currentTarget.value)} onchange= place | :799 | `GET /api/v1/repos/{owner}/{name}/pipelines` | `RepoRead` | rust+web+browser |
| updateTriggerRef(event.currentTarget.value)} onchange= place | :799 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}` | `RepoRead` | rust+web+browser |
| updateTriggerRef(event.currentTarget.value)} onchange= place | :799 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}/artifacts` | `RepoRead` | rust+web+browser |
| i18n:pipeline.retry | :928 | `POST /api/v1/repos/{owner}/{name}/pipelines/{id}/retry` | `RepoWrite` | rust+web |
| i18n:pipeline.retry | :928 | `GET /api/v1/repos/{owner}/{name}/pipelines` | `RepoRead` | rust+web |
| i18n:pipeline.retry | :928 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}` | `RepoRead` | rust+web |
| i18n:pipeline.retry | :928 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}/artifacts` | `RepoRead` | rust+web |
| i18n:pipeline.cancel | :931 | `POST /api/v1/repos/{owner}/{name}/pipelines/{id}/cancel` | `RepoWrite` | rust |
| i18n:pipeline.play_manual | :991 | `POST /api/v1/repos/{owner}/{name}/pipelines/{id}/jobs/{job_id}/play` | `RepoWrite` | rust |
| i18n:pipeline.play_manual | :991 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}` | `RepoRead` | rust+web |
| i18n:pipeline.play_manual | :991 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}/artifacts` | `RepoRead` | rust+web |
| i18n:pipeline.approval_recorded | :994 | `POST /api/v1/repos/{owner}/{name}/pipelines/{pipeline_id}/jobs/{job_id}/approve` | `RepoAuthRead` | rust |
| i18n:pipeline.approval_recorded | :994 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}` | `RepoRead` | rust+web |
| i18n:pipeline.approval_recorded | :994 | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}/artifacts` | `RepoRead` | rust+web |
| i18n:pipeline.artifact_downloading | :1030 | `GET /api/v1/artifacts/{id}/download` | `RepoRead` | rust+web |
| i18n:pipeline.artifact_deleting | :1036 | `DELETE /api/v1/artifacts/{id}` | `RepoWrite` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/pipelines/workflow-dispatch` | `RepoRead` | rust+web+browser |
| _(page load)_ | — | `GET /api/v1/ws/job/{job_id}` | `Foreign:ws.rs` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/pipelines/{id}/jobs/{job_id}/log` | `RepoRead` | rust+web+browser |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/pulls`

| Control | Source | Call | `Access` | test |
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
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/forks` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/pulls/[number]`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:common.retry | :675 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:common.retry | :675 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:common.retry | :675 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:common.retry | :675 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:common.retry | :675 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:common.retry | :675 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:common.retry | :675 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.mark_ready | :696 | `PATCH /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoWrite` | rust+web+browser |
| × | :741 | `DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers/{username}` | `RepoWrite` | rust+browser |
| i18n:pulls.reviewers.request | :754 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoWrite` | rust+browser |
| i18n:pulls.reviewers.request | :754 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :769 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/ci-approval` | `RepoWrite` | web |
| i18n:pulls.fork_ci.approving | :769 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :769 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :769 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :769 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :769 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :769 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.fork_ci.approving | :769 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :801 | `DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/merge-queue` | `RepoWrite` | browser |
| i18n:pulls.merge.leave_queue | :801 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :801 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :801 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :801 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :801 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :801 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.merge.leave_queue | :801 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.merge.disable_auto | :814 | `DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/auto-merge` | `RepoWrite` | browser |
| i18n:pulls.merge.merging | :826 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/merge` | `RepoWrite` | rust+web |
| i18n:pulls.merge.merging | :826 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.merge.merging | :826 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.merge.merging | :826 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.merge.merging | :826 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.merge.merging | :826 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.merge.merging | :826 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.merge.merging | :826 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :829 | `PUT /api/v1/repos/{owner}/{name}/pulls/{number}/auto-merge` | `RepoWrite` | rust+browser |
| i18n:pulls.merge.enabling_auto | :829 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :829 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :829 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :829 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :829 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :829 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.merge.enabling_auto | :829 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :832 | `PUT /api/v1/repos/{owner}/{name}/pulls/{number}/merge-queue` | `RepoWrite` | rust+browser |
| i18n:pulls.merge.joining_queue | :832 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :832 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :832 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :832 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :832 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :832 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.merge.joining_queue | :832 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :884 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/reviews/{id}/dismiss` | `RepoWrite` | rust+web |
| i18n:pulls.review.dismissing | :884 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :884 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :884 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :884 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :884 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :884 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.review.dismissing | :884 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :908 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/suggestions/apply` | `RepoWrite` | rust |
| i18n:pulls.suggestion.applying_selected | :908 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :908 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :908 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :908 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :908 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :908 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.applying_selected | :908 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :950 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}/suggestion/apply` | `RepoWrite` | rust |
| i18n:pulls.suggestion.apply | :950 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :950 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :950 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :950 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :950 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :950 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :950 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.threads.reopen | :962 | `PATCH /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}/resolution` | `RepoWrite` | **—** |
| i18n:pulls.threads.reopen | :962 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :1020 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}/suggestion/apply` | `RepoWrite` | rust |
| i18n:pulls.suggestion.apply | :1020 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :1020 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :1020 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :1020 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :1020 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :1020 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:pulls.suggestion.apply | :1020 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.threads.reopen | :1030 | `PATCH /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}/resolution` | `RepoWrite` | browser |
| i18n:pulls.threads.reopen | :1030 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:pulls.diff.submit_comment | :1050 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoAuthRead` | rust+browser |
| i18n:pulls.diff.submit_comment | :1050 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:common.retry | :1062 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web |
| i18n:common.retry | :1062 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web |
| i18n:common.retry | :1062 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web |
| i18n:common.retry | :1062 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web |
| i18n:common.retry | :1062 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web |
| i18n:common.retry | :1062 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web |
| i18n:common.retry | :1062 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web |
| i18n:pulls.review.submit | :1089 | `POST /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoAuthRead` | rust+browser |
| i18n:pulls.review.submit | :1089 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoRead` | rust+web+browser |
| i18n:pulls.review.submit | :1089 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/diff` | `RepoRead` | rust+web+browser |
| i18n:pulls.review.submit | :1089 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviews` | `RepoRead` | rust+web+browser |
| i18n:pulls.review.submit | :1089 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/comments` | `RepoRead` | rust+web+browser |
| i18n:pulls.review.submit | :1089 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/timeline` | `RepoRead` | rust+web+browser |
| i18n:pulls.review.submit | :1089 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/reviewers` | `RepoRead` | rust+web+browser |
| i18n:pulls.review.submit | :1089 | `GET /api/v1/repos/{owner}/{name}/merge-queue` | `RepoRead` | rust+web+browser |
| i18n:common.saving | :1116 | `PATCH /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}` | `RepoAdmin` | web+browser |
| i18n:common.deleting | :1144 | `DELETE /api/v1/repos/{owner}/{name}/pulls/{number}/comments/{id}` | `RepoAdmin` | web+browser |
| i18n:common.deleting | :1160 | `DELETE /api/v1/repos/{owner}/{name}/pulls/{number}` | `RepoAdmin` | web+browser |
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
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/assets` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/issues/{number}/subscription` | `RepoAuthRead` | web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/pulls/{number}/subscription` | `RepoAuthRead` | web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/pulls/comments/{comment_id}/assets` | `RepoRead` | rust+web |

### `/[owner]/[repo]/releases`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| (event) => handleAssetUpload(release.id, | :550 | `POST /api/v1/repos/{owner}/{name}/releases/{release_id}/assets` | `RepoWrite` | rust+web |
| · )} | :570 | `GET /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/download` | `RepoRead` | rust+web |
| i18n:releases.attestation.verifying | :612 | `POST /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation/verify` | `RepoRead` | rust+web |
| i18n:releases.attestation.signing | :623 | `POST /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation` | `RepoWrite` | rust+web |
| i18n:common.delete | :649 | `DELETE /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}` | `RepoWrite` | rust+web+browser |
| i18n:common.delete | :686 | `DELETE /api/v1/repos/{owner}/{name}/releases/{id}` | `RepoWrite` | rust+web |
| i18n:common.delete | :686 | `GET /api/v1/instance` | `Public` | rust+web+browser |
| i18n:common.delete | :686 | `GET /api/v1/repos/{owner}/{name}/releases` | `RepoRead` | rust+web |
| i18n:common.delete | :686 | `GET /api/v1/repos/{owner}/{name}/releases/{id}` | `RepoRead` | rust+web |
| i18n:common.delete | :686 | `GET /api/v1/repos/{owner}/{name}/releases/{release_id}/assets` | `RepoRead` | rust+web |
| i18n:common.delete | :686 | `GET /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation` | `RepoRead` | rust+web |
| i18n:common.previous | :701 | `GET /api/v1/instance` | `Public` | rust+web |
| i18n:common.previous | :701 | `GET /api/v1/repos/{owner}/{name}/releases` | `RepoRead` | rust+web |
| i18n:common.previous | :701 | `GET /api/v1/repos/{owner}/{name}/releases/{id}` | `RepoRead` | rust+web |
| i18n:common.previous | :701 | `GET /api/v1/repos/{owner}/{name}/releases/{release_id}/assets` | `RepoRead` | rust+web |
| i18n:common.previous | :701 | `GET /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation` | `RepoRead` | rust+web |
| i18n:common.next | :709 | `GET /api/v1/instance` | `Public` | rust+web |
| i18n:common.next | :709 | `GET /api/v1/repos/{owner}/{name}/releases` | `RepoRead` | rust+web |
| i18n:common.next | :709 | `GET /api/v1/repos/{owner}/{name}/releases/{id}` | `RepoRead` | rust+web |
| i18n:common.next | :709 | `GET /api/v1/repos/{owner}/{name}/releases/{release_id}/assets` | `RepoRead` | rust+web |
| i18n:common.next | :709 | `GET /api/v1/repos/{owner}/{name}/releases/assets/{asset_id}/attestation` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/releases/edit/[id]`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| * | :205 | `PATCH /api/v1/repos/{owner}/{name}/releases/{id}` | `RepoWrite` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/releases/{id}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/releases/new`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| * tagName = tag} > )} * selectedTargetType = 'tag'} > select | :155 | `POST /api/v1/repos/{owner}/{name}/releases` | `RepoWrite` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/tags` | `RepoRead` | web+browser |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/settings`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:settings.repository_info.description | :342 | `PATCH /api/v1/repos/{owner}/{name}` | `RepoAdmin` | rust+web+browser |
| i18n:settings.transfer.confirm | :514 | `POST /api/v1/repos/{owner}/{name}/transfer` | `RepoOwner` | rust+web+browser |
| i18n:settings.delete.confirm_button | :528 | `DELETE /api/v1/repos/{owner}/{name}` | `RepoOwner` | rust+web |
| i18n:common.saving | :546 | `PATCH /api/v1/repos/{owner}/{name}` | `RepoAdmin` | rust+web |
| i18n:settings.rename.renaming | :563 | `PATCH /api/v1/repos/{owner}/{name}` | `RepoAdmin` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/branches` | `RepoRead` | web |

### `/[owner]/[repo]/settings/branches`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| )} removeRequiredStatusCheck(index)} disabled= aria-label= ) | :254 | `PATCH /api/v1/repos/{owner}/{name}/branches/protection/{id}` | `RepoAdmin` | rust+web+browser |
| )} removeRequiredStatusCheck(index)} disabled= aria-label= ) | :254 | `POST /api/v1/repos/{owner}/{name}/branches/protection` | `RepoAdmin` | rust+web+browser |
| )} removeRequiredStatusCheck(index)} disabled= aria-label= ) | :254 | `GET /api/v1/repos/{owner}/{name}/branches/protection` | `RepoRead` | rust+web+browser |
| i18n:common.delete | :403 | `DELETE /api/v1/repos/{owner}/{name}/branches/protection/{id}` | `RepoAdmin` | web+browser |
| i18n:common.delete | :403 | `GET /api/v1/repos/{owner}/{name}/branches/protection` | `RepoRead` | rust+web |

### `/[owner]/[repo]/settings/ci-secrets`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:settings.ci_secrets.name | :111 | `PUT /api/v1/repos/{owner}/{name}/actions/secrets/{secret_name}` | `RepoAdmin` | rust+web+browser |
| i18n:settings.ci_secrets.name | :111 | `GET /api/v1/repos/{owner}/{name}/actions/secrets` | `RepoAdmin` | rust+web+browser |
| i18n:settings.ci_secrets.name | :111 | `GET /api/v1/repos/{owner}/{name}/actions/environments` | `RepoRead` | rust+web |
| i18n:common.delete | :111 | `DELETE /api/v1/repos/{owner}/{name}/actions/secrets/{secret_name}` | `RepoAdmin` | rust+web+browser |
| i18n:common.delete | :111 | `GET /api/v1/repos/{owner}/{name}/actions/secrets` | `RepoAdmin` | rust+web |
| i18n:common.delete | :111 | `GET /api/v1/repos/{owner}/{name}/actions/environments` | `RepoRead` | rust+web |

### `/[owner]/[repo]/settings/collaborators`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:settings.collaborators.user_identifier | :222 | `POST /api/v1/repos/{owner}/{name}/collaborators` | `RepoAdmin` | rust+web+smoke+browser |
| i18n:settings.collaborators.user_identifier | :222 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust+web+browser |
| i18n:common.save | :285 | `PATCH /api/v1/repos/{owner}/{name}/collaborators/{id}` | `RepoAdmin` | rust+web+browser |
| i18n:common.save | :285 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust+web |
| i18n:common.delete | :293 | `DELETE /api/v1/repos/{owner}/{name}/collaborators/{id}` | `RepoAdmin` | rust+web+browser |
| i18n:common.delete | :293 | `GET /api/v1/repos/{owner}/{name}/collaborators` | `RepoRead` | rust+web |

### `/[owner]/[repo]/settings/deploy-keys`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:settings.deploy_keys.name | :174 | `POST /api/v1/repos/{owner}/{name}/keys` | `RepoAdmin` | rust+web+browser |
| i18n:settings.deploy_keys.name | :174 | `GET /api/v1/repos/{owner}/{name}/keys` | `RepoAdmin` | rust+web+browser |
| i18n:common.delete | :199 | `DELETE /api/v1/repos/{owner}/{name}/keys/{id}` | `RepoAdmin` | rust+browser |
| i18n:common.delete | :199 | `GET /api/v1/repos/{owner}/{name}/keys` | `RepoAdmin` | rust+web |

### `/[owner]/[repo]/settings/environments`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:settings.environments.name | :110 | `POST /api/v1/repos/{owner}/{name}/actions/environments` | `RepoAdmin` | rust+web+browser |
| i18n:settings.environments.name | :110 | `PUT /api/v1/repos/{owner}/{name}/actions/environments/{id}` | `RepoAdmin` | web+browser |
| i18n:settings.environments.name | :110 | `GET /api/v1/repos/{owner}/{name}/actions/environments` | `RepoRead` | rust+web+browser |
| i18n:common.delete | :117 | `DELETE /api/v1/repos/{owner}/{name}/actions/environments/{id}` | `RepoAdmin` | rust+web+browser |
| i18n:common.delete | :117 | `GET /api/v1/repos/{owner}/{name}/actions/environments` | `RepoRead` | rust+web |

### `/[owner]/[repo]/settings/labels`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:common.saving | :332 | `PATCH /api/v1/repos/{owner}/{name}/labels/{id}` | `RepoWrite` | rust+web+browser |
| i18n:common.saving | :332 | `POST /api/v1/repos/{owner}/{name}/labels` | `RepoWrite` | rust+web+browser |
| i18n:common.saving | :332 | `GET /api/v1/repos/{owner}/{name}/labels` | `RepoRead` | rust+web+browser |
| i18n:common.deleting | :352 | `DELETE /api/v1/repos/{owner}/{name}/labels/{id}` | `RepoWrite` | rust+web+browser |
| i18n:common.deleting | :352 | `GET /api/v1/repos/{owner}/{name}/labels` | `RepoRead` | rust+web |

### `/[owner]/[repo]/settings/lfs-locks`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:settings.lfs_locks.force_unlock | :129 | `POST /api/v1/repos/{owner}/{name}/lfs/locks/{id}/unlock` | `Foreign:api/lfs_locks.rs` | rust+web |
| i18n:settings.lfs_locks.more | :142 | `GET /api/v1/repos/{owner}/{name}/lfs/locks` | `Foreign:api/lfs_locks.rs` | rust+web+smoke |

### `/[owner]/[repo]/settings/lfs-storage`

| Control | Source | Call | `Access` | test |
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

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:settings.mirror.url | :211 | `PATCH /api/v1/repos/{owner}/{name}/mirror` | `RepoAdmin` | rust+web+browser |
| i18n:settings.mirror.url | :211 | `POST /api/v1/repos/{owner}/{name}/mirror` | `RepoAdmin` | rust+web+browser |
| i18n:common.loading | :252 | `POST /api/v1/repos/{owner}/{name}/mirror/sync` | `RepoAdmin` | rust+browser |
| i18n:common.loading | :252 | `GET /api/v1/repos/{owner}/{name}/mirror` | `RepoAdmin` | rust+web+browser |
| i18n:common.loading | :255 | `DELETE /api/v1/repos/{owner}/{name}/mirror` | `RepoAdmin` | rust+web+browser |

### `/[owner]/[repo]/settings/push-mirrors`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:common.loading | :216 | `POST /api/v1/repos/{owner}/{name}/push_mirrors/{id}/sync` | `RepoAdmin` | web+browser |
| i18n:common.loading | :216 | `GET /api/v1/repos/{owner}/{name}/push_mirrors` | `RepoAdmin` | rust+web |
| i18n:settings.push_mirrors.disable | :219 | `PATCH /api/v1/repos/{owner}/{name}/push_mirrors/{id}` | `RepoAdmin` | rust+web+browser |
| i18n:settings.push_mirrors.disable | :219 | `GET /api/v1/repos/{owner}/{name}/push_mirrors` | `RepoAdmin` | rust+web |
| i18n:settings.push_mirrors.delete | :222 | `DELETE /api/v1/repos/{owner}/{name}/push_mirrors/{id}` | `RepoAdmin` | web+browser |
| i18n:settings.push_mirrors.delete | :222 | `GET /api/v1/repos/{owner}/{name}/push_mirrors` | `RepoAdmin` | rust+web |
| i18n:settings.push_mirrors.url | :257 | `POST /api/v1/repos/{owner}/{name}/push_mirrors` | `RepoAdmin` | rust+web+browser |
| i18n:settings.push_mirrors.url | :257 | `GET /api/v1/repos/{owner}/{name}/push_mirrors` | `RepoAdmin` | rust+web+browser |

### `/[owner]/[repo]/settings/retention`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:settings.retention.artifact_days | :126 | `PUT /api/v1/repos/{owner}/{name}/actions/retention` | `RepoAdmin` | web+browser |
| i18n:settings.retention.cleanup | :133 | `DELETE /api/v1/repos/{owner}/{name}/actions/retention/expired` | `RepoAdmin` | rust+browser |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/actions/retention` | `RepoAdmin` | rust+web+browser |

### `/[owner]/[repo]/settings/tags`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| ) as part} | :122 | `POST /api/v1/repos/{owner}/{name}/tags/protection` | `RepoAdmin` | rust+web+browser |
| ) as part} | :122 | `PATCH /api/v1/repos/{owner}/{name}/tags/protection/{id}` | `RepoAdmin` | rust+web+browser |
| ) as part} | :122 | `GET /api/v1/repos/{owner}/{name}/tags/protection` | `RepoRead` | rust+web+browser |
| i18n:common.delete | :129 | `DELETE /api/v1/repos/{owner}/{name}/tags/protection/{id}` | `RepoAdmin` | rust+web+browser |
| i18n:common.delete | :129 | `GET /api/v1/repos/{owner}/{name}/tags/protection` | `RepoRead` | rust+web |

### `/[owner]/[repo]/settings/webhooks`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| application/json application/x-www-form-urlencoded toggleEve | :397 | `POST /api/v1/repos/{owner}/{name}/hooks` | `RepoAdmin` | rust+web+browser |
| application/json application/x-www-form-urlencoded toggleEve | :397 | `GET /api/v1/repos/{owner}/{name}/hooks` | `RepoAdmin` | rust+web+browser |
| (e) => setActive(hook, e.currentTarget.c | :468 | `PATCH /api/v1/repos/{owner}/{name}/hooks/{id}` | `RepoAdmin` | rust+web+browser |
| (e) => setActive(hook, e.currentTarget.c | :468 | `GET /api/v1/repos/{owner}/{name}/hooks` | `RepoAdmin` | rust+web |
| i18n:settings.webhooks.hide_deliveries | :477 | `GET /api/v1/repos/{owner}/{name}/hooks/{id}` | `RepoAdmin` | rust+web+browser |
| i18n:settings.webhooks.hide_deliveries | :477 | `GET /api/v1/repos/{owner}/{name}/hooks/{id}/deliveries` | `RepoAdmin` | rust+web+browser |
| i18n:common.loading | :488 | `DELETE /api/v1/repos/{owner}/{name}/hooks/{id}` | `RepoAdmin` | rust+web+browser |
| i18n:common.loading | :488 | `GET /api/v1/repos/{owner}/{name}/hooks` | `RepoAdmin` | rust+web |
| i18n:common.loading | :506 | `GET /api/v1/repos/{owner}/{name}/hooks/{id}/deliveries` | `RepoAdmin` | rust+web |
| i18n:settings.webhooks.redelivering | :541 | `POST /api/v1/repos/{owner}/{name}/hooks/{id}/deliveries/{delivery_id}/redeliver` | `RepoAdmin` | rust+web+browser |
| i18n:settings.webhooks.redelivering | :541 | `GET /api/v1/repos/{owner}/{name}/hooks/{id}/deliveries` | `RepoAdmin` | rust+web |

### `/[owner]/[repo]/tags`

| Control | Source | Call | `Access` | test |
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
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/time_tracking`

| Control | Source | Call | `Access` | test |
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
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/issues` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/wiki`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| showCreate = false}> | :113 | `POST /api/v1/repos/{owner}/{name}/wiki` | `RepoWrite` | rust+web+browser |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/wiki` | `RepoRead` | rust+web+browser |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/wiki/[title]`

| Control | Source | Call | `Access` | test |
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
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/[owner]/[repo]/wiki/[title]/history`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:common.view | :170 | `GET /api/v1/repos/{owner}/{name}/wiki/{title}/revisions/{rev_id}` | `RepoRead` | rust+web |
| toggleStar | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/star` | `RepoAuthRead` | rust+web+smoke |
| toggleStar | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/starred` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `DELETE /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `PUT /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| 👁 | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/watch` | `RepoAuthRead` | rust+web |
| ⚡ | `RepoHeader` | `POST /api/v1/repos/{owner}/{name}/fork` | `RepoAuthRead` | rust+smoke |
| i18n:repo.download_zip | `RepoHeader` | `GET /api/v1/repos/{owner}/{name}/archive/{archive}` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}/wiki/{title}/history` | `RepoRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}/{name}` | `RepoRead` | rust+web |

### `/admin/audit` 🔒

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| applyFilter | :173 | `GET /api/v1/admin/audit/logs` | `InstanceAdmin` | rust+web+smoke+browser |
| )} | :178 | `GET /api/v1/admin/audit/logs` | `InstanceAdmin` | rust+web+smoke |
| i18n:admin.audit.clear_filters | :185 | `GET /api/v1/admin/audit/logs` | `InstanceAdmin` | rust+web+smoke |
| i18n:admin.audit.fields.details | :241 | `GET /api/v1/admin/audit/logs/{id}` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:common.prev_arrow | :254 | `GET /api/v1/admin/audit/logs` | `InstanceAdmin` | rust+web+smoke |
| i18n:common.next_arrow | :256 | `GET /api/v1/admin/audit/logs` | `InstanceAdmin` | rust+web+smoke |

### `/admin/hooks` 🔒

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| selectedEvents = toggle(selectedEvents, event, input.current | `ScopedWebhooks` | `POST /api/v1/orgs/{name}/hooks` | `OrgAdmin` | rust |
| selectedEvents = toggle(selectedEvents, event, input.current | `ScopedWebhooks` | `POST /api/v1/admin/hooks` | `InstanceAdmin` | browser |
| selectedEvents = toggle(selectedEvents, event, input.current | `ScopedWebhooks` | `GET /api/v1/orgs/{name}/hooks` | `OrgAdmin` | rust+smoke |
| selectedEvents = toggle(selectedEvents, event, input.current | `ScopedWebhooks` | `GET /api/v1/admin/hooks` | `InstanceAdmin` | rust+smoke+browser |
| (input) => void update(hook, { active: i | `ScopedWebhooks` | `PATCH /api/v1/orgs/{name}/hooks/{id}` | `OrgAdmin` | **—** |
| (input) => void update(hook, { active: i | `ScopedWebhooks` | `PATCH /api/v1/admin/hooks/{id}` | `InstanceAdmin` | browser |
| (input) => void update(hook, { active: i | `ScopedWebhooks` | `GET /api/v1/orgs/{name}/hooks` | `OrgAdmin` | rust+smoke |
| (input) => void update(hook, { active: i | `ScopedWebhooks` | `GET /api/v1/admin/hooks` | `InstanceAdmin` | rust+smoke |
| i18n:settings.webhooks.save_events | `ScopedWebhooks` | `PATCH /api/v1/orgs/{name}/hooks/{id}` | `OrgAdmin` | **—** |
| i18n:settings.webhooks.save_events | `ScopedWebhooks` | `PATCH /api/v1/admin/hooks/{id}` | `InstanceAdmin` | **—** |
| i18n:settings.webhooks.save_events | `ScopedWebhooks` | `GET /api/v1/orgs/{name}/hooks` | `OrgAdmin` | rust+smoke |
| i18n:settings.webhooks.save_events | `ScopedWebhooks` | `GET /api/v1/admin/hooks` | `InstanceAdmin` | rust+smoke |
| i18n:settings.webhooks.view_deliveries | `ScopedWebhooks` | `GET /api/v1/orgs/{name}/hooks/{id}/deliveries` | `OrgAdmin` | **—** |
| i18n:settings.webhooks.view_deliveries | `ScopedWebhooks` | `GET /api/v1/admin/hooks/{id}/deliveries` | `InstanceAdmin` | browser |
| i18n:settings.webhooks.redeliver | `ScopedWebhooks` | `POST /api/v1/orgs/{name}/hooks/{id}/deliveries/{delivery_id}/redeliver` | `OrgAdmin` | **—** |
| i18n:settings.webhooks.redeliver | `ScopedWebhooks` | `POST /api/v1/admin/hooks/{id}/deliveries/{delivery_id}/redeliver` | `InstanceAdmin` | browser |
| i18n:settings.webhooks.redeliver | `ScopedWebhooks` | `GET /api/v1/orgs/{name}/hooks/{id}/deliveries` | `OrgAdmin` | **—** |
| i18n:settings.webhooks.redeliver | `ScopedWebhooks` | `GET /api/v1/admin/hooks/{id}/deliveries` | `InstanceAdmin` | **—** |
| i18n:common.delete | `ScopedWebhooks` | `DELETE /api/v1/orgs/{name}/hooks/{id}` | `OrgAdmin` | **—** |
| i18n:common.delete | `ScopedWebhooks` | `DELETE /api/v1/admin/hooks/{id}` | `InstanceAdmin` | browser |
| i18n:common.delete | `ScopedWebhooks` | `GET /api/v1/orgs/{name}/hooks` | `OrgAdmin` | rust+smoke |
| i18n:common.delete | `ScopedWebhooks` | `GET /api/v1/admin/hooks` | `InstanceAdmin` | rust+smoke |

### `/admin/orgs` 🔒

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:common.prev_arrow | :157 | `GET /api/v1/admin/orgs` | `InstanceAdmin` | rust+web |
| i18n:common.next_arrow | :159 | `GET /api/v1/admin/orgs` | `InstanceAdmin` | rust+web |
| i18n:common.loading | :177 | `DELETE /api/v1/admin/orgs/{name}` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:common.loading | :177 | `GET /api/v1/admin/orgs` | `InstanceAdmin` | rust+web+browser |

### `/admin/runners` 🔒

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:common.loading | :239 | `POST /api/v1/runners/register` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:common.loading | :239 | `GET /api/v1/admin/runners` | `InstanceAdmin` | web+smoke+browser |
| i18n:common.previous | :295 | `GET /api/v1/admin/runners` | `InstanceAdmin` | web+smoke |
| i18n:common.next | :297 | `GET /api/v1/admin/runners` | `InstanceAdmin` | web+smoke |
| i18n:common.loading | :309 | `DELETE /api/v1/admin/runners/{id}` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:common.loading | :309 | `GET /api/v1/admin/runners` | `InstanceAdmin` | web+smoke+browser |

### `/admin/settings` 🔒

| Control | Source | Call | `Access` | test |
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
| _(page load)_ | — | `GET /api/v1/admin/settings` | `InstanceAdmin` | rust+web+smoke+browser |

### `/admin/users` 🔒

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| (showCreate = false)}> | :281 | `POST /api/v1/admin/users` | `InstanceAdmin` | rust+web+smoke+browser |
| (showCreate = false)}> | :281 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+web+browser |
| i18n:admin.users.working | :342 | `POST /api/v1/admin/users/{id}/unlock` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:admin.users.working | :342 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+web+browser |
| i18n:admin.users.reset_password | :348 | `POST /api/v1/admin/users/{id}/password-reset` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:admin.users.reset_mfa | :351 | `POST /api/v1/admin/users/{id}/mfa/reset` | `InstanceAdmin` | web+smoke+browser |
| i18n:admin.users.reset_mfa | :351 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+web+browser |
| i18n:common.prev_arrow | :366 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+web |
| i18n:common.next_arrow | :368 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+web |
| i18n:common.loading | :406 | `PATCH /api/v1/admin/users/{id}` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:common.loading | :406 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+web+browser |
| i18n:common.loading | :427 | `DELETE /api/v1/admin/users/{id}` | `InstanceAdmin` | rust+web+smoke+browser |
| i18n:common.loading | :427 | `GET /api/v1/admin/users` | `InstanceAdmin` | rust+web+browser |

### `/dashboard`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| )} * / | :245 | `POST /api/v1/repos` | `User` | rust+web+smoke+browser |
| i18n:common.loading | :253 | `GET /api/v1/orgs` | `User` | rust+web+smoke+browser |
| i18n:common.loading | :310 | `GET /api/v1/repos/templates/gitignores` | `Public` | web |
| i18n:common.loading | :310 | `GET /api/v1/repos/templates/licenses` | `Public` | web |
| i18n:common.loading | :310 | `GET /api/v1/repos/templates/readmes` | `Public` | web |
| i18n:common.loading | :310 | `GET /api/v1/repos/templates/labels` | `Public` | web |
| _(page load)_ | — | `GET /api/v1/repos/{owner}` | `PublicFiltered` | rust+web+browser |

### `/explore`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| ← | :82 | `GET /api/v1/repos/explore` | `PublicFiltered` | rust+web+smoke |
| → | :88 | `GET /api/v1/repos/explore` | `PublicFiltered` | rust+web+smoke |

### `/forgot-password`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:auth.forgot_password.email | :56 | `POST /api/v1/users/forgot-password` | `Public` | rust |

### `/imports`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:imports.refresh | :216 | `GET /api/v1/imports` | `User` | rust+web |
| GitHub GitLab Gitea Git | :228 | `POST /api/v1/imports` | `User` | rust+web |
| GitHub GitLab Gitea Git | :228 | `GET /api/v1/imports` | `User` | rust+web |
| i18n:imports.deleting | :325 | `DELETE /api/v1/imports/{id}` | `User` | rust+web |

### `/login`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:auth.login.sso_retry | :259 | `GET /api/v1/auth/sso/providers` | `Public` | rust+web |

### `/notifications`

| Control | Source | Call | `Access` | test |
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
| _(page load)_ | — | `GET /api/v1/ws/notifications` | `Foreign:ws.rs` | rust+web |

### `/orgs`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| * | :119 | `GET /api/v1/orgs` | `User` | rust+web+smoke |

### `/orgs/[name]`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:common.loading | :544 | `DELETE /api/v1/orgs/{name}` | `OrgAdmin` | rust+web+smoke+browser |
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
| ` ? t('common.loading') : t('common.delete')} | :760 | `DELETE /api/v1/orgs/{name}/members/{user_id}` | `OrgAdmin` | rust+web+smoke+browser |
| ` ? t('common.loading') : t('common.delete')} | :760 | `GET /api/v1/orgs/{name}/members` | `OrgRead` | rust+web |
| _(page load)_ | — | `GET /api/v1/orgs/{name}` | `OrgRead` | rust+web+smoke |

### `/orgs/[name]/hooks`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| selectedEvents = toggle(selectedEvents, event, input.current | `ScopedWebhooks` | `POST /api/v1/orgs/{name}/hooks` | `OrgAdmin` | rust+browser |
| selectedEvents = toggle(selectedEvents, event, input.current | `ScopedWebhooks` | `POST /api/v1/admin/hooks` | `InstanceAdmin` | **—** |
| selectedEvents = toggle(selectedEvents, event, input.current | `ScopedWebhooks` | `GET /api/v1/orgs/{name}/hooks` | `OrgAdmin` | rust+smoke+browser |
| selectedEvents = toggle(selectedEvents, event, input.current | `ScopedWebhooks` | `GET /api/v1/admin/hooks` | `InstanceAdmin` | rust+smoke |
| (input) => void update(hook, { active: i | `ScopedWebhooks` | `PATCH /api/v1/orgs/{name}/hooks/{id}` | `OrgAdmin` | browser |
| (input) => void update(hook, { active: i | `ScopedWebhooks` | `PATCH /api/v1/admin/hooks/{id}` | `InstanceAdmin` | **—** |
| (input) => void update(hook, { active: i | `ScopedWebhooks` | `GET /api/v1/orgs/{name}/hooks` | `OrgAdmin` | rust+smoke |
| (input) => void update(hook, { active: i | `ScopedWebhooks` | `GET /api/v1/admin/hooks` | `InstanceAdmin` | rust+smoke |
| i18n:settings.webhooks.save_events | `ScopedWebhooks` | `PATCH /api/v1/orgs/{name}/hooks/{id}` | `OrgAdmin` | **—** |
| i18n:settings.webhooks.save_events | `ScopedWebhooks` | `PATCH /api/v1/admin/hooks/{id}` | `InstanceAdmin` | **—** |
| i18n:settings.webhooks.save_events | `ScopedWebhooks` | `GET /api/v1/orgs/{name}/hooks` | `OrgAdmin` | rust+smoke |
| i18n:settings.webhooks.save_events | `ScopedWebhooks` | `GET /api/v1/admin/hooks` | `InstanceAdmin` | rust+smoke |
| i18n:settings.webhooks.view_deliveries | `ScopedWebhooks` | `GET /api/v1/orgs/{name}/hooks/{id}/deliveries` | `OrgAdmin` | browser |
| i18n:settings.webhooks.view_deliveries | `ScopedWebhooks` | `GET /api/v1/admin/hooks/{id}/deliveries` | `InstanceAdmin` | **—** |
| i18n:settings.webhooks.redeliver | `ScopedWebhooks` | `POST /api/v1/orgs/{name}/hooks/{id}/deliveries/{delivery_id}/redeliver` | `OrgAdmin` | browser |
| i18n:settings.webhooks.redeliver | `ScopedWebhooks` | `POST /api/v1/admin/hooks/{id}/deliveries/{delivery_id}/redeliver` | `InstanceAdmin` | **—** |
| i18n:settings.webhooks.redeliver | `ScopedWebhooks` | `GET /api/v1/orgs/{name}/hooks/{id}/deliveries` | `OrgAdmin` | **—** |
| i18n:settings.webhooks.redeliver | `ScopedWebhooks` | `GET /api/v1/admin/hooks/{id}/deliveries` | `InstanceAdmin` | **—** |
| i18n:common.delete | `ScopedWebhooks` | `DELETE /api/v1/orgs/{name}/hooks/{id}` | `OrgAdmin` | browser |
| i18n:common.delete | `ScopedWebhooks` | `DELETE /api/v1/admin/hooks/{id}` | `InstanceAdmin` | **—** |
| i18n:common.delete | `ScopedWebhooks` | `GET /api/v1/orgs/{name}/hooks` | `OrgAdmin` | rust+smoke |
| i18n:common.delete | `ScopedWebhooks` | `GET /api/v1/admin/hooks` | `InstanceAdmin` | rust+smoke |

### `/reset-password`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:auth.reset_password.new_password | :107 | `POST /api/v1/users/reset-password` | `Public` | rust |

### `/search`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| _(page load)_ | — | `GET /api/v1/search` | `PublicFiltered` | rust+web |

### `/settings/agents`

| Control | Source | Call | `Access` | test |
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

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| (event) => toggle(key, event) | :95 | `PUT /api/v1/users/me/notification-settings` | `User` | rust+web |
| _(page load)_ | — | `GET /api/v1/users/me/notification-settings` | `User` | web |

### `/settings/profile`

| Control | Source | Call | `Access` | test |
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

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:account_security.mfa.retry | :422 | `GET /api/v1/users/mfa/backup` | `User` | rust+web |
| i18n:account_security.mfa.current_password | :437 | `POST /api/v1/users/mfa/backup/regenerate` | `User` | rust |
| i18n:account_security.mfa.current_password | :437 | `GET /api/v1/users/mfa/backup` | `User` | rust+web |
| i18n:account_security.mfa.current_password | :437 | `GET /api/v1/users/passkeys` | `User` | rust+web |
| i18n:account_security.mfa.current_password | :437 | `GET /api/v1/users/me/sso` | `User` | rust+web |
| i18n:account_security.mfa.current_password | :437 | `GET /api/v1/auth/sso/providers` | `Public` | rust+web |
| i18n:account_security.mfa.current_password | :457 | `POST /api/v1/users/mfa/disable` | `User` | rust |
| i18n:account_security.mfa.current_password | :457 | `GET /api/v1/users/mfa/backup` | `User` | rust+web |
| i18n:account_security.mfa.current_password | :457 | `GET /api/v1/users/passkeys` | `User` | rust+web |
| i18n:account_security.mfa.current_password | :457 | `GET /api/v1/users/me/sso` | `User` | rust+web |
| i18n:account_security.mfa.current_password | :457 | `GET /api/v1/auth/sso/providers` | `Public` | rust+web |
| i18n:account_security.mfa.starting | :470 | `POST /api/v1/users/mfa/setup` | `User` | rust+web+smoke |
| i18n:account_security.passkeys.retry | :500 | `GET /api/v1/users/passkeys` | `User` | rust+web |
| i18n:account_security.passkeys.remove | :514 | `DELETE /api/v1/users/passkeys/{id}` | `User` | rust |
| i18n:account_security.sso.retry | :567 | `GET /api/v1/users/me/sso` | `User` | rust+web |
| i18n:account_security.sso.unlink | :582 | `DELETE /api/v1/auth/sso/{slug}/unlink` | `User` | rust+web |
| )} | :600 | `POST /api/v1/auth/sso/{slug}/link` | `User` | rust+web |
| i18n:account_security.setup.code | :623 | `POST /api/v1/users/mfa/enable` | `User` | rust+smoke |
| i18n:account_security.setup.code | :623 | `GET /api/v1/users/mfa/backup` | `User` | rust+web |
| i18n:account_security.setup.code | :623 | `GET /api/v1/users/passkeys` | `User` | rust+web |
| i18n:account_security.setup.code | :623 | `GET /api/v1/users/me/sso` | `User` | rust+web |
| i18n:account_security.setup.code | :623 | `GET /api/v1/auth/sso/providers` | `Public` | rust+web |
| _(page load)_ | — | `GET /api/v1/users/me` | `User` | rust+web+smoke |

### `/settings/signing-keys`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:signing_keys.name | :108 | `POST /api/v1/users/signing-keys` | `User` | web |
| i18n:signing_keys.name | :108 | `GET /api/v1/users/signing-keys` | `User` | rust+web |
| i18n:signing_keys.delete | :138 | `DELETE /api/v1/users/signing-keys/{id}` | `User` | web |
| i18n:signing_keys.delete | :138 | `GET /api/v1/users/signing-keys` | `User` | rust+web |

### `/settings/ssh-keys`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:ssh_keys.name | :130 | `POST /api/v1/users/ssh-keys` | `User` | rust+web+smoke |
| i18n:ssh_keys.name | :130 | `GET /api/v1/users/ssh-keys` | `User` | rust+web |
| i18n:ssh_keys.deleting | :182 | `DELETE /api/v1/users/ssh-keys/{id}` | `User` | rust |
| i18n:ssh_keys.deleting | :182 | `GET /api/v1/users/ssh-keys` | `User` | rust+web |

### `/settings/tokens`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:access_tokens.name | :191 | `POST /api/v1/users/tokens` | `User` | rust+web |
| i18n:access_tokens.name | :191 | `GET /api/v1/users/tokens` | `User` | rust+web |
| i18n:access_tokens.revoking | :242 | `DELETE /api/v1/users/tokens/{id}` | `User` | rust |
| i18n:access_tokens.revoking | :242 | `GET /api/v1/users/tokens` | `User` | rust+web |

### `/verify-email`

| Control | Source | Call | `Access` | test |
|---|---|---|---|---|
| i18n:auth.verify_email.working | :67 | `POST /api/v1/users/verify-email` | `Public` | rust+web |

## Routes not reached from the browser

This includes legitimate non-browser clients such as Git, OCI, LFS,
CI runners, and webhooks. A browser test cannot cover these routes.

| Method | URL | `Access` | test |
|---|---|---|---|
| GET | `/v2/` | `Foreign:oci.rs` | rust |
| GET | `/v2` | `Foreign:oci.rs` | rust |
| GET | `/v2/auth/token` | `Public` | rust |
| GET | `/v2/{owner}/{repo}/tags/list` | `Foreign:oci.rs` | rust |
| GET | `/v2/{owner}/{repo}/manifests/{reference}` | `Foreign:oci.rs` | rust+smoke |
| HEAD | `/v2/{owner}/{repo}/manifests/{reference}` | `Foreign:oci.rs` | rust |
| PUT | `/v2/{owner}/{repo}/manifests/{reference}` | `Foreign:oci.rs` | rust |
| DELETE | `/v2/{owner}/{repo}/manifests/{reference}` | `Foreign:oci.rs` | rust |
| GET | `/v2/{owner}/{repo}/blobs/{digest}` | `Foreign:oci.rs` | rust |
| HEAD | `/v2/{owner}/{repo}/blobs/{digest}` | `Foreign:oci.rs` | rust |
| POST | `/v2/{owner}/{repo}/blobs/uploads/` | `Foreign:oci.rs` | rust |
| POST | `/v2/{owner}/{repo}/blobs/uploads` | `Foreign:oci.rs` | rust |
| PATCH | `/v2/{owner}/{repo}/blobs/uploads/{uuid}` | `Foreign:oci.rs` | rust |
| GET | `/v2/{owner}/{repo}/blobs/uploads/{uuid}` | `Foreign:oci.rs` | rust+smoke |
| PUT | `/v2/{owner}/{repo}/blobs/uploads/{uuid}` | `Foreign:oci.rs` | rust |
| GET | `/api-docs/openapi.json` | `User` | rust+smoke |
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
| POST | `/api/v1/users/me/sudo` | `User` | rust+smoke |
| POST | `/api/v1/users/password/initial` | `Public` | rust+web |
| GET | `/api/v1/avatars/{username}` | `Public` | rust |
| POST | `/api/v1/users/mfa/verify` | `Public` | rust |
| POST | `/api/v1/users/passkeys/register/start` | `User` | rust |
| POST | `/api/v1/users/passkeys/register/finish` | `User` | rust |
| POST | `/api/v1/users/passkeys/login/start` | `Public` | rust |
| POST | `/api/v1/users/passkeys/login/finish` | `Public` | rust |
| GET | `/api/v1/auth/sso/{slug}` | `Public` | rust |
| GET | `/api/v1/auth/sso/{slug}/callback` | `Public` | rust |
| POST | `/api/v1/auth/sso/{slug}/reauthenticate` | `User` | rust |
| GET | `/api/v1/repos/{owner}/{name}/labels/{id}` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/issue_config/validate` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/issues/{number}/labels` | `RepoRead` | rust |
| GET | `/api/v1/repos/{owner}/{name}/pulls/{number}/reviews/{id}` | `RepoRead` | rust |
| POST | `/api/v1/repos/{owner}/{name}/lfs/objects/batch` | `Foreign:api/lfs.rs` | rust |
| GET | `/api/v1/repos/{owner}/{name}/lfs/objects/{oid}` | `Foreign:api/lfs.rs` | rust+smoke |
| PUT | `/api/v1/repos/{owner}/{name}/lfs/objects/{oid}` | `Foreign:api/lfs.rs` | rust |
| POST | `/api/v1/repos/{owner}/{name}/lfs/locks` | `Foreign:api/lfs_locks.rs` | rust |
| POST | `/api/v1/repos/{owner}/{name}/lfs/locks/verify` | `Foreign:api/lfs_locks.rs` | **—** |
| GET | `/api/v1/orgs/{name}/hooks/{id}` | `OrgAdmin` | rust |
| GET | `/api/v1/admin/hooks/{id}` | `InstanceAdmin` | **—** |
| GET | `/api/v1/repos/{owner}/{name}/pipelines/{id}/jobs/{job_id}` | `RepoRead` | rust |
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
| GET | `/api/v1/admin/orgs/{name}` | `InstanceAdmin` | rust+web |
| GET | `/api/v1/admin/sso/providers/{id}` | `InstanceAdmin` | rust |
| POST | `/api/v1/repos/{owner}/{name}/webhooks/external/ci` | `RepoWrite` | rust |
| GET | `/api/v1/ai/repos/{owner}/{name}/summary` | `RepoRead` | rust |
| GET | `/api/v1/ai/repos/{owner}/{name}/issues` | `RepoRead` | rust |
| GET | `/api/v1/ai/repos/{owner}/{name}/prs` | `RepoRead` | rust |
| GET | `/api/v1/ai/repos/{owner}/{name}/tree` | `RepoRead` | rust |
| GET | `/api/v1/ai/repos/{owner}/{name}/search/code` | `RepoRead` | rust |
| POST | `/api/v1/ai/repos/{owner}/{name}/index` | `RepoWrite` | rust |
| POST | `/api/v1/mcp` | `User` | rust |

