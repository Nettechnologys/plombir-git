# IronForge 分模块系统分析报告

> **生成日期**: 2026-07-07
> **分析基线**: `main` 分支，最近提交 `ebb6728`（2026-07-06）
> **事实来源**: `ironforge-docs/` 下 9 份现行文档 + 当前代码库度量（LOC/测试/构建/依赖）
> **配套文档**: `project-architecture-2026-07.md`、`frontend-backend-structure-2026-07.md`、`architecture-followups-2026-07.md`、`audit-report-2026-07-03.md`、`gitea-vs-ironforge-2026.md`、`ironforge-improvement-analysis-2026-06-09.md`、`source-optimization-analysis-2026-06-17.md`、`functional-test-guide-2026-07-03.md`

---

## 0. 执行摘要

IronForge 是一个以 Rust（Axum 0.8 + SeaORM 1.1 + gix 0.84 + russh 0.51）后端 + SvelteKit 5 前端实现的轻量级 Git 托管平台，对标 Gitea 1.26。系统已完成 Phase 1–21，整体功能完成度约 **85%**，P0/P1 安全、权限、部署缺口已完成首轮修复（33 项缺陷于 2026-07-04 全部清零）。

**规模指标（本次实测）**:

| 维度 | 数值 |
|------|------|
| Rust crate 数 | 9（含 3 个二进制：`ironforge` / `ironforge-runner` / `ironforge-mcp`） |
| Rust 代码总量 | ~62,200 行 / 295 文件 |
| 单元测试函数 | 261 个（另 `rg-http` 有 22 个集成测试文件） |
| 编译状态 | 0 警告 / 0 错误（2026-07-04 起稳定；本次后台 `cargo check` 复验中） |
| 前端 | SvelteKit SPA，~50 页面路由 + 20+ API client 模块 + 中英文 i18n |
| 依赖关系 | 无循环依赖 |

**最关键的 3 个结构性判断**:

1. **`rg-core` 已从业务层膨胀为"超级模块"**（82 文件 / 17,455 行 / 83 测试），横切关注点（audit/notification/search/webhook）与业务实体混杂，是长期维护成本的核心来源。
2. **Git 协议层已完成"CLI 收敛"但仍是技术债焦点**：`raw Command::new("git")` 在 `rg-git` 之外已清零（防回归守卫通过），但 `GitCommandGateway` 内部仍有 ~16 类操作走 git CLI（Diff/Fetch/Rebase/Pack/GPG/Clone），gix 原生迁移因上游能力阻塞停留在 ~70%。
3. **测试分布极不均匀**：`rg-core`/`rg-http` 测试较充分，`rg-cli`/`rg-mcp`/`rg-runner`/`rg-ssh` 几乎为 0；数据库层虽有 132 个索引但仅 11 个测试。

---

## 1. 架构分层与依赖方向

依赖方向（来自 `project-architecture-2026-07.md` §4，本次实测一致）：

```
rg-cli    → rg-ci, rg-core, rg-db, rg-git, rg-http, rg-ssh
rg-http   → rg-core, rg-db, rg-git
rg-ssh    → rg-core, rg-db, rg-git
rg-core   → rg-db, rg-git
rg-ci     → rg-core, rg-db   （注：rg-ci 通过 CiTrigger trait 注入 HTTP，已解耦）
rg-db     → 无本地依赖
rg-git    → 无本地依赖
rg-runner → 无本地依赖（独立 HTTP 客户端型二进制）
rg-mcp    → 无本地依赖（独立 HTTP 客户端型二进制）
```

**判断**: 分层整体合理、无循环。但存在两点边界张力：
- `rg-http` 早期直接依赖 `rg-ci`（已被 `CiTrigger` trait + DI 解耦，见 `audit-report` M-14），当前边界已收敛。
- `rg-core` 内部 25 个子模块尚未物理拆分，逻辑边界靠目录约定维持。

---

## 2. 逐模块分析

### 2.1 rg-cli（主二进制 / 进程入口）

| 指标 | 值 |
|------|-----|
| 文件数 | 1（`main.rs`） |
| 代码行数 | 1,618 |
| 测试数 | 0 |

**职责**: `serve` / `migrate` / `create-repo` / `import` / `index` / `package` / `runner` / `backup-db` / `restore-db` 等子命令。启动链路：`Cli::parse → run_serve → 配置/JWT/日志 → repo_root → GitCommandGateway → DB 连接 → run_migrations → AppState → spawn HTTP + SSH`。

**优势**: `main()` 已从 668 行拆分为 `run_serve()`（2026-06-17 优化），可维护性改善；`backup-db`/`restore-db` 已用 `VACUUM INTO` 实现 SQLite 热备。

**风险**:
- **零测试**：CLI 命令的参数解析与错误路径无单测覆盖，回归只能靠手工 `functional-test-guide` 全链路。
- **配置耦合**：TOML 与 env 混合加载，JWT secret 已支持 `IRONFORGE_JWT_SECRET` 优先（安全化完成），但配置项仍在持续扩张。

**建议**: 为子命令参数与配置加载补充 `#[cfg(test)]` 单测；将 `run_serve` 进一步拆分为 `build_app_state()` 便于集成测试。

---

### 2.2 rg-core（业务核心服务层）— 系统最大模块

| 指标 | 值 |
|------|-----|
| 文件数 | 82 |
| 代码行数 | 17,455（全系统最大） |
| 测试数 | 83 |
| 子模块数 | 25（Identity / Collaboration / Delivery / Infrastructure 四域） |

**职责覆盖**（来自 `frontend-backend-structure-2026-07.md` §2.2）：认证、用户、仓库、Issue、PR、Review、Wiki、LFS、Webhook、CI bridge、Package Registry、Org/Notification、SSO/MFA/LDAP、Audit、Search、Mirror/Import、Board/Time Tracking、Platform helpers。

**完成度**: 功能层面最完整。Gitea 对比中核心 Git 托管、Issue/PR、Wiki、CI、看板、时间追踪、镜像、LFS、组织、分支保护、协作者、企业认证（LDAP/OAuth2/TOTP）、审计、导入均与 Gitea 持平或接近（85%）。

**优势**:
- 已建立 `CoreError` 枚举（`NotFound/Forbidden/Conflict/InvalidInput/Internal`），逐步替代 148 处 `anyhow`（2026-06-17）。
- 权限检查已引入 `PermissionCache`（tokio RwLock，30s TTL），缓解每请求 2–4 次 DB 查询（H-4/M-5 已修复）。
- 事务保护已补 `create_issue`（M-13 修复），但覆盖仍窄。

**风险（核心结构性债务）**:
1. **模块膨胀**：25 个子模块 / 17k 行集中于单 crate，增量编译慢、跨模块耦合高。完整物理拆分预估 6 人天（被多次列为 P2 但始终未排期）。
2. **事务覆盖近零**：除 `issue_label_ops` 与 `create_issue` 外，PR merge→branch+status+audit、镜像、协作者等多表写入无事务（M-13 仅部分修复）。部分失败会导致数据不一致。
3. **横切关注点混杂**：`audit`/`notification`/`search`/`webhook` 与实体模块同处一 crate，难以独立测试与部署。
4. **测试密度偏低**：83 测试 / 17,455 行 ≈ 4.8/千行，认证/权限等安全关键路径无覆盖率门槛。
5. **`rg-core` 直接 spawn git CLI**：merge/rebase/diff 通过 `GitCommandGateway` 调用，但 PR merge 的 gix 操作已用 `spawn_blocking` 包裹（P0 #3 已修复），rebase 路径 `gix::open` 已从 8 次降到 1 次（P1 #7）。

**建议**:
- P2：从 `rg-core` 拆出 `rg-notification`（WebSocket+SMTP）与 `rg-search`（FTS5）为独立 crate。
- P1：为 PR merge / 镜像 / 协作者批量写入补 SeaORM 事务。
- P2：对 `auth`/`repo`/`pull_request` 服务设 llvm-cov 行覆盖率门槛（≥75%）并 CI enforce。

---

### 2.3 rg-git（Git 协议层）

| 指标 | 值 |
|------|-----|
| 文件数 | 8 |
| 代码行数 | 2,851 |
| 测试数 | 24 |

**职责**: `pkt_line`（编解码 + V2 Delim/ResponseEnd）、`sideband`（band 1/2/3）、`upload_pack`、`receive_pack`、`v2`（ls-refs/fetch/object-info）、`cli_gateway`（Git CLI 统一入口）。HTTP Git 与 SSH Git 复用同一协议层。

**优势**:
- 已实现 Smart HTTP + Protocol V2 的 ls-refs、fetch、object-info、shallow/deepen 和 partial-clone filter。2026-07-14 起真实 Git 客户端矩阵覆盖 HTTP/SSH、V1/V2 的 clone/fetch、V1 push、depth/deepen/unshallow/shallow-exclude/shallow-since，以及 HTTP `blob:none`/SSH `tree:0` promisor lazy fetch，并进入主 CI。
- **`GitCommandGateway` 收敛成功**：全代码库 `raw Command::new("git")` 调用已清零（仅在 `cli_gateway.rs` 内部及其回归守卫测试中出现），13 个文件引用网关。
- receive-pack 已在写 ref 前接入 protected-branch 拒绝（`ng` 前置拦截，Wave 4 修复）。

**风险（gix 迁移技术债）**:
- **gix 原生迁移 ~70%**：Diff×4、Fetch×2、Rebase×4、Pack×3、GPG×2、Clone×1 仍经网关走 git CLI。Rebase 因 `gix-rebase` 处 "idea" 阶段、Pack 因缺高层协商 API、GPG 因需 sequoia/pgpgme 而阻塞（source-optimization #16）。
- **文档口径不一致**：`source-optimization-analysis` 称 ~85%，`gitea-vs` 称 70%，`working memory` 称 70% 且 16–19 处 CLI fallback。建议统一为"~70% 路径完成，CLI 调用已全部经网关收敛，但 16 类操作仍依赖 git CLI 二进制"。
- V1 upload-pack 的 pack 生成仍偏简单，大仓库性能待优化。

**建议**: 每次 gix 升级复查阻塞表；优先迁移可对拍验证的只读路径（如 `git diff` 的 gix tree-diff 已可用）；在 `CLAUDE.md` 统一口径。

---

### 2.4 rg-ssh（SSH 服务端）

| 指标 | 值 |
|------|-----|
| 文件数 | 1 |
| 代码行数 | 633 |
| 测试数 | 4 |

**职责**: russh 0.51 服务端，`auth_publickey`/`auth_password` 查 DB；exec path 分发 `git-upload-pack`/`git-receive-pack`。

**优势**: SSH Git V1/V2 完整；exec path 已接入 repo-level `can_read`/`can_write`（Wave 2 修复）；SSH 启动失败不阻塞 HTTP。

**风险**:
- **测试几乎为零**（4 个）：认证与协议分发的回归只能靠手工 SSH clone/push（T06）。
- 单文件 633 行，auth 与 dispatch 逻辑未拆分。
- Host Key 持久化在 2026-06-09 被列为 P2，当前 `host_key` 缺失时自动生成 ed25519——需确认重启后是否沿用同一 key（否则 SSH 客户端报 host key changed）。

**建议**: 为 auth 路径补单元测试（key fingerprint 校验、password 查 DB）；确认 host key 持久化落 `data/ssh/`。

---

### 2.5 rg-http（HTTP / REST / Git HTTP / OCI / WebSocket 聚合层）

| 指标 | 值 |
|------|-----|
| 文件数 | 45 |
| 代码行数 | 21,802（全系统最大单 crate） |
| 测试数 | 35（单元）+ 22 集成测试文件 |

**职责**: Axum 0.8 服务端；承载 REST（`/api/v1`，30+ API 模块）、Git HTTP（`/git` + root）、OCI（`/v2`）、WebSocket（通知 / job log）、OpenAPI（`/api-docs`）、SPA fallback。中间件含 metrics、CSP nonce、request-id、CORS、rate-limit、maintenance、PAT-to-Bearer、docs-auth。

**优势**:
- 30+ API 模块覆盖用户/仓库/Issue/PR/Wiki/CI/包/组织/通知/审计/SSO/MFA/Board/TimeTracking/Mirror/Import 等，后端能力全面。
- 安全中间件已硬化：CSP per-request nonce（H-2 修复）、CORS 白名单（H-1 修复）、Rate Limit 默认只信 socket IP（Wave 4 修复）、`/metrics` 未初始化返回 503（不 panic）。
- 认证已统一为 `AuthUser` FromRequestParts extractor（H-3 修复），cookie-aware 模型覆盖关键用户/Admin/SSO 路径。
- API 错误响应已统一为 `AppError`（H-10/H-11 修复）。

**风险**:
1. **单 crate 过大**：21,802 行 / 45 文件，`api/` 扁平结构（历史文档误称 `routes/`/`middleware/` 目录，已在 L-2 修正）。新 handler 直接挂在 `lib.rs`。
2. **handler 边界不统一**：多数走 `rg-core` service，部分直接调 `rg-db::ops`（架构文档 §6.2 指出），与"handler → core service → db ops"目标仍有距离。
3. **WebSocket 隔离**：C-1（全量广播无用户隔离）已于 2026-07-03 修复为 per-user channel，但 job_log 仍走独立 channel，长期建议 per-repo channel（P2 #9）。
4. **集成测试覆盖关键路径但偏薄**：22 个测试文件主要覆盖权限（ci/package/oci/runner/auth），PR merge 三策略、SSH 全链路等仍缺（improvement #7）。

**建议**: 每个业务模块暴露 `pub fn router() -> Router<AppState>` 顶层 `.merge()`（P2 #9）；继续将直连 `rg-db::ops` 的 handler 收敛到 core service；补 PR merge / SSH push 集成测试。

---

### 2.6 rg-db（数据层）

| 指标 | 值 |
|------|-----|
| 文件数 | 146（多为 SeaORM 生成的 entity） |
| 代码行数 | 14,489 |
| 测试数 | 11 |
| DB 索引 | 132 |

**职责**: SeaORM entities（Identity/Repo/Issue/PR/Wiki/CI/Org/Notification/Package/OCI/扩展/搜索 FTS5）、ops、migrations（m000001 系列 + m20260508/m20260607 系列）。

**优势**:
- 132 个索引覆盖 63 个排序查询，查询性能基础良好。
- 迁移在 `serve`/`migrate` 启动时自动执行；已有多次纠偏迁移修复表名单复数不一致。
- SQLite WAL + `busy_timeout` + 写连接池调优已落地（P0 完成）。
- FTS5 同时用于仓库/Issue/Wiki/代码搜索。

**风险**:
1. **测试几乎为零**：11 个测试仅覆盖 `issue_ops`/`label_ops` 的批量查询，绝大多数 `ops/*` 无测试（P1 #6）。
2. **软删除策略不统一**：仅 `repo` 有 `deleted_at`，`user`/`org`/`issue`/`package` 仍硬删除，导致审计日志 `actor_id` 外键可能悬空（P2）。
3. **迁移命名混用**：`m000001_create_users` 与 `m20260508_000001_*` 两种风格并存，执行顺序靠时间戳+序号约束，长期有风险（P2）。
4. **PostgreSQL 不支持**：SeaORM 多后端能力未启用，FTS5→tsvector、AUTOINCREMENT→SERIAL 等迁移未验证（P3）。

**建议**: P2 补 `ops/*` 基础 CRUD 测试；补 `user`/`org`/`issue`/`package_version` 软删除 + `SoftDeleteEntity` trait；迁移命名统一为 `m{YYYYMMDD}_{HHMMSS}_{desc}`。

---

### 2.7 rg-ci（CI/CD 引擎）

| 指标 | 值 |
|------|-----|
| 文件数 | 4 |
| 代码行数 | 1,841 |
| 测试数 | 17 |

**职责**: `.ironforge-ci.yml` 原生格式解析 + Gitea Actions 兼容转换 + Pipeline 执行器 + Docker Runner + job log 写队列。通过 `CiTrigger` trait 注入 `rg-http`，保持解耦。

**优势**:
- CI 完整：YAML 解析、pipeline/stage/job 记录、内置 runner、外部 runner long-poll、job log 队列、runner labels、CI job token。
- job log 已用 `tokio::sync::mpsc` 写队列缓冲（P1 完成，缓解 `SQLITE_BUSY`）。
- 外部 runner 指定 image 时 Docker 不可用直接 fail-closed（Wave 4 修复）。

**风险**:
1. **Gitea Actions 兼容性缺失**：自研引擎不兼容 GitHub Actions 生态（对比完成度仅 30%），是最大功能差距。
2. **零集成测试**：17 个单测覆盖解析，但 PR merge 触发、runner 调度、re-run 失败 job、Concurrency 控制等缺测试（P2）。
3. **CI YAML 不支持语法静默忽略**：`${{ env.VAR }}`/`needs.outputs`/`matrix` 不支持时应 `warn!()` 而非静默（improvement #7）。
4. **Runner Watchdog 60s**：快速 job 崩溃后最多等 60s 才重调度（建议自适应 15s）。

**建议**: 明确是否投入 Gitea Actions 兼容层（6–8 周）还是坚持自研格式；补 pipeline 触发与 merge 策略集成测试；YAML 不支持字段显式告警。

---

### 2.8 rg-runner（独立 Runner Agent）

| 指标 | 值 |
|------|-----|
| 文件数 | 1（`main.rs`） |
| 代码行数 | 603 |
| 测试数 | 1 |

**职责**: 外部 runner 注册、轮询、执行、回传日志；通过 `/api/v1/runners` 与主服务通信。

**优势**: 独立二进制，不直接依赖内部 crate，边界清晰；注册已收紧为 admin auth token 或既有 runtime token（Wave 4 修复）；Docker 不可用 fail-closed。

**风险**:
- **几乎零测试**（1 个）：轮询/执行/日志回传逻辑无覆盖。
- 单文件 603 行，与 `rg-ci` 之间缺明确的 `PipelineSpec → ExecutionPlan` 转换层（P3）。

**建议**: 补 runner 注册/轮询/日志回传单测；定义 `ExecutionPlan` 接口使 runner 只消费规范结构。

---

### 2.9 rg-mcp（MCP Server）

| 指标 | 值 |
|------|-----|
| 文件数 | 7 |
| 代码行数 | 866 |
| 测试数 | 0 |

**职责**: `ironforge-mcp` stdio server，暴露 `list_repos`/`read_file`/`read_dir`/`get_issue`/`get_pr` 5 个 tool 与 `repo://`/`file://`/`issue://` 3 类 resource，通过 REST API + PAT 访问数据。

**优势**: 独有差异化能力（Gitea 不具备）；stdio 模式稳定，启动时显式创建 Tokio runtime 避免 panic（Wave 4 修复）；`--sse` 已明确 fail-fast，文档口径统一。

**风险**:
1. **零测试**：tools/resources 调用无覆盖。
2. **能力偏窄**：仅读操作，Issue/PR 写操作、CI 触发等写 tool 待扩展（P2）。
3. **SSE transport 缺失**：网页端 Agent 场景缺 transport（P2，等 HTTP/SSE 设计明确）。

**建议**: 补 tools/resources 冒烟测试；按需求扩展写操作 tool；SSE 待设计明确后实现。

---

### 2.10 web（SvelteKit 5 前端 SPA）

| 指标 | 值 |
|------|-----|
| 框架 | SvelteKit 2 + Svelte 5（adapter-static，SSR disabled） |
| 页面路由 | ~50 个 `+page.svelte` |
| API client | 20+ 独立领域模块 + `client.svelte.ts` 38 行 re-export |
| i18n | 中/英双语（en.json 646 keys / zh-CN.json 654 keys） |

**职责**: 登录/仓库/Issue/PR/Wiki/CI/审查/组织/通知/设置/Admin 等页面；API client 拆分到 `auth/repos/issues/pulls/pipelines/wiki/packages/runners/boards/timeTracking/search` 等领域模块。

**优势**:
- API client 已完成领域拆分（Wave 4，2026-07-05），`client.svelte.ts` 降至 38 行纯 re-export，避免重复实现漂移。
- 认证已迁移 HttpOnly cookie 主导 + 内存 token 兼容；`extract_user_id` cookie-aware（C-2/H-3 修复）。
- WebSocket 已接入通知 + CI job log（前端弹窗实时追加）。
- i18n 8 个缺失键已补齐（H-6），`repo.private` 等缺失键已修复（H-7）。

**风险**:
1. **后端有功能、前端无页面**：看板视图、时间追踪报表、Mirror 同步状态、Runner 管理、MFA 设置、Import 向导、MCP 配置等后端能力缺 Web 入口（improvement #5，部分已在 Wave 4 补齐但仍有缺口）。
2. **i18n 覆盖率无 CI 门禁**：en/zh 键结构差异仍可能因新增页面引入回归（P3）。
3. **`localStorage` 残留风险**：文档称已迁移 HttpOnly cookie，但前端内存 token 与部分路径仍需核查是否仍 `localStorage.setItem`（M-4 标注已修复，建议复验）。
4. **功能测试指南路径笔误**：`functional-test-guide` 中 `cd /Users/yuqu/vibeCodeing/ironforge` 应为 `Vbercodeing`（文档级缺陷，不影响功能）。

**建议**: P1 补全看板/时间追踪/Runner 管理页面；P3 加 `check:i18n` CI 门禁；复验前端是否仍有 `localStorage` token 写入。

---

## 3. 跨模块主题分析

### 3.1 安全与认证

| 主题 | 状态 | 说明 |
|------|------|------|
| 认证矩阵（JWT/HttpOnly Cookie/PAT/SSH Key/TOTP/LDAP/OAuth2/Runner Token/CI Job Token/OCI Token） | ✅ 完整 | 9 类凭证语义分离，不混用 |
| 认证提取统一 | ✅ 已修复 | `AuthUser` extractor（H-3） |
| CSP | ✅ 已修复 | per-request nonce（H-2） |
| CORS | ✅ 已修复 | 白名单 + `IRONFORGE_CORS_ORIGINS`（H-1） |
| Rate Limit | ✅ 已修复 | 默认 socket IP，可信代理才读转发头 |
| 密码重置时序侧信道 | ✅ 已修复 | 100ms 归一化延迟（H-5） |
| LDAP TLS | ✅ 已修复 | 默认校验证书 |
| WebSocket 用户隔离 | ✅ 已修复 | per-user channel（C-1） |
| 分支保护 pre-receive | ✅ 已修复 | receive-pack `ng` 前置拒绝 |
| JWT secret 明文 TOML | ✅ 已修复 | env 优先（P0） |

**结论**: P0/P1 安全缺口已清零，认证体系是当前系统最稳健的部分。剩余为 P2/P3 体验与文档口径。

### 3.2 数据库与持久化

- **WAL + 写池调优**：✅ 已落地，CI 日志写队列 ✅。
- **事务**：⚠️ 仅 2 处，多表写入仍无保护（M-13 部分）。
- **软删除**：⚠️ 仅 repo 表，其余硬删除（P2）。
- **PostgreSQL**：❌ 不支持，生产化方向（P3）。
- **迁移命名**：⚠️ 两套风格混用（P2）。

### 3.3 Git 技术债（gix 迁移）

- **CLI 收敛**：✅ `rg-git` 之外零 raw git CLI，全部经 `GitCommandGateway`。
- **原生迁移**：⚠️ ~70%，16 类操作仍走 CLI（Diff/Fetch/Rebase/Pack/GPG/Clone），受 gix 上游阻塞。
- **文档口径**：⚠️ 三份文档给出 70%/85% 不一致数字，需统一。

### 3.4 测试与质量

| crate | 测试数 | 评价 |
|-------|--------|------|
| rg-core | 83 | 不足（安全路径无门槛） |
| rg-http | 35 + 22 集成 | 较充分 |
| rg-db | 11 | 不足（ops 几乎无测） |
| rg-git | 24 | 不足 |
| rg-ci | 17 | 基础 |
| rg-ssh | 4 | 严重不足 |
| rg-cli | 0 | 零 |
| rg-runner | 1 | 零 |
| rg-mcp | 0 | 零 |

**总评**: 261 单测 / 62k 行 ≈ 4.2/千行，低于健康阈值（通常建议 ≥8/千行）。分布极不均匀，CLI/Runner/MCP 三大二进制无实质测试。

### 3.5 运维与可观测性

- **Prometheus `/metrics`**：✅ 已实现（含 registry 未初始化 503 保护）。
- **`/health`**：✅ 检查 DB/FS/metrics/git/smtp。
- **Docker**：✅ 多阶段构建，镜像含 3 个二进制；compose 需先生成 `.env` + 强 JWT secret。
- **GitHub Actions 回归**：✅ 已加 `regression.yml`（Rust/前端/迁移/compose）。
- **备份恢复**：✅ `backup-db`/`restore-db` CLI。
- **日志轮转**：⚠️ 文档建议 `tracing_appender` rolling，待确认是否落地。

---

## 4. 风险与优先级矩阵（剩余项）

> P0/P1 安全/权限/部署项已清零。以下为剩余中期与长期项。

| 优先级 | 模块 | 问题 | 建议 |
|--------|------|------|------|
| P2 | rg-core | 模块膨胀（25 子模块） | 拆 `rg-notification`/`rg-search` |
| P2 | rg-db | 事务覆盖近零 | PR merge/镜像/协作者补事务 |
| P2 | rg-db | 软删除不统一 | 补 `deleted_at` + trait |
| P2 | 全栈 | 测试分布不均 | CLI/Runner/MCP/SSH 补测试；核心路径覆盖率门槛 |
| P2 | rg-ci | Gitea Actions 不兼容 | 明确兼容层 or 自研路线 |
| P2 | rg-mcp | 仅读工具 / 无 SSE | 扩写工具；SSE 待设计 |
| P2 | web | 后端功能缺前端页 | 看板/时间追踪/Runner 管理 |
| P2 | rg-git | gix 迁移 ~70% | 每次升级复查；迁移只读路径 |
| P3 | rg-db | PostgreSQL 不支持 | SeaORM 多后端 + 迁移验证 |
| P3 | 文档 | gix 完成度口径不一 | 统一为"CLI 收敛完成，原生 ~70%" |

---

## 5. 结论与路线图建议

**当前系统定位**: IronForge 已从"功能验证原型"进入"功能完整的平台型服务"，安全/权限/部署首轮闭环已完成，可基于现有 `docker-compose` + 强 JWT secret 部署到中小规模生产环境。与 Gitea 1.26 相比，最大差距在 **Gitea Actions 兼容性**（30%）与 **gix 原生迁移**（70%），独有优势在 **MCP AI 集成**与 **纯 Rust 技术栈**。

**建议执行顺序**（基于既有 Phase 22 规划与本次分析修正）:

1. **质量加固（1–2 周）**：补 `rg-db::ops` / `rg-ssh` / `rg-cli` / `rg-runner` / `rg-mcp` 测试；为 `rg-core` 认证/权限路径设覆盖率门槛。这是当前最高杠杆的债务清理。
2. **数据一致性（1 周）**：PR merge / 镜像 / 协作者多表写入补事务；补软删除。
3. **模块解耦（1 周）**：从 `rg-core` 拆 `rg-notification`/`rg-search`；`rg-http` 路由模块化 `.merge()`。
4. **前端补全（1–2 周）**：看板 / 时间追踪 / Runner 管理页面；i18n CI 门禁。
5. **长期生产化（按需）**：PostgreSQL 后端、MCP SSE、Package 专用协议补全（go/conan/conda/alpine/debian/rpm/swift）、gix 后续迁移。

**关键风险预警**: 代码量近 3 周增长约 17%（54k → 62k 行），而测试增长未同步（180 → 261），债务表面仍在扩张。若不立即建立测试门槛与模块拆分纪律，维护成本将随功能叠加加速上升。

---

*报告基于 `ironforge-docs/` 现行文档与 2026-07-07 代码度量生成。构建状态以 `cargo check` 复验为准。*
