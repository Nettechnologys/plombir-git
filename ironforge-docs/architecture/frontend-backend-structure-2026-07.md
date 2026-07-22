# IronForge 前后端结构分布（2026-07）

**生成日期**: 2026-07-05  
**配套文档**: `project-architecture-2026-07.md`、`architecture-followups-2026-07.md`

---

## 1. 总览

IronForge 前后端边界可以概括为：

```text
SvelteKit SPA
  -> web/src/lib/api/client.svelte.ts
  -> web/src/lib/api/_base.svelte.ts
  -> /api/v1 REST API
  -> rg-http handlers
  -> rg-core service / rg-db ops
  -> SQLite + repo_root filesystem
```

非普通 REST 的入口包括：

```text
Git clone/push
  -> /git/{owner}/{repo}
  -> rg-http Git handlers
  -> rg-git protocol

SSH Git
  -> rg-ssh
  -> rg-git protocol

OCI clients
  -> /v2
  -> rg-http oci.rs

Notifications
  -> /api/v1/ws/notifications

Runner
  -> /api/v1/runners

AI Agent
  -> ironforge-mcp
  -> /api/v1 REST API
```

---

## 2. 后端结构

### 2.1 Workspace Crates

| Crate | 路径 | 职责 |
|-------|------|------|
| `rg-cli` | `crates/rg-cli` | 主二进制、serve/migrate/create-repo/import/index/package/runner 命令 |
| `rg-http` | `crates/rg-http` | Axum HTTP、REST API、Git HTTP、OCI、WebSocket、OpenAPI、静态资源 |
| `rg-ssh` | `crates/rg-ssh` | SSH server、SSH auth、Git command 分发 |
| `rg-core` | `crates/rg-core` | 业务服务、认证、安全、包注册表、搜索、导入、镜像、审计等 |
| `rg-db` | `crates/rg-db` | SeaORM entities、ops、migrations |
| `rg-git` | `crates/rg-git` | Git wire protocol、pkt-line、sideband、Protocol V2、Git CLI gateway |
| `rg-ci` | `crates/rg-ci` | CI 配置解析、Gitea Actions 兼容、pipeline runner |
| `rg-runner` | `crates/rg-runner` | 独立 runner agent |
| `rg-mcp` | `crates/rg-mcp` | MCP tools/resources server |

### 2.2 `rg-core` 领域模块

| 领域 | 路径 |
|------|------|
| 认证 | `crates/rg-core/src/auth/` |
| 用户 | `crates/rg-core/src/user/` |
| 仓库 | `crates/rg-core/src/repo/` |
| Issue | `crates/rg-core/src/issue/` |
| Pull Request | `crates/rg-core/src/pull_request/` |
| Review | `crates/rg-core/src/review/` |
| Wiki | `crates/rg-core/src/wiki/` |
| LFS | `crates/rg-core/src/lfs/` |
| Webhook | `crates/rg-core/src/webhook/` |
| CI bridge | `crates/rg-core/src/ci/` |
| Package Registry | `crates/rg-core/src/package_registry/` |
| Org / Notification | `crates/rg-core/src/org/`、`notification/` |
| SSO/MFA/LDAP | `crates/rg-core/src/auth/sso.rs`、`totp.rs`、`ldap.rs` |
| Audit | `crates/rg-core/src/audit/` |
| Search | `crates/rg-core/src/search/` |
| Mirror / Import | `crates/rg-core/src/mirror/`、`import/` |
| Board / Time Tracking | `crates/rg-core/src/board/`、`time_tracking/` |
| Platform helpers | `crates/rg-core/src/platform/` |

### 2.3 `rg-http` API 模块

| API 文件 | 主要 REST 能力 |
|----------|----------------|
| `users.rs` | 注册、登录、当前用户、PAT、密码重置 |
| `mfa.rs` | TOTP MFA setup/enable/verify/disable/backup |
| `sso.rs` | OAuth2 SSO providers/authorize/callback/refresh/unlink |
| `repos.rs` | 仓库 CRUD、star/watch、fork、transfer、status、label templates |
| `repo_content.rs` | tree/blob/file/log/branches/tags/commit signature |
| `issues.rs` | Issue CRUD、comments、labels、milestones |
| `pulls.rs` | PR CRUD、diff、merge |
| `reviews.rs` | PR review 与 inline comments |
| `deploy_keys.rs` | 仓库 Deploy Key CRUD（admin） |
| `wiki.rs` | Wiki pages、history、diff |
| `releases.rs` | Release 与 release assets |
| `ci.rs` | pipelines、jobs、retry、cancel |
| `runners.rs` | runner registration、polling、admin runners |
| `artifacts.rs` | artifact metadata |
| `packages.rs` | package REST 与协议 metadata endpoints |
| `orgs.rs` | organizations、teams、members |
| `notifications.rs` | notification list/read/delete |
| `webhooks.rs` | repo webhooks |
| `webhooks_external.rs` | external CI webhook |
| `branch_protection.rs` | protected branches |
| `collaborators.rs` | repo collaborators |
| `search.rs` | repos/issues/wiki/code search |
| `boards.rs` | kanban board |
| `time_tracking.rs` | time entries |
| `mirrors.rs` | repository mirror |
| `imports.rs` | GitHub/GitLab import |
| `audit.rs` | admin audit logs |
| `admin.rs` | admin users/orgs/settings |
| `lfs.rs` | Git LFS |
| `archive.rs` | repo archives |
| `ai.rs` | AI-assist API |

---

## 3. 前端结构

### 3.1 技术栈

| 项 | 当前实现 |
|----|----------|
| Framework | SvelteKit 2 + Svelte 5 |
| Rendering | static SPA |
| Adapter | `@sveltejs/adapter-static` |
| SSR | disabled |
| Build output | `web/build` |
| Dev proxy | `/api/v1`、`/health` -> `127.0.0.1:8080` |
| API client | `web/src/lib/api/client.svelte.ts` |
| Request base | `web/src/lib/api/_base.svelte.ts` |

### 3.2 前端目录

| 路径 | 职责 |
|------|------|
| `web/src/routes` | SvelteKit 页面路由 |
| `web/src/lib/api` | API request 基础层、聚合 client、旧拆分 client |
| `web/src/lib/components` | Navbar、RepoHeader、FileEditor、PipelineBadge 等 |
| `web/src/lib/stores` | auth、instance、keyboard 等全局状态 |
| `web/src/lib/i18n` | 中英文翻译、locale、日期格式化 |
| `web/src/lib/utils` | Markdown、diff、search、format helpers |
| `web/static` | 静态资源 |

### 3.3 API Client

当前事实主入口：

```text
web/src/lib/api/client.svelte.ts
```

请求基础层：

```text
web/src/lib/api/_base.svelte.ts
  API_BASE = VITE_API_BASE || /api/v1
  credentials: include
  in-memory token optional Authorization
  timeout wrapper
```

需要注意：

- 登录后 token 仍保存在内存，用于 Bearer 兼容。
- 页面刷新后内存 token 会丢失，浏览器用户 API 应依赖 HttpOnly cookie；关键用户、Admin、SSO 和 PAT 管理路径已按 cookie-aware 模型修复。
- `client.svelte.ts` 已降至 38 行，是纯 re-export 聚合入口；API 真实实现已拆到 `repos.ts`、`auth.ts`、`admin.ts`、`packages.ts`、`issues.ts`、`pulls.ts`、`websockets.ts` 等领域模块，后续新增领域应优先独立建模块。

---

## 4. 页面到后端能力映射

### 4.1 全局页面

| 页面 | 前端路径 | 主要后端 API |
|------|----------|--------------|
| 首页/探索 | `/` | `/api/v1/repos/explore`、`/health` |
| 登录 | `/login` | `/api/v1/users/login` |
| 注册 | `/register` | `/api/v1/users/register` |
| Dashboard | `/dashboard` | `/api/v1/repos`、notifications |
| 搜索 | `/search` | `/api/v1/search` |
| 通知 | `/notifications` | `/api/v1/notifications`、`/api/v1/ws/notifications` |
| 用户设置 | `/settings/*` | `/api/v1/users/me`、tokens、MFA |
| Admin | `/admin/*` | `/api/v1/admin/*`、audit、runners |

### 4.2 仓库页面

| 页面 | 前端路径 | 后端能力 |
|------|----------|----------|
| 仓库首页/文件 | `/{owner}/{repo}` | repo detail、tree/blob、README |
| 文件浏览 | `/{owner}/{repo}/src/...` | repo content API |
| commits/log | `/{owner}/{repo}/commits` | repo log API |
| branches/tags | `/{owner}/{repo}/branches`、`tags` | branches/tags API |
| Issues | `/{owner}/{repo}/issues` | issues、labels、milestones |
| Pull Requests | `/{owner}/{repo}/pulls` | pulls、reviews、merge |
| Deploy Keys | `/{owner}/{repo}/settings/deploy-keys` | `/repos/{owner}/{name}/keys` |
| Wiki | `/{owner}/{repo}/wiki` | wiki pages/history/diff |
| Pipelines | `/{owner}/{repo}/pipelines` | ci pipelines/jobs |
| Packages | `/{owner}/{repo}/packages` | package registry REST |
| Releases | `/{owner}/{repo}/releases` | releases/assets |
| Webhooks | `/{owner}/{repo}/settings/hooks` | webhooks |
| Branch protection | settings | branch protection |
| Collaborators | settings | collaborators |
| Mirror/import/board/time | corresponding pages | mirrors/imports/boards/time tracking |

### 4.3 非页面客户端

| 客户端 | 入口 | 后端 |
|--------|------|------|
| Git CLI HTTP | `http(s)://host/git/{owner}/{repo}` | `rg-http` Git handlers |
| Git CLI SSH | `ssh://git@host:2222/{owner}/{repo}` | `rg-ssh` |
| Docker/OCI | `/v2/{owner}/{repo}` | `rg-http/src/oci.rs` |
| External runner | `/api/v1/runners` | `api/runners.rs` |
| AI Agent | `ironforge-mcp` stdio | REST API through PAT |

---

## 5. 状态管理

| Store | 路径 | 职责 |
|-------|------|------|
| Auth | `web/src/lib/stores/auth.svelte.ts` | 当前用户、登录态、fetchUser、logout |
| Instance | `web/src/lib/stores/instance.svelte.ts` | 维护模式、实例 banner、快捷键提示 |
| Keyboard | `web/src/lib/stores/keyboard.svelte.ts` | 快捷键帮助和事件管理 |
| i18n | `web/src/lib/i18n/` | locale、翻译、格式化 |

认证状态当前设计意图是 HttpOnly cookie 主导，内存 token 为兼容 Bearer 场景。但后端 `GET /users/me` 等路径仍需修正为 cookie-aware，才能保证刷新后恢复登录态。

---

## 6. WebSocket

| 通道 | 后端 | 前端 |
|------|------|------|
| Notifications | `/api/v1/ws/notifications` | `connectNotificationWebSocket()` 已接入 |
| CI job logs | `/api/v1/ws/job/{job_id}` | `connectJobLogWebSocket()` + pipelines 日志弹窗已接入 |

WebSocket auth 支持：

- HttpOnly cookie；
- `Sec-WebSocket-Protocol: bearer.<jwt>`；
- query token。

---

## 7. Package Registry 前后端口径

前端 `PACKAGE_FORMATS` 与后端 `package_types::ALL` 都列出：

```text
cargo, npm, maven, pypi, docker, nuget, rubygems, go, helm, composer,
conan, conda, alpine, debian, rpm, swift, generic
```

但专用 adapter/protocol endpoint 主要覆盖：

```text
cargo, npm, maven, pypi, docker/oci, nuget, rubygems, helm, composer, generic
```

因此 UI/文档已把 `go/conan/conda/alpine/debian/rpm/swift` 标注为 Generic fallback；后续若补专用协议，再从格式元数据中移除 fallback 标记。

---

## 8. 后端能力与前端覆盖

| 能力 | 后端 | 前端 | 备注 |
|------|------|------|------|
| 用户登录/注册 | 有 | 有 | 关键浏览器用户 API 已支持 HttpOnly cookie |
| PAT | 有 | 有 | settings/tokens |
| MFA | 有 | 有 | disable 已校验密码 bool 返回值 |
| SSO | 有 | 有入口 | callback 设置 HttpOnly cookie 并 redirect |
| 仓库浏览 | 有 | 有 | tree/blob/log/branches/tags |
| Git HTTP/SSH | 有 | clone URL | 浏览器不直接调用协议 |
| Issues/Labels/Milestones | 有 | 有 | |
| PR/Review/Merge | 有 | 有 | |
| Wiki history/diff | 有 | 有 | |
| CI pipelines | 有 | 有 | job log WebSocket 已接入日志弹窗 |
| Runner admin | 有 | 有 | runner 注册要求 admin auth token 或既有 runtime token |
| Package Registry | 有 | 有 | REST/protocol 读写已接入 repo-scoped 权限 |
| OCI `/v2` | 有 | 无页面直接调用 | Docker/OCI client 使用 |
| Notifications WS | 有 | 有 | |
| Audit logs | 有 | 有 admin 页面 | |
| Mirror | 有 | 有 | |
| Import | 有 | 有 | |
| Board | 有 | 有 | |
| Time tracking | 有 | 有 | |
| MCP | 有 | 无浏览器页面 | AI Agent 使用 |
| LDAP | core 能力 | 未确认 UI | 登录集成需核验 |

---

## 9. 新功能落点建议

新增后端领域功能时：

1. DB 表和 migration 放 `crates/rg-db/src/entities`、`migrations`、`ops`。
2. 业务逻辑优先放 `crates/rg-core/src/{domain}`。
3. HTTP handler 放 `crates/rg-http/src/api/{domain}.rs`。
4. 路由挂载在 `crates/rg-http/src/lib.rs`。
5. 前端 API 方法通过 `web/src/lib/api/client.svelte.ts` 统一导出；新增领域优先放独立模块，再由主入口 re-export。
6. 页面放 `web/src/routes` 对应业务域。
7. 新增权限边界时同步补 `rg-http/tests` 集成测试。
8. 需要 OpenAPI 的 handler 同步补 `utoipa::path` 和 contract/smoke 覆盖。

新增 Git 协议功能时：

1. 优先放 `rg-git`。
2. HTTP 和 SSH 只做入口适配。
3. 读写权限必须在入口层明确校验。
4. Git CLI 调用统一走 `GitCommandGateway`。

新增前端页面时：

1. 复用 `RepoHeader`、`Navbar`、现有 store 和 i18n。
2. API 调用统一走 `_base.svelte.ts` 的 `request()`。
3. 需要实时能力时优先确认后端 WebSocket 是否已有。
4. 避免新增和 `client.svelte.ts` 并行的重复 client；新增旧路径时应保持 re-export 或先完成主 client 领域拆分。
