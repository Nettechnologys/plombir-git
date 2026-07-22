# IronForge 架构修复执行计划（2026-07）

**生成日期**: 2026-07-05  
**来源**: `architecture-followups-2026-07.md`  
**目标**: 把架构重盘发现的 P0/P1 缺口转成可拆分、可验证的修复计划。

---

## 范围说明

本文不是新的架构盘点，而是后续代码修复的执行蓝图。修复优先级按安全风险、权限一致性、部署可用性和回归验证价值排序。

修复时建议坚持三条边界：

- 浏览器会话、PAT、Git HTTP Basic/Bearer、Runner token 不要混成一个认证入口；可以复用底层 JWT/PAT 校验，但语义要保持清楚。
- 所有仓库关联资源默认通过 `can_read` / `can_write` 做 repo-scoped 授权，只有明确公开资源才允许匿名读。
- 每个 P0 修复必须补集成测试，避免再次出现“功能存在但关键路径不可用”的状态。

---

## 执行波次

### Wave 1：认证会话正确性

**状态（2026-07-05）**: 已完成首批代码修复。`users/me`、PAT token 管理、Admin helper、SSO refresh/unlink 已改为 cookie-aware；MFA disable 已检查 `verify_password` 返回值；SSO callback 缺少 query state 已拒绝。已通过 `cargo check -p rg-http --tests` 和 3 个新增目标测试。后续如发现其他浏览器页面仍调用 Bearer-only handler，再按同一模式补齐。

#### 1. 统一浏览器用户 API 的 Cookie/Bearer 认证

**问题**: 前端已经 cookie-first，但部分后端 handler 仍只调用 `extract_bearer_claims`，浏览器刷新后可能无法通过 `/users/me`、Admin、SSO 账号管理等接口恢复状态。  
**主要文件**:

- `crates/rg-http/src/api/auth.rs`
- `crates/rg-http/src/api/users.rs`
- `crates/rg-http/src/api/admin.rs`
- `crates/rg-http/src/api/audit.rs`
- `crates/rg-http/src/api/sso.rs`
- `crates/rg-http/src/api/repos.rs`
- `crates/rg-http/src/api/labels.rs`
- `crates/rg-http/src/api/time_tracking.rs`

**建议做法**:

1. 以 `AuthUser` extractor 或 `extract_user_id` 作为浏览器用户 API 的默认认证入口。
2. 保留 `extract_bearer_claims` 给确实需要 username claim 或 Bearer-only 语义的路径。
3. `require_admin` 改为接受 cookie 或 Bearer，内部只返回当前 user id。
4. PAT 认证中间件和 Git HTTP 认证不要被这次改动破坏。

**验收测试**:

- 登录后只携带 `ironforge_token` cookie，`GET /api/v1/users/me` 返回 200。
- Admin 用户只携带 cookie，可访问 Admin API；普通用户返回 403。
- 现有 `/api-docs` JWT/PAT 测试保持通过。

#### 2. 修复 MFA disable 密码校验

**问题**: `disable_mfa` 调用 `verify_password` 后只处理错误，没有检查返回的 `bool`。  
**主要文件**:

- `crates/rg-http/src/api/mfa.rs`

**建议做法**:

```rust
let password_ok = verify_password(&req.password, &user.password_hash)
    .map_err(|_| AppError::unauthorized("invalid password"))?;
if !password_ok {
    return Err(AppError::unauthorized("invalid password"));
}
```

**验收测试**:

- MFA 已启用时，错误密码禁用 MFA 返回 401/403，且 `mfa_enabled` 仍为 true。
- 正确密码可以禁用 MFA。

#### 3. 收紧 SSO callback state 校验

**问题**: callback 缺少 `state` 参数时仍有兼容分支继续执行，削弱 OAuth CSRF 防护。  
**主要文件**:

- `crates/rg-http/src/api/sso.rs`

**建议做法**:

1. 缺少 state cookie、缺少 query state、state 不一致均拒绝。
2. 如确实需要历史兼容，增加显式配置项，例如 `auth.sso_allow_missing_state = false`，默认关闭。
3. 复查 callback 是否应设置 HttpOnly auth cookie 并 redirect 前端，避免只返回 JSON 导致浏览器 SSO 闭环不可用。

**验收测试**:

- 无 state cookie 的 callback 返回 403。
- query state 与 cookie state 不一致返回 403。
- 正常 authorize/callback 流程返回可登录结果，且 cookie/redirect 行为与前端约定一致。

---

### Wave 2：仓库级权限一致性

**状态（2026-07-05）**: Pipeline API、Package REST/protocol endpoints、OCI `/v2` 与 SSH Git repo-scoped 权限已完成首批修复。Pipeline 的 `list/get/job` 已按仓库 `can_read_repo` 授权，`trigger/retry/cancel` 已按 `can_write_repo` 授权，并补充 pipeline/job 与 URL 仓库的归属校验。Package 的 REST 读、protocol metadata/download 已接入 `can_read_repo`，publish/delete/yank 已接入 `can_write_repo`，publish 已显式调用 adapter validate；同时补了 package 表名单复数纠偏迁移。OCI pull/push handlers 已绑定 IronForge repo 读写权限，token endpoint 只签发实际允许的 scope，测试 router 已补挂 `/v2`。SSH exec path 已在启动 `git-upload-pack` / `git-receive-pack` 前分别检查仓库读写权限。已通过 `cargo check -p rg-http --tests`、`ci_permission_tests`、`package_permission_tests`、`oci_permission_tests`、`cargo check -p rg-ssh -j1` 和 `git diff --check`；`cargo test -p rg-ssh -j1 -- --nocapture` 两次卡在依赖测试编译阶段后中断。

#### 4. Pipeline API 接入认证和仓库权限

**问题**: Pipeline list/get/job/trigger/retry/cancel 路径需要按仓库读写权限约束。  
**主要文件**:

- `crates/rg-http/src/api/ci.rs`
- `crates/rg-core/src/repo/service.rs`
- `crates/rg-http/tests/`

**建议权限模型**:

| 操作 | 权限 |
|------|------|
| list pipelines | `can_read` |
| get pipeline | `can_read` |
| get job/log | `can_read` |
| trigger pipeline | `can_write` |
| retry/cancel pipeline 或 job | `can_write` |

**验收测试**:

- 匿名用户不能读取私有仓库 pipeline。
- 只读协作者可查看 pipeline，但不能 trigger/retry/cancel。
- write/admin 协作者可执行写操作。

#### 5. Package Registry REST/protocol 端点补 repo 权限

**问题**: 包列表、下载、发布、删除和 yank 需要与仓库可见性和写权限一致。  
**主要文件**:

- `crates/rg-http/src/api/packages.rs`
- `crates/rg-core/src/package_registry/`
- `crates/rg-http/tests/`

**建议权限模型**:

| 操作 | 权限 |
|------|------|
| list/get/download package | public repo 可匿名读；private repo 需 `can_read` |
| protocol metadata/index | 同读权限 |
| publish/upload | `can_write` |
| yank/delete | `can_write` 或 admin |

**实现建议**:

1. 增加统一 helper：根据 owner/repo/package resolve repo，再检查 `can_read_repo` 或 `can_write_repo`。
2. 包协议 handler 不要绕过 REST helper。
3. `PackageAdapter::validate` 应在 publish 时显式调用，再执行 metadata extract。

**验收测试**:

- 私有仓库 package metadata/download 匿名返回 401/404。
- read 协作者可下载，不可 publish/delete。
- write 协作者可 publish。

#### 6. OCI `/v2` 接入仓库可见性与 scope 授权

**问题**: OCI anonymous pull 和 token scope 需要绑定 repo 权限。  
**主要文件**:

- `crates/rg-http/src/oci.rs`
- `crates/rg-core/src/auth/oci_token.rs`
- `crates/rg-core/src/repo/service.rs`

**建议做法**:

1. 解析 OCI repository name 到 IronForge owner/repo。
2. pull scope：public repo 可匿名读；private repo 需 `can_read`。
3. push scope：需 `can_write`。
4. token endpoint 只能签发调用者有权访问的 scope。

**验收测试**:

- 匿名不能拉取私有镜像 manifest/blob。
- read 协作者可 pull，不可 push。
- write 协作者可 push manifest/blob。

#### 7. SSH Git 接入 repo-level 授权

**状态（2026-07-05）**: 已完成 repo-level 授权。`auth_publickey` / `auth_password` 已保存 IronForge `user_id`，`exec_request` 已解析 SSH Git 命令中的 `owner/repo`，`git-upload-pack` 走 `can_read_repo`，`git-receive-pack` 走 `can_write_repo`。分支保护 pre-receive 强拦截仍作为独立待办保留。

**问题**: SSH 层认证了用户身份，但 exec path 需要在 `git-upload-pack` / `git-receive-pack` 前做仓库读写权限检查。  
**主要文件**:

- `crates/rg-ssh/src/lib.rs`
- `crates/rg-core/src/repo/service.rs`

**建议做法**:

1. 在 SSH auth 成功后保存 IronForge `user_id`，不要只保存 SSH username。
2. 从 exec command 解析 owner/repo 和 service。
3. `git-upload-pack` 调用 `can_read`。
4. `git-receive-pack` 调用 `can_write`，并在 ref update 前接入分支保护强拦截。

**验收测试**:

- 私有仓库非协作者无法 SSH clone。
- read 协作者可以 clone，不能 push。
- write 协作者可以 push。
- 受保护分支 push 在 ref 更新前被拒绝。

---

### Wave 3：Runner、部署和自动回归

**状态（2026-07-05）**: Runner 自动注册闭环已完成首批修复。`/api/v1/runners/register` 已收紧为 admin-only，并复用 cookie-aware `extract_user_id`；`ironforge-runner register/run` 与 `ironforge runner` 自动注册已支持 `--auth-token` 或 `IRONFORGE_AUTH_TOKEN`，已有 `--runner-id + --token` 运行路径不受影响。已通过 `runner_auth_tests`、`cargo check -p rg-runner`、`cargo check -p rg-cli` 和 `git diff --check`。

#### 8. 明确 Runner 注册模型

**状态（2026-07-05）**: 已完成 admin auth token 注册路径。独立 registration token 仍可作为后续增强，用于更细的过期、一次性使用和作用域控制。

**问题**: 后端 runner register 要 Bearer JWT，但 `ironforge-runner` / `ironforge runner` 自动注册路径未携带 Authorization。  
**主要文件**:

- `crates/rg-http/src/api/runners.rs`
- `crates/rg-runner/src/main.rs`
- `crates/rg-cli/src/main.rs`

**已落地方案**:

- Runner 首次注册时提交 admin `--auth-token` 或 `IRONFORGE_AUTH_TOKEN`，后端换发 runner runtime token。
- 后续 poll/heartbeat/job update 只使用 runner runtime token。
- 后端注册入口要求 admin 身份，避免普通用户注册任意全局 runner。

**后续增强**:

- Admin API 或配置生成独立 registration token。
- registration token 可一次性使用或按配置过期。

**验收测试**:

- 无 token 注册失败。
- 非 admin 用户注册失败。
- admin Bearer / HttpOnly cookie 可注册 runner。
- runner runtime token 不能调用普通用户 API。

#### 9. 外部 Runner Docker 策略 fail closed

**状态（2026-07-05）**: 已完成。`ironforge-runner` 指定 `image` 的 job 在 Docker 不可用时直接返回失败日志，不再回退到 `run_job_local`；内置 runner 既有 fail-closed 策略保持一致。已补 `docker_unavailable_message_is_fail_closed` 测试，并通过 `cargo test -p rg-runner docker_unavailable -- --nocapture` 与 `cargo check -p rg-runner`。

**问题**: 外部 runner 在 Docker 不可用时回退 local，与内置 runner 的安全预期不一致。  
**主要文件**:

- `crates/rg-runner/src/main.rs`
- `crates/rg-cli/src/main.rs`

**建议做法**:

- job 指定 container/image 时，Docker 不可用直接失败。
- 只有 job 明确使用 local executor，才允许在宿主执行。
- 日志中明确 executor、image、工作目录和失败原因。

#### 10. 修复部署示例的默认 secret 和 Prometheus target

**状态（2026-07-05）**: 已完成。主 compose 不再内置被启动校验拒绝的 `change-me-in-production`，改为通过 `deploy/.env` 提供 `IRONFORGE_JWT_SECRET`；新增 `deploy/.env.example` 和部署 README 生成 secret 步骤。主服务和观测栈共享固定 Docker network `ironforge-net`，Prometheus target 已改为 `ironforge:8080`。已通过两个 compose 文件的 `docker compose config` 静态校验。

**问题**: compose 示例使用会被启动校验拒绝的默认 JWT secret；Prometheus target 与服务默认端口不一致。  
**主要文件**:

- `deploy/docker-compose.yml`
- `deploy/prometheus/prometheus.yml`
- `ironforge.example.toml`
- 可新增 `deploy/.env.example`

**建议做法**:

1. compose 中改为从 `.env` 读取 `IRONFORGE_JWT_SECRET`。
2. `.env.example` 只给生成命令，不给可直接用于生产的弱密钥。
3. Prometheus target 对齐 compose service name 和实际端口。

**验收测试**:

- 按部署文档生成 secret 后，compose 可启动到 `/health` healthy。
- Prometheus 能抓到 `/metrics`。

#### 11. 增加正式 CI workflow

**状态（2026-07-05）**: 已完成首版 GitHub Actions。`.github/workflows/regression.yml` 在 PR、main push 和手动触发时运行 Rust test compile、`rg-http` 集成测试、workspace tests、fresh SQLite migration smoke、前端 `npm run check/build` 和两个 compose 文件的 `docker compose config` 静态校验。后续可把 `scripts/full-interface-regression.mjs` 的 runtime/browser smoke 拆成按需 job。

**问题**: 当前回归脚本完整，但没有托管 CI 自动执行。  
**建议文件**:

- `.github/workflows/regression.yml` 或 `.gitlab-ci.yml`

**建议分层**:

1. backend quick：`cargo test -p rg-http`
2. workspace：`cargo test --workspace`
3. frontend：`cd web && npm ci && npm run check && npm run build`
4. migration smoke：fresh SQLite DB 执行 migrate + 关键 API smoke
5. full regression：按需运行 `scripts/full-interface-regression.mjs`

---

### Wave 4：P1 安全和运维硬化

| 任务 | 主要文件/范围 | 验收标准 |
|------|---------------|----------|
| LDAP TLS 默认校验证书 | `rg-core/src/auth/ldap.rs` | ✅ 2026-07-05 已完成：默认不调用 `set_no_tls_verify(true)`，测试环境需显式 insecure |
| SSO callback 浏览器登录闭环 | `rg-http/src/api/sso.rs`、`web/src/routes/login/+page.svelte` | ✅ 2026-07-05 已完成：callback 设置 HttpOnly cookie 并 redirect，MFA 用户回到登录页继续二步验证 |
| trusted proxy 配置 | `rg-http/src/rate_limit.rs`、配置结构 | ✅ 2026-07-05 已完成：默认忽略转发头，只有 `trusted_proxies` 命中的代理来源才读取 `X-Forwarded-For` / `X-Real-IP` |
| CI_JOB_TOKEN HTTP 接入 | `rg-http/src/api/auth.rs`、`repo_content.rs`、`archive.rs`、`packages.rs` | ✅ 2026-07-05 已完成：同仓库 `repo:read` 可读 repo content/archive，同仓库 `packages:read` 可读 package；跨 repo token 拒绝 |
| MCP runtime 修复 | `crates/rg-mcp/src/main.rs` | ✅ 2026-07-05 已完成：stdio 入口创建单 worker Tokio runtime，tools/resources 的 async HTTP 调用不因缺少 runtime panic |
| `/metrics` 未初始化保护 | `rg-http/src/metrics.rs` 或路由 handler | ✅ 2026-07-05 已完成：registry 未初始化时返回 503 文本响应，不再 panic |
| SQLite backup/restore | `rg-cli/src/main.rs`、文档 | ✅ 2026-07-05 已完成：新增 `backup-db` / `restore-db` CLI，部署文档补在线备份和离线恢复命令 |
| Artifact 文件链路 | `rg-http/src/api/artifacts.rs`、`rg-core`/`rg-db` | ✅ 2026-07-05 已完成：支持 runner raw body 上传、服务端保存、artifact 下载和 repo read 权限校验 |
| Docker runtime 多二进制镜像 | `Dockerfile`、`deploy/docker-compose.yml`、部署文档 | ✅ 2026-07-05 已完成：runtime 镜像包含 `ironforge`、`ironforge-runner`、`ironforge-mcp`，可选 runner 示例改用独立 runner 二进制 |
| Git receive-pack pre-receive 分支保护 | `rg-git/src/protocol/receive_pack.rs`、`rg-http/src/lib.rs`、`rg-ssh/src/lib.rs` | ✅ 2026-07-05 已完成：HTTP/SSH Git push 在 pack/ref 更新前按 protected branch 规则生成 rejected refs 并返回 `ng`，避免事后审计才发现 |
| job log WebSocket 前端接入 | `web/src/lib/api/client.svelte.ts`、`web/src/routes/**/pipelines/**` | ✅ 2026-07-05 已完成：pipeline job 日志弹窗订阅 `/ws/job/{job_id}`，实时追加日志 chunk，并在关闭/切换时断开 |
| MCP SSE 口径 | `rg-mcp`、文档 | ✅ 2026-07-05 已完成：文档统一 stdio-only，`--sse` 返回非零错误，避免误当可用 transport |
| CSP connect-src 跨域 API/WS | `rg-http/src/security.rs`、部署文档 | ✅ 2026-07-05 已完成：`connect-src` 从 CORS origins 和显式 CSP origins 生成，并自动补 HTTP(S) 对应 WS(S) origin |
| README/CONTRIBUTING 回归入口 | `README.md`、`CONTRIBUTING.md` | ✅ 2026-07-05 已完成：移除旧 E2E 脚本口径，统一到现有 `scripts/` 自动化回归入口 |
| Package fallback 标注 | `web/src/lib/packageFormats.ts`、package 页面、文档 | ✅ 2026-07-05 已完成：前端区分 native adapter 与 Generic fallback，避免 17 种 type 被误读为 17 种完整专用协议 |
| API 旧领域文件 re-export | `web/src/lib/api/{auth,repos,...}.ts` | ✅ 2026-07-05 已完成：旧拆分文件改为从 `client.svelte.ts` 显式 re-export，消除重复实现漂移风险并保留兼容导入路径 |
| Markdown sanitizer allowlist | `web/src/lib/utils/markdown.ts`、`web/scripts/markdown-sanitizer-smoke.mjs` | ✅ 2026-07-05 已完成：Markdown HTML sanitizer 改为 DOMParser + 标签/属性/URL allowlist，并补 fallback smoke 测试 |
| API client WebSocket helper 拆分 | `web/src/lib/api/websockets.ts`、`client.svelte.ts` | ✅ 2026-07-05 已完成：notification/job log WebSocket helper 从主聚合 client 抽出，主入口 re-export 保持兼容 |
| API client Package Registry 拆分 | `web/src/lib/api/packages.ts`、`client.svelte.ts` | ✅ 2026-07-05 已完成：Package Registry API 从主聚合 client 抽出，主入口 re-export `packages` 保持兼容 |
| API client Runner 管理拆分 | `web/src/lib/api/runners.ts`、`client.svelte.ts` | ✅ 2026-07-05 已完成：Runner 管理 API、响应类型和 labels 归一化从主聚合 client 抽出，主入口 re-export 保持兼容 |
| API client Boards/Time/Search 拆分 | `web/src/lib/api/boards.ts`、`timeTracking.ts`、`search.ts` | ✅ 2026-07-05 已完成：Boards、Time Tracking、Search API 从主聚合 client 抽出，主入口 re-export 保持兼容 |
| API client Auth/Releases/Issues/PR/CI/Wiki 拆分 | `web/src/lib/api/auth.ts`、`releases.ts`、`issues.ts`、`pulls.ts`、`pipelines.ts`、`wiki.ts` | ✅ 2026-07-05 已完成：Auth、Releases、Issues、Pull Requests/Reviews、Pipelines、Wiki API 从主聚合 client 抽出，主入口 re-export 保持兼容 |
| API client 剩余领域拆分 | `repos.ts`、`collaborators.ts`、`labels.ts`、`notifications.ts`、`orgs.ts`、`branchProtections.ts`、`mirrors.ts`、`webhooks.ts`、`imports.ts`、`milestones.ts`、`tokens.ts`、`mfa.ts`、`admin.ts` | ✅ 2026-07-05 已完成：剩余领域 API 全部从主聚合 client 抽出，`client.svelte.ts` 降至 38 行纯 re-export |

---

## 推荐 PR 切分

| PR | 内容 | 风险控制 |
|----|------|----------|
| PR-1 | Cookie/Bearer 统一、MFA disable、SSO state | 只动认证入口和相关测试，保留 PAT/Git HTTP 行为 |
| PR-2 | Pipeline、Package、OCI、SSH repo-scoped 权限 | 每类资源补私有仓库权限测试 |
| PR-3 | Runner 注册模型、Docker fail closed | 新旧配置兼容期明确，runner token 不复用用户 token |
| PR-4 | Compose secret、Prometheus target、CI workflow、fresh DB smoke | 先让 CI 跑 quick/backend/frontend，再逐步扩 full regression |
| PR-5 | LDAP TLS、trusted proxy、MCP runtime、metrics、backup/artifact/job log | P1 硬化逐项合入，避免一次性扩大风险 |

---

## 回归验证矩阵

每个 PR 至少执行：

```bash
cargo fmt --check
cargo test -p rg-http
```

涉及跨 crate 行为时执行：

```bash
cargo test --workspace
```

涉及前端或 API contract 时执行：

```bash
cd web
npm run check
npm run build
```

合并前完整回归建议：

```bash
node scripts/full-interface-regression.mjs
```

部署相关修复还应单独验证：

```bash
docker compose -f deploy/docker-compose.yml up --build
curl -fsS http://localhost:8080/health
curl -fsS http://localhost:8080/metrics
```

---

## 关键回归用例清单

| 领域 | 用例 |
|------|------|
| Auth | cookie-only `/users/me`、Admin cookie、PAT API docs、Bearer API |
| MFA | 错误密码不能 disable，正确密码可 disable |
| SSO | 缺 state、不匹配 state、正常 state |
| CI | private repo pipeline 匿名/read/write 三种角色 |
| Package | private package list/download/publish/delete 权限 |
| OCI | anonymous/private pull、read pull、write push |
| SSH | private clone/push 权限、protected branch push 拒绝 |
| Runner | 无 registration token 注册失败，合法 token 注册成功，runtime token 权限隔离 |
| Deploy | compose secret 替换后启动成功，Prometheus target 可抓取 |
| Migration | fresh DB migrate 后核心 API smoke 通过 |

---

## 文档回填要求

代码修复完成后同步更新：

- `ironforge-docs/architecture/architecture-followups-2026-07.md`：把已修复项从 P0/P1 移到“已修复”或标注完成日期。
- `ironforge-docs/architecture/project-architecture-2026-07.md`：把认证、权限、Runner、部署章节的风险提示改为当前事实。
- `ironforge-docs/architecture/frontend-backend-structure-2026-07.md`：如 API client 或页面接入发生变化，更新前后端映射。
- `AGENTS.md` / `AGENT.md`：只同步长期约定和踩坑，不把临时计划塞入入口文档。

---
