# IronForge — AI 协作上下文

> 本文件是 **Claude Code 的默认入口**，也是所有 AI 编程助手的深度参考文档。
> 提供项目关键背景、约定、踩坑记录、依赖版本速查和常见任务的操作指南。
> **如果你刚打开本项目，建议先快速浏览 `AGENT.md` 获取概览，再深入阅读本文件获取完整细节。**

---

## 文档地图（按 AI 工具）

不同 AI 工具读取文件的习惯不同，以下是指南：

| AI 工具 | 自动读取的文件 | 建议补充读取 |
|---------|-------------|------------|
| **Claude Code** | `CLAUDE.md`（本文件） | `AGENT.md` + 2026-07 架构文档 + 按任务类型从「按任务类型选读」中选 |
| **WorkBuddy** | `.workbuddy/memory/MEMORY.md` + 每日日志 | `AGENT.md` + `CLAUDE.md` + 2026-07 架构文档 |
| **Codex** | 通常读取项目根目录的 `README.md` | **`AGENT.md`** ⭐（AI 统一入口） |
| **Trae** | 通常读取 `README.md` | **`AGENT.md`** ⭐（AI 统一入口） |
| **CodeBuddy** | 通常读取 `README.md` | **`AGENT.md`** ⭐（AI 统一入口） |
| **AI Agent（.ai/）** | `.ai/README.md` | 按任务类型选读（AGENT.md / CLAUDE.md / 2026-07 架构文档） |

**如果你是其他 AI 工具且未自动读取本文件**：请先阅读 `AGENT.md`（更轻量的统一入口），然后通读本文件获取完整细节，再按任务类型从「按任务类型选读」中选择延伸阅读。

---

## 项目简介

**IronForge**（铁匠铺）是一个用 Rust 从零实现的轻量级 Git 托管平台，对标 Gitea/Forgejo。

- **二进制名**: `ironforge`（crate `rg-cli` 的 bin target）
- **目标**: 内存 <50MB、单二进制部署、全功能（仓库/Issue/PR/Wiki/CI）
- **当前阶段**: **Phase 1~20 全部完成**（核心功能 + Protocol V2 + 前端 i18n + P0 Gap + P1 增强 + CI/CD Runner + gix 迁移 + P2 功能 + 工程化）+ **Phase 21 已完成**（Package Registry / LDAP/SSO/2FA / Audit Log / Mirror / Board / Tracking / 代码搜索 / SSH V2）

---

## 仓库结构

```
ironforge/
├── Cargo.toml              # Workspace 根（统一依赖版本）
├── ARCHITECTURE.md         # 历史架构方案（当前事实以 2026-07 架构文档为准）
├── CLAUDE.md               # 本文件（AI 协作上下文）
├── CONTRIBUTING.md         # 开发规范
├── .ai/                  # AI Agent 接入规范（README + MCP配置 + prompt模板）
├── ironforge-docs/         # 分析报告（按主题分子目录，索引见 README.md）
│   ├── README.md                       # 文档索引（单一事实来源）
│   ├── architecture/                   # 架构总览/前后端结构/后续/followups/分模块/DB多后端
│   ├── analysis/                       # 改进与优化整合报告
│   ├── comparison/                     # Gitea 对比 + 差距清单
│   ├── ci/                             # CI Runner 架构
│   ├── testing/                        # 功能测试 + 审计
│   └── archive/                        # 过程文档与过时报告（追溯）
├── docs/
│   ├── p0-prd.md                   # P0 功能 PRD
│   ├── p0-system-design.md         # P0 系统设计 + 任务分解
│   ├── p0-completion-plan.md       # P0 完善方案 — 剩余缺口与实施计划
│   ├── git-protocol.md             # Git 协议实现细节与踩坑记录
│   ├── ai-agent-integration.md     # AI Agent 集成方案（三层架构）
│   └── project-audit-2026-06.md    # 项目审计与进度报告（2026-06-06）
├── crates/
│   ├── rg-cli/             # 主二进制入口（bin = "ironforge"）
│   ├── rg-core/            # 核心业务逻辑（✅ auth/user/repo/issue/pr/wiki/lfs/webhook/review/branch_protection/collaborator/org/notification/email/package_registry/mirror/board/time_tracking/import/audit/search/code_indexer）
│   ├── rg-git/             # Git 协议层（✅ 完整实现，RefUpdate 返回 push 信息）
│   ├── rg-ssh/             # SSH 服务端 russh（✅ 完整实现）
│   ├── rg-http/            # HTTP 服务端 + REST API（✅ 完整实现 + Git 协议鉴权 + 文件浏览 + 静态资源 + WebSocket + Rate Limit + 分页 + GPG）
│   ├── rg-db/              # 数据库层 SeaORM（✅ 实体+迁移+ops）
│   ├── rg-ci/              # CI/CD 引擎（✅ YAML 解析 + Pipeline 执行器 + Docker Runner）
│   ├── rg-runner/          # Runner Agent 独立二进制（bin = "ironforge-runner"）
│   └── rg-mcp/             # MCP 服务器（bin = "ironforge-mcp"，stdio-only，暴露 Tools + Resources 给 AI Agent）
└── web/                    # SvelteKit 前端（✅ 登录/仓库/Issue/PR/Wiki/CI/代码审查/组织/通知/国际化）
```

---

## 关键约定

### 命令规范

```bash
# 编译（请始终用 release 构建做集成测试）
cargo build --release

# 启动服务器
./target/release/ironforge serve \
  --repo-root /tmp/ironforge/repos \
  --http-addr 0.0.0.0:8080 \
  --ssh-addr  0.0.0.0:2222 \
  --host-key  /tmp/ironforge_host_key

# 创建测试仓库
./target/release/ironforge create-repo <owner> <repo> --repo-root /tmp/ironforge/repos
# → 创建 /tmp/ironforge/repos/<owner>/<repo>.git
```

### SSH 测试命令模板

```bash
SSH_CMD="ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null"
GIT_SSH_COMMAND="$SSH_CMD" git clone ssh://git@localhost:2222/testuser/testrepo /tmp/if_test
GIT_SSH_COMMAND="$SSH_CMD" git push origin main
```

### HTTP 路由前缀

HTTP Git 端点的路由前缀是 `/git/`（**不是** 直接 `/<owner>/<repo>`）：

```
GET  http://localhost:8080/git/<owner>/<repo>/info/refs?service=git-upload-pack
POST http://localhost:8080/git/<owner>/<repo>/git-upload-pack
POST http://localhost:8080/git/<owner>/<repo>/git-receive-pack
GET  http://localhost:8080/health
```

git clone 示例：

```bash
git clone http://localhost:8080/git/testuser/testrepo /tmp/if_http
```

---

## AI Agent 集成（MCP Server）

IronForge 通过 **MCP (Model Context Protocol)** 暴露仓库数据给 AI Agent（Claude Code / Cursor / Continue.dev 等）。

### 二进制

- **`ironforge-mcp`** — `rg-mcp` crate 的 bin target，位于 `target/debug/ironforge-mcp`

### 使用方式

```bash
# 编译
cargo build -p rg-mcp

# 作为子进程启动（AI Agent 会自动调用）
IRONFORGE_URL=http://localhost:8080 IRONFORGE_PAT=<token> ./target/debug/ironforge-mcp

# 测试（手动发送 JSON-RPC 请求）
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.1"}}}' | ./target/debug/ironforge-mcp
```

### 暴露的 Tools

| Tool 名称 | 说明 |
|-----------|------|
| `list_repos` | 列出当前用户可访问的仓库 |
| `read_file` | 读取仓库文件内容（UTF-8） |
| `read_dir` | 列出仓库目录内容 |
| `get_issue` | 获取单个 Issue 详情 |
| `get_pr` | 获取单个 PR 详情（含 diff） |

### 暴露的 Resources

| URI 模板 | 说明 |
|-----------|------|
| `repo://{owner}/{name}` | 仓库元数据（JSON） |
| `file://{owner}/{name}/{path}` | 文件内容（text/plain） |
| `issue://{owner}/{name}/{number}` | Issue 详情（JSON） |

### 环境变量

| 变量 | 默认值 | 说明 |
|------|--------|------|
| `IRONFORGE_URL` | `http://localhost:8080` | IronForge API 地址 |
| `IRONFORGE_PAT` | _(空)_ | Bearer Token（API 认证） |

### 支持的 Transport

- **stdio** — 作为 AI Agent 子进程运行
- **SSE** — 未实现；不要把 `--sse` 写成可用 transport

---

## 实现现状（2026-07-14）

### ✅ 已完成（Phase 1 ~ Phase 20 + P0/P1/P2 Gap Analysis + 工程化）

| 模块 | 文件 | 说明 |
|------|------|------|
| pkt-line 协议 | `rg-git/src/pkt_line.rs` | 完整编解码 + **V2 Delim/ResponseEnd** |
| sideband-64k | `rg-git/src/sideband.rs` | band 1/2/3 |
| git-upload-pack | `rg-git/src/protocol/upload_pack.rs` | SSH + HTTP 模式 |
| git-receive-pack | `rg-git/src/protocol/receive_pack.rs` | SSH + HTTP 模式，返回 `Vec<RefUpdate>` |
| **Git Protocol V2** | `rg-git/src/protocol/v2.rs` | **ls-refs + fetch + object-info；HTTP/SSH 共用 capability 列表；真实 Git V2 clone/fetch/shallow/deepen/partial-clone 回归已进入 CI；广告 `fetch=shallow filter`** |
| **V2 HTTP 集成** | `rg-http/src/git_v2.rs` + `rg-http/src/lib.rs` | **Git-Protocol: version=2 header 检测 + V2 处理** |
| SSH 服务端 | `rg-ssh/src/lib.rs` | russh 0.51，auth_publickey/auth_password 查 DB；真实注册公钥 push/clone 回归已覆盖 |
| HTTP 服务端 | `rg-http/src/lib.rs` | Axum 0.8，/git/ 路由 + **Git 协议权限鉴权** + 分支保护审计 + **SvelteKit 静态资源** |
| REST API | `rg-http/src/api/` | Users + Repos + Issues + PRs + Wiki + LFS + Webhooks + CI/CD + **Reviews + Branch Protection + Collaborators + Repo Content** |
| 数据库实体 | `rg-db/src/entities/` | users / repositories / ssh_keys / access_tokens / issues / issue_comments / pull_requests / milestones / wiki_pages / lfs_objects / webhooks / webhook_deliveries / pipelines / pipeline_stages / pipeline_jobs / **pr_reviews / review_comments / protected_branches / repo_collaborators** / **labels / issue_labels / repo_watches / commit_statuses / release_assets** |
| DB 迁移 | `rg-db/src/migrations/` | m20260424_000001~000009 + m20260508_000001~000006 + m20260510_000001~000004 + m20260511_000001~000003，自动 up on start |
| 用户认证 | `rg-core/src/auth/` | argon2 password hash + JWT HS256 |
| 用户服务 | `rg-core/src/user/service.rs` | register / login |
| 仓库服务 | `rg-core/src/repo/service.rs` | create_repo + can_read/can_write（**集成 collaborator 权限**） |
| Issue 服务 | `rg-core/src/issue/service.rs` | CRUD + labels + milestone + comments |
| Issue/PR Markdown Template | `rg-core/src/issue_template.rs` + `rg-http/src/api/issues.rs` | 按 Gitea 1.26.4 兼容目录从默认分支发现 Markdown 模板，解析 front matter / chooser config，Web 支持 Issue 选择预填与 PR 正文预填；YAML Issue Form 仍由 ISSUE-102 跟踪 |
| Issue/PR/评论附件 | `rg-core/src/attachment.rs` + `rg-http/src/api/attachments.rs` | 四类显式归属、统一 BlobStorage、100 MiB 单文件限制、1 GiB 仓库配额、扩展名白名单、跨仓库 IDOR 防护；上传/本地下载/本地删除补偿流式处理；Web 统一附件面板、OpenAPI、桌面/移动 Issue/PR 浏览器闭环已完成 |
| PR 服务 | `rg-core/src/pull_request/service.rs` | create + diff(git CLI) + merge(3策略) + **分支保护检查** |
| Wiki 服务 | `rg-core/src/wiki/service.rs` | 页面 CRUD（DB 存储） |
| LFS 服务 | `rg-core/src/lfs/service.rs` | batch API + 对象上传/下载（磁盘存储）；HMAC 用途/仓库/OID 绑定 URL（下载 1h、上传 6h，过期 410） |
| **统一 BlobStorage** | `rg-core/src/blob_storage.rs` + `ironforge-docs/storage/blob-storage-contract-2026-07.md` | 稳定 BlobKey、原子本地 backend、盘点/删除/越界防护；LFS/Package/OCI/Artifact/Release 新写入已接入，legacy 路径可读；S3/迁移工具仍由 STORAGE-002 跟踪 |
| Webhook 服务 | `rg-core/src/webhook/service.rs` | 注册/触发/投递/HMAC-SHA256 签名 |
| CI/CD 引擎 | `rg-ci/src/` | YAML 解析 + Pipeline 执行器 + 后台运行 |
| Git 鉴权 | `rg-http/src/lib.rs` | HTTP git 协议 Bearer Token 认证 + can_read/can_write |
| **代码审查** | `rg-core/src/review/service.rs` | submit review (comment/approve/request_changes/dismiss) + inline comments |
| **PR 不可变事件流** | `rg-db/src/entities/pr_event.rs` + `rg-http/src/api/reviews.rs` | append-only 时间线，保留 Reviewer/线程/Draft/Auto-merge/Queue 历史状态 |
| **分支保护** | `rg-core/src/branch_protection/service.rs` | protected branches + require PR + require approval + required status checks |
| **协作者** | `rg-core/src/collaborator/service.rs` | repo collaborators + read/write/admin permission |
| **文件浏览** | `rg-http/src/api/repo_content.rs` | tree/blob/log/branches/tags API (git CLI) |
| **Web UI** | `web/src/routes/` | SvelteKit 5 + SPA mode（登录/注册/Dashboard/仓库/Issue/PR/Wiki/CI） |
| **前端组件** | `web/src/lib/components/` | Navbar / Layout / RepoHeader / PipelineBadge |
| **API 客户端** | `web/src/lib/api/client.ts` | REST API 全量 TypeScript 封装 |
| **认证 Store** | `web/src/lib/stores/auth.ts` | JWT 状态管理（Svelte 5 runes） |
| **Docker Runner** | `rg-ci/src/runner.rs` | CI Job Docker 容器化执行（`docker run --rm` + volume mount） |
| **Merge-group CI** | `rg-core/src/pull_request/merge_queue.rs` | FIFO 队首生成 speculative merge commit，CI 成功后才合并 |
| **Deploy Key** | `rg-http/src/api/deploy_keys.rs` + `rg-ssh/src/lib.rs` | 仓库级只读/读写 SSH Key、admin CRUD、仓库隔离与使用时间记录 |
| **CI Variables / Actions 边界** | `rg-ci/src/runner.rs` + `gitea_actions.rs` | 变量注入内置/外部 Runner；不支持的 `uses:` 显式拒绝，不静默跳过 |
| **第三方 Actions 安全模型** | `ironforge-docs/ci/adr-0001-third-party-actions-execution.md` | `CI-200` 已冻结专用外部 Runner、每 Job 临时容器、默认断网、Secret 最小注入与 fork 审批边界；runtime 尚未实现，第三方 `uses:` 继续 fail closed |
| **CI Secrets / Matrix** | `ci_secrets` + `rg-ci` runner/config | 仓库 Secret 加密存储、三类 Runner 注入与日志脱敏；原生/Actions Matrix 最多展开 256 个 job |
| **CI 工作区 / Cache** | `rg-ci` + `rg-runner` + runner workspace/cache API | 精确 commit 隔离工作区；仓库隔离 tar Cache；原生与 `actions/cache@v4`；本地/Docker/外部 Runner restore-save |
| **本地 Reusable Workflow** | `rg-ci/src/gitea_actions.rs` | 同 commit 下 `workflow_call` 递归展开、inputs、继承 Secrets、needs 依赖重写；循环/远程调用 fail closed |
| **CI 执行策略** | `pipeline_jobs.allow_failure/timeout_seconds/when_condition/if_condition` + 两类 Runner | `continue-on-error`、Job timeout、`when: manual`；静态 `if` 支持 ref/event/SHA、env/matrix、布尔/比较/字符串函数；外部 Runner 统一前置阶段门控与失败下游跳过，运行时状态条件 fail closed |
| **受保护 Environment** | `ci_environments` / `ci_environment_approvals` + Job environment gate | 原生与 Actions environment、管理员规则、指定审批人/审批数、去重审批、内置与外部 Runner 可恢复部署 |
| **CI Workload OIDC** | `/api/v1/ci/oidc/*` + Ed25519 JWKS | `CI_JOB_TOKEN` 换取 5 分钟 audience-bound 身份令牌；repo/pipeline/job/ref/SHA 声明与运行态数据库绑定 |
| **CI Retention** | `ci_retention_policies` / `ci_cache_entries` + hourly cleanup | 仓库级 Artifact/Cache 保留期、滑动 Cache 过期、磁盘与 DB 一致清理、管理员 API/UI 与手动回收 |
| **Tag 保护** | `protected_tags` + HTTP/SSH receive-pack | 通配 Tag 规则、允许用户白名单、仓库管理员 API/UI、HTTP/SSH 统一执行 |
| **签名提交强制** | `protected_branches.require_signed_commits` + receive-pack | pack 入库后、ref 更新前验证本次引入的所有 commit；HTTP/SSH 共用策略 |
| **LDAP / MFA 登录** | `rg-core/src/auth/ldap.rs` + `user::service::login_with_configured_auth` + MFA challenge cookie | LDAP 首次建号、Provider 身份绑定与管理员连接测试；RFC4515 转义/超时/TLS；MFA 必须持有五分钟第一因素挑战，失败日志与五次锁定覆盖本地/LDAP |
| **OAuth 账户表兼容** | migration `000015` | 将历史派生名 `o_auth_accounts` 安全迁移为实体使用的 `oauth_accounts`，恢复 OAuth account 查询/关联保护 |
| **OIDC Discovery / PKCE** | `rg-core/src/auth/sso.rs` + `rg-http/src/api/sso.rs` | 通用 OIDC authorize/token/refresh/userinfo 使用 discovery document；PKCE S256 与签名 state/verifier Cookie；缺 verifier fail closed；本地模拟 Provider 全链路回归 |
| **登录事件与解锁** | `/api/v1/admin/login-attempts` + `/admin/users/{id}/unlock` + Admin UI | 管理员筛选密码/LDAP/SSO/MFA 登录事件、IP、UA 与失败原因；用户页显示锁定/失败计数并可审计地解锁 |
| **多数据库后端** | `rg-db::connect_with_pool` + backend-aware migrations/FTS + `multi_backend_smoke` | SQLite/PostgreSQL/MySQL scheme 分流；2026-07-13 在真实 PostgreSQL/MySQL 上通过迁移、CRUD、计数器、FTS、并发登录锁定及服务启动 `/health` 验证；连接诊断统一隐藏 URL 密码 |
| **组织系统** | `rg-core/src/org/mod.rs` + `rg-http/src/api/orgs.rs` | CRUD + 成员管理 + 团队 + 权限 |
| **通知系统** | `rg-core/src/notification/mod.rs` + `rg-http/src/api/notifications.rs` | 创建/列表/已读/批量已读/删除 |
| **Rate Limiting** | `rg-http/src/rate_limit.rs` | Token Bucket 中间件（IP 限流 + 可配置窗口） |
| **WebSocket 通知** | `rg-http/src/ws.rs` | 实时通知按用户隔离，Job 日志按 `job_id` 独立 channel；断开自动回收，JWT + 仓库读取权限校验 |
| **邮件通知** | `rg-core/src/email/mod.rs` | SMTP 邮件（lettre + HTML 模板） |
| **组织仓库** | `rg-core/src/repo/service.rs` | org_id 关联 + find_repo_by_owner_name |
| **权限鉴权完善** | `rg-core/src/repo/service.rs` | org member + team permission → can_read/can_write |
| **TLS/HTTPS** | `rg-http/src/lib.rs` | axum-server + rustls，CLI --tls-cert/--tls-key |
| **TOML 配置** | `rg-cli/src/main.rs` | 优先级 CLI > config > defaults，ironforge.example.toml |
| **日志轮转** | `rg-cli/src/main.rs` | tracing-appender RollingFileAppender (DAILY + non-blocking) |
| **API 分页** | `rg-http/src/pagination.rs` | PaginationParams + PaginatedResponse\<T\>，5 个 list API |
| **GPG 签名** | `rg-http/src/api/repo_content.rs` | GET /repos/:owner/:name/commits/:sha/signature |
| **Git V2** | `rg-git/src/protocol/v2.rs` | Protocol V2 HTTP 支持（ls-refs/fetch 命令） |
| **前端 i18n** | `web/src/lib/i18n/` | locale store + localStorage + 中/英翻译（199 key） |
| **代码覆盖率** | `cargo-llvm-cov` | LLVM 覆盖率工具，支持 HTML/LCOV/JSON 输出 |
| **P0: Star/Watch** | `rg-core/src/repo/service.rs` + `rg-http/src/api/repos.rs` | Star 计数 + Watch 三态 + Watch 列表查询 |
| **P0: 仓库删除** | `rg-core/src/repo/service.rs` | 软删除（deleted_at）+ Git 数据清理 |
| **P0: Releases/Tags** | `rg-core/src/release/service.rs` + `rg-http/src/api/repos.rs` | 创建/编辑/删除 Release + 关联 Tag + Asset 上传 |
| **P0: Labels CRUD** | `rg-db/src/entities/label.rs` + `rg-db/src/ops/label_ops.rs` | 独立 labels 表 + issue_labels 关联表 + 颜色/描述 |
| **P0: Milestones API** | `rg-http/src/api/issues.rs` | list/create/update/delete REST API |
| **P0: API Tokens/PAT** | `rg-http/src/api/users.rs` | 创建/吊销 PAT + Bearer Token 认证 |
| **P0: Fork 仓库** | `rg-core/src/repo/service.rs` | 复制 Git 数据 + fork_id 双向关联 |
| **P0: 仓库转移** | `rg-core/src/repo/service.rs` | POST /transfer，支持用户→用户/组织 |
| **P0: Commit Status** | `rg-db/src/entities/commit_status.rs` + `rg-core/src/repo/service.rs` | upsert(repo_id,sha,context) + combined status 聚合 |
| **P0: FTS5 搜索** | `rg-core/src/search/service.rs` + `rg-http/src/api/search.rs` | repos_fts/issues_fts/wiki_pages_fts + 触发器自动同步 |
| **P1: Labels-Issue 关联** | `rg-db/src/ops/issue_label_ops.rs` + `rg-http/src/api/issues.rs` | ?labels= 过滤 + GET issue labels |
| **P1: Webhooks 扩展** | `rg-core/src/webhook/service.rs` | 13 个事件（release/branch/tag/issue/PR/milestone）|
| **P1: Watch 通知** | `rg-core/src/notification/mod.rs` | push/PR/milestone 通知（排除 actor）|
| CLI | `rg-cli/src/main.rs` | clap 4，`serve`（含 --db-url, --jwt-secret, --docker, --rate-limit-*, --smtp-*, --tls-*, --config, --log-*, --external-runners）/ `create-repo` / `migrate` / `runner` |

### ✅ Phase 13 已完成（DB 分页 + V2 + Admin，2026-04-27~28）

- PaginatedResponse 统一分页（5 个 list API）
- Git Protocol V2 HTTP 集成完善
- Admin API 增强用户管理

### ✅ Phase 14-15 已完成（P0 Gap 补齐，2026-05-08~09）

- Star/Watch、仓库删除/转移、Releases/Tags
- Labels CRUD + Issue 关联、Milestones、PAT
- Fork 仓库、Commit Status、FTS5 搜索
- Webhooks 13 事件、Watch 通知

### ✅ Phase 16 已完成（P1 增强，2026-05-09）

- Webhooks 扩展（13 个事件）
- Watch 通知集成
- Labels-Issue 关联 API

### ✅ Phase 17 已完成（CI/CD Runner 收尾，2026-05-10）

- Runner Token Bearer 认证中间件（`authenticate_runner`）
- 外部 Runner 模式（`--external-runners` flag）
- Runner Agent 独立二进制（`crates/rg-runner/` → `ironforge-runner`）
- Artifact 管理（DB 迁移 + entity + ops + API 4 端点）
- Job 日志 WebSocket 实时推送（`/ws/job/:job_id`，按 Job 隔离并校验仓库读取权限）
- Admin Runner 管理前端

### ✅ Phase 18 已完成（gix 迁移，2026-05-10）

- rg-ci CI 配置读取迁移（read_ci_config + has_ci_config → gix）
- rg-core checkout 迁移（git checkout ×2 → gix edit_reference）
- rg-core fast-forward 迁移（git merge --ff-only → gix repo.reference）
- 进度 50% → ~60%（18 → 13 处 git CLI 保留）
- **2026-06-06**: 进一步迁移 Merge×4 + Commit×2 + Ref delete×1 到 gix（进度 ~70%，16 处 CLI 保留）

### ✅ Phase 19 已完成（P2 功能，2026-05-11）

- R-14: Fork PR 跨仓库支持（DB 迁移 + resolve_head_ref + 跨仓库 compute_diff/merge_pr）
- R-15: Release Asset HTTP 端点（upload/download/list/get/delete 5 个端点）
- R-16: Search API 细分（SearchFilters qualifier 解析 + search_issues/search_repos 过滤）

### ✅ Phase 20 已完成（工程化，2026-05-11）

- Step 1: 构建优化（release profile 已有 lto/opt-level/strip）
- Step 2: 统一错误处理（AppError enum + IntoResponse）
- Step 3: SQLite 性能调优（WAL + 7 项 PRAGMA 优化 + 连接池配置）
- Step 4: 配置校验（validate_config 拒绝危险默认值）
- Step 5: 健康检查增强（/health: DB ping + FS check + version/phase）
- Step 6: Request-ID 中间件（UUID v4 + tracing span）
- Step 7: Rate Limiter（Token bucket per-IP）
- Step 8: SQL 注入防护（参数化三元组 filter_clauses）
- Step 9: 集成测试（10 个 API 测试，9 passed / 1 ignored）
- Step 10: OpenAPI 全量覆盖（142 个 utoipa::path 注解 + Swagger UI /api-docs/）

### ✅ Phase 21 已完成（2026-06-07 — 大规模功能扩展）

#### P0: Package Registry（包注册表）
- `rg-core/src/package_registry/` — PackageAdapter trait + 10 种适配器（9 native + generic fallback）
  - Cargo (sparse index) / npm (registry metadata) / PyPI (PEP 503 Simple Index)
  - Maven (maven-metadata.xml) / NuGet (service/registration/search)
  - Helm (index.yaml) / RubyGems (dependencies API) / Docker (OCI 标记)
  - Composer (packagist metadata) / Generic (通用文件，其他类型 fallback)
- `rg-core/src/package_registry/oci/` — OCI Distribution Spec v1.0 容器镜像仓库
  - `storage.rs`: 内容寻址 blob 存储
  - `types.rs`: OCI 类型定义
  - `manifest.rs`: 镜像清单处理
- `rg-http/src/oci.rs` — 11 个 `/v2/` 路由处理器（api-version/blob/manifest/tags/upload 全链路）
- `rg-db` 新增实体：package / package_version / package_file / package_registry
- `rg-db` 新增 ops：package_ops / package_version_ops / package_file_ops / package_registry_ops
- `rg-db` 迁移：m20260607_000005_create_package_registry
- `rg-http/src/api/packages.rs` — REST API + 19 个处理器覆盖所有包协议
- 非 OCI 的 9 类原生 REST/协议矩阵已覆盖发布、列表、版本、下载及专用索引 E2E；OCI 另有独立权限/协议测试
- `rg-cli` — package CLI 命令

#### P1: LDAP / SSO / 2FA 认证增强
- `rg-core/src/auth/encryption.rs` — AES-256-GCM 加密（敏感配置存储）
- `rg-core/src/auth/ldap.rs` — LDAP bind 认证
- `rg-core/src/auth/sso.rs` — OAuth2 SSO（reqwest 直接实现，未使用 oauth2 crate）
- `rg-core/src/auth/totp.rs` — TOTP 两步验证 + QR 码生成
- `rg-db` 新增实体/迁移：oauth_account / mfa_backup_code / login_log / sso_provider（m20260607_000006~000010）
- `rg-http/src/api/sso.rs` — SSO 认证流程端点
- `rg-http/src/api/mfa.rs` — MFA 设置/验证/禁用端点（8 个 REST 端点）
- 登录流程集成：密码认证后可触发 MFA step-up

#### P1: 审计日志（Audit Log）
- `rg-core/src/audit/` — audit! 宏 + record() fire-and-forget
- `audit/archiver.rs` — 服务启动默认接入 90 天归档；限量批次、唯一 NDJSON.zst、原子落盘后清理 DB；`[audit]` 可配置或关闭
- `rg-db` 迁移：m20260607_000011_create_audit_logs
- `rg-db`：audit_log 实体 + audit_log_ops
- `rg-http/src/api/audit.rs` — Admin 专用审计日志查询端点

#### P1: Mirror / Board / Time Tracking
- `rg-core/src/mirror/` — 仓库镜像同步服务
- `rg-core/src/board/` — 看板管理（Board/Column/Card）
- `rg-core/src/time_tracking/` — 工时追踪
- `rg-db` 迁移：m20260607_000001~000003（mirrors / boards / time_entries）
- `rg-http/src/api/mirrors.rs` / `boards.rs` / `time_tracking.rs`

#### P1: 数据导入（Data Import）
- `rg-core/src/import/` — GitHub/GitLab 导入（github_client + gitlab_client + service）
- `rg-db` 迁移：m20260607_000004_create_import_tasks
- `rg-db`：import_task 实体 + import_task_ops
- `rg-http/src/api/imports.rs`

#### P2: 代码搜索端点
- `rg-core/src/search/code_indexer.rs` — 代码索引器（CLI index-repo 命令）
- `rg-db` 迁移：m20260512_000001_create_code_fts（FTS5 全文搜索）
- `rg-http` — `/search/code` 端点

#### P2: SSH Protocol V2 改进
- `rg-git/src/protocol/v2.rs` — ls-refs / fetch / object-info；2026-07-14 已实现 shallow/deepen 与 partial-clone filter，统一广告 `fetch=shallow filter`

#### 工程化
- `rg-http/src/openapi.rs` — OpenAPI 注解更新
- `rg-http/src/api/runners.rs` — Runner 端点 OpenAPI 注解补充

所有 Phase 1~21 全部完成。剩余待完成项见下方。

---

## 重要踩坑（必读！）

在修改 Git 协议相关代码时，请务必了解以下已踩过的坑：

### 1. pkt-line 解析必须用 `read_pkt_line`，不能用 `read_line`

pkt-line 格式是 `<4 hex 字节长度><payload>`。长度包含自身 4 字节。
`read_line()` 会把 `004a...` 这样的长度头当成文本内容读进来，导致 UTF-8 解析失败或逻辑错误。
**正确方式**：始终使用 `rg_git::pkt_line::read_pkt_line(&mut BufReader::new(stream))`。

### 2. receive-pack 的 report-status 必须整体 sideband 封装

当服务端广告了 `side-band-64k` 能力（我们始终广告），客户端期望所有响应都通过 sideband 发送。

**错误做法**：先发 sideband flush `0000`，再发 plain pkt-lines。  
**正确做法**（已验证）：

```
① 把 report-status pkt-lines 写入内存 buf（unpack ok + ok/ng ref... + 0000）
② 整体用 sideband::write_sideband_data(writer, &report_buf) 发出（band 1）
③ 调用 sideband::write_sideband_flush(writer) 发 sideband flush
```

对应代码：`rg-git/src/protocol/receive_pack.rs` 中的 `send_response()` 函数。

### 3. russh ChannelStream 的关闭顺序

SSH 会话结束时必须按以下顺序操作，否则会丢失缓冲数据：

```rust
// ① 先发 exit-status（channel 还活着）
handle.exit_status_request(channel_id, exit_code).await?;

// ② 再 shutdown stream（发 SSH EOF，让客户端知道数据发完了）
stream.shutdown().await?;

// ③ stream drop → channel close
```

对应代码：`rg-ssh/src/lib.rs` 中 `exec_request` 的 `tokio::spawn` 块。

### 4. git push 发送的是 thin pack

客户端 `git push --thin` 发送 thin pack，服务端必须用：

```bash
git index-pack --fix-thin --stdin
```

不能用普通的 `git index-pack --stdin`，否则 pack 文件不完整。

### 5. git for-each-ref 不列出 HEAD

`git for-each-ref` 只列出 refs/heads/...、refs/tags/... 等，不包括 HEAD（符号引用）。
需要额外调用 `git rev-parse HEAD` 单独解析，且要校验返回值是 40 位 hex（空 repo 返回字面 "HEAD"）。

### 6. HTTP info/refs 路由的 Content-Type

git HTTP 协议对 Content-Type 极为敏感：

- `GET /info/refs?service=git-upload-pack` → `application/x-git-upload-pack-advertisement`
- `GET /info/refs?service=git-receive-pack` → `application/x-git-receive-pack-advertisement`
- `POST /git-upload-pack` → `application/x-git-upload-pack-result`
- `POST /git-receive-pack` → `application/x-git-receive-pack-result`

### 7. argon2 0.5 的 SaltString 用法

```rust
// 正确：
use password_hash::rand_core::OsRng;
let salt = SaltString::generate(&mut OsRng);

// 错误（rand 0.9 的 rng() 不满足 CryptoRngCore）：
use rand::rng;
let salt = SaltString::generate(&mut rng()); // ❌
```

### 8. axum 0.8 的 Router::nest() 类型约束

`Router::nest()` 要求前后 Router 的 State 类型一致。
推荐做法：把所有 route handler 先组成一个完整 Router，再统一加 `.with_state(state)`。

### 9. axum TLS 必须用 axum-server

- ❌ `tokio-rustls::TlsAcceptor` + `axum::serve(TcpStream)`：`TlsStream` 无法转 `TcpStream`
- ❌ `hyper` 直接处理：`Router` 不实现 `Service<Request<Incoming>>`
- ✅ `axum-server::bind_rustls()` + `RustlsConfig::from_config()`

### 10. serde default 函数类型匹配

`#[serde(default = "fn_name")]` 的函数返回类型必须与字段完全匹配。`Option<String>` 字段不能用返回 `String` 的函数，改用 `#[serde(default)]`（Option 自动 None）。

### 11. utoipa OpenAPI 注解注意事项

- `serde_json::Value` **不能**放在 `schemas()` 列表（不实现 ToSchema）；在 path 注解中用 `request_body(content = serde_json::Value)` 代替
- 通过 `route_layer()` 注册的路由不会被 `.route()` 正则匹配发现，`__path_*` 符号缺失需手动排除
- 添加 `use utoipa::ToSchema;` 时**不能**插入到 `use axum::{` 块内（导致 proc-macro 解析失败）
- handler 名冲突（如 `register` 同时在 users 和 runners 模块）需用 `module::handler` 做 key

### 12. SQLite FTS5 触发器的 'delete' 命令

FTS5 的 `INSERT INTO fts_table(fts_table, rowid, ...) VALUES('delete', ...)` 特殊命令**不接受内容列值**，会导致 `SQL logic error`。
**正确方式**：用标准 SQL `DELETE FROM fts_table WHERE rowid = old.id` 代替。

### 13. 迁移 `#[derive(Iden)]` 生成的是**单数**表名 ⚠️（曾导致全功能模块运行时崩溃）

迁移里写 `#[derive(Iden)] enum Organization { Table }`，SeaORM 生成的表名是 **单数** `organization`；但实体声明的是 **复数** `#[sea_orm(table_name = "organizations")]`。两者一旦不一致：
- 该实体的所有查询在运行时报 `no such table: organizations`（功能静默不可用）；
- 后续任何 `ALTER TABLE organizations` 的迁移会**让整个服务启动崩溃**（迁移在 `serve` 启动时执行）。

历史教训：phase8（`m20260424_000009`）建出 `organization`/`team`/`notification`（单数），org/team 功能长期不可用却因无集成测试未被发现，最终靠 `m20260616_0000015_rename_org_team_plural` 修正。

**正确方式**：
1. 新增表时显式指定表名（`#[sea_orm(iden = "things")]` 标注 `Table` 变体，或用 raw SQL），并确认与实体 `table_name`（复数）完全一致；
2. 用全新库验证：`ironforge migrate --db-url "sqlite:///tmp/x.db?mode=rwc"` 成功后 `sqlite3 /tmp/x.db ".tables"` 核对表名；
3. 新功能模块务必补 `crates/rg-http/tests/` 集成测试（参考 `org_tests.rs`）。

### 14. 迁移应幂等 + AppState 字段变更要同步测试构造器

- 迁移可能半执行后崩溃再重跑，`ALTER TABLE ... ADD COLUMN` 等非幂等语句要用 `manager.has_table()/has_column()` 守卫（见 `m20260616_000002_add_soft_delete_columns`）。
- 给 `AppState`（`rg-http/src/lib.rs`）新增字段时，**必须**同步更新 `crates/rg-http/tests/common/mod.rs::build_test_app_state`，否则集成测试无法编译。

---

## 开发工作流

### 新功能开发流程

1. 阅读 `ironforge-docs/architecture/project-architecture-2026-07.md` 和对应前后端结构文档确认当前代码事实
2. 必要时阅读 `ARCHITECTURE.md` 了解历史设计意图
3. 确认要修改的 crate 和文件
4. 先写单元测试（或端到端测试脚本）
5. 实现功能
6. `cargo build --release` 验证编译
7. 端到端测试验证（见 README.md 中的测试脚本）
8. 更新本文件中的"实现现状"表格

### 后续开发建议

所有 Phase 1~21 全部完成。Package Registry / LDAP/SSO/2FA / 审计日志 / 数据迁移均已实现。

**当前剩余 P0/P1 差距（按优先级，2026-06-16 代码验证）：**

> ⚠️ 2026-06-16 已完成项：密码重置、Composer 适配器、CI/CD 日志写队列、Git CLI 统一封装（6/20 处）、Pipeline 可视化、Wiki Markdown+TOC+删除、GPG 签名 UI、审计日志归档、软删除统一、Subpath 归档下载、搜索高亮+快捷键、维护模式+实例横幅、外部 CI Webhook

> 🔧 2026-06-17 回归修复：① org/team 表名单复数错配（fresh DB 启动崩溃 + org/team 运行时不可用）→ 新增 `m20260616_0000015_rename_org_team_plural` 修正；② archive 路由 axum 0.8 非法（启动 panic）+ 切片 panic；③ 软删除迁移幂等化；④ SSH 与 HTTP 生命周期解耦 + host key 缺失自动生成（零配置可用）；⑤ 登录字段 `username` 别名；⑥ 编译警告清零 + runner match 不可达分支修复；⑦ 修复测试编译（composer/AppState）+ 密码策略；⑧ 新增 `org_tests.rs` 回归守护。

#### P0（无 — ✅ 全部完成）

#### P1 余量（已解决 ✅）
1. ~~**Git CLI 替换余量**~~ — 2026-07-04 完成：全部 raw `Command::new("git")` 已通过 `GitCommandGateway` 统一（最后收尾 `repo/service.rs` 13 处：`auto_init`/`create_or_update_file`/`delete_file`；网关新增 `run_with_env` 支持 commit 身份 env），防回归守卫 `test_no_raw_git_command_in_crates` 无豁免通过。

#### 技术债
2. **gix 迁移继续** — 2026-07-04: raw git 全消除（经 GitCommandGateway），gix 原生覆盖率 ~70%，16 处 CLI 经网关保留（Diff×4/Fetch×2/Rebase×4/Pack×3/GPG×2/Clone×1）
   - ✅ Phase 1: 所有 git 子进程调用统一走 `GitCommandGateway`（防回归测试通过）
   - ✅ Phase 2A: PR diff numstat 迁移至 gix tree-to-tree diff（per-file 行数统计）
   - ✅ Phase 2B: commit gpgsig 头读取迁移至 gix `extra_headers()`
   - ⚠️ PR diff unified patch 仍走网关（gix blob-diff 字节一致性待验证 → Phase 3）
   - ⚠️ GPG 加密验签仍走 CLI（gix 无验签能力 → Phase 3）

#### Phase 3 — 等待 gix 上游成熟（不执行，仅复查）
| 待办 | 阻塞原因 | 复查 / 解除条件 |
|---|---|---|
| Rebase 合并（PR rebase ×2 路径） | `gix-rebase` 仍处 "idea" 阶段，无 API | gix 发布稳定 rebase API |
| Pack 生成（upload-pack / v2 fetch） | gix 无高层 pack 协商/生成 API | gix 提供 server 端 pack 生成 |
| Thin-pack 索引（receive-pack `index-pack --fix-thin`） | gix 缺针对现有 ODB 的 thin 补全解析 | gix `gix-pack` 支持 `--fix-thin` 等价操作 |
| 加密级 GPG 验签 | gix 无验签；需 gpgme/sequoia | gix 内建验签，或单独引入 `sequoia-openpgp` |
| 本地 bare clone（fork） | `prepare_clone_bare` 不支持本地路径 | gix 支持 file-transport bare clone |
| git archive 原生化（可选） | 非 gix 阻塞（可用 gix tree-walk + tar/flate2） | 视需要单独排期 |
| blob-diff unified patch | gix blob-diff 输出与 git diff 字节一致性待验证 | 对拍测试通过后迁移 |

复查节奏：每次 `gix` 版本升级（当前 `0.84`）时过一遍本表。

#### 维护增强
3. ~~OpenAPI 注解补全~~ ✅ 2026-06-17 — 30 端点 + 18 schema + 4 tag
4. ~~Wiki 版本历史~~ ✅ 2026-06-17 — DB + API + 前端历史页面 + diff
5. ~~看板/时间追踪前端页~~ ✅ 2026-06-17 — Kanban 看板页面 + 时间追踪页面
6. ~~PostgreSQL/MySQL 可选后端与实库 smoke~~ ✅ 2026-07-13 — 真实服务完成 migration/CRUD/counter/FTS/认证并发验证；HA、备份恢复和长期压测仍属生产化后续
7. ~~Gitea Actions 兼容层~~ ✅ 2026-06-17 — 解析器 + 转换层 + 多文件发现

---

## 依赖版本速查

```toml
axum            = "0.8"
axum-server     = "0.7"      # features: tls-rustls
tower           = "0.5"
tower-http      = "0.6"      # features: cors, trace, fs
russh           = "0.51"
russh-keys      = "0.45"
sea-orm         = "1.1"      # features: sqlx-sqlite, runtime-tokio-rustls, macros
clap            = "4"        # features: derive
tokio           = "1"        # features: full
serde           = "1"        # features: derive
serde_json      = "1"
toml            = "0.8"
tracing         = "0.1"
tracing-subscriber = "0.3"   # features: env-filter
tracing-appender = "0.2"
rustls-pemfile  = "2"
tokio-rustls    = "0.26"
lettre          = "0.11"     # default-features = false, features: tokio1-rustls-tls, builder, smtp-transport
utoipa          = "5"        # features: chrono（⚠️ 未纳入 workspace，在 rg-http 中硬编码）
utoipa-swagger-ui = "8"      # Swagger UI 嵌入（⚠️ 未纳入 workspace）
anyhow          = "1"
thiserror       = "2"
gix             = "0.84"     # features: blocking-http-transport-curl, max-performance, blob-diff, pack-cache-lru-dynamic, merge
chrono          = "0.4"      # features: serde
uuid            = "1"        # features: v4, serde
# Auth / Crypto
argon2          = "0.5"
jsonwebtoken    = "9"
password-hash   = "0.5"
rand_core       = "0.6"
aes-gcm         = "0.10"     # SSO/LDAP 敏感配置加密
hmac            = "0.12"
sha2            = "0.10"
hex             = "0.4"
base64          = "0.22"
# 2FA / TOTP
totp-rs         = "5"        # features: gen_secret, otpauth
qrcode          = "0.14"
rand            = "0.8"
# LDAP
ldap3           = "0.11"     # features: tls
# OAuth2（声明但未直接使用，SSO 通过 reqwest 手动实现）
oauth2          = "5"        # ⚠️ 未直接使用
openidconnect   = "4"        # ⚠️ 未直接使用
# 其他
zstd            = "0.13"     # LFS 压缩（⚠️ 未纳入 workspace，在 rg-core 中硬编码）
reqwest         = "0.12"     # features: json
flate2          = "1"
tar             = "0.4"
home            = "0.5"
```

---

## 常见错误排查

| 错误信息 | 原因 | 解决方案 |
|----------|------|----------|
| `fatal: the remote end hung up unexpectedly` | SSH 流关闭时机不对 | 确保按 exit_status → shutdown → drop 顺序 |
| `bad band #110` | HTTP receive-pack 响应没有 sideband 编码 | report-status 必须包在 band-1 sideband 中 |
| `bad line length character: unpa` | 发送了 plain pkt-lines 但客户端期望 sideband | 整体用 write_sideband_data 包装 |
| `stream did not contain valid UTF-8` | 用 read_line 读了 pkt-line 二进制头 | 改用 read_pkt_line |
| `nul byte found in provided data` | 向 Command::arg() 传了含 NUL 的字符串 | 先用 split('\0').next() 剥离 capabilities |
| `the feature requires unstable` | 用了需要 nightly 的 gix API | 用系统 git 命令替代 |
| `--repo-root` not found | CLI 用法错误 | 必须加 `serve` 子命令：`ironforge serve --repo-root ...` |
| `HEAD` not found in ref list | `git for-each-ref` 不列出 HEAD | 用 gix API (`repo.references().all()`) 替代，它会正确返回 HEAD |
| `fatal: not a valid ref` (HTTP clone) | Content-Type 不正确 | 确保 `info/refs` 响应使用 `application/x-git-*-advertisement` |
| `pack has delta resolution error` | thin pack 未加 `--fix-thin` | `git index-pack` 必须加 `--fix-thin` 参数 |
| handler 返回类型编译错误 | Axum handler 返回类型不一致 | 同一 handler 不能混用 `(StatusCode, Json)` 和 `Html` |
| JSON 响应 `data` 字段为空 | `PaginatedResponse` 未用 `to_value()` 包装 | 必须用 `serde_json::to_value(resp)` 包装后返回 |
| Axum TLS 报错 | 用了 `axum::serve()` 而不是 `axum_server` | TLS 必须用 `axum_server::bind_rustls()` |
| SeaORM 批量删除不生效 | 用了错误的方法 | 必须用 `Entity::delete_many().filter(...).exec(db)` |
| SeaORM 单行更新失败 | 直接构造 ActiveModel | 必须先 `find_by_id()` 再 `into_active_model()` |
| russh `fingerprint()` 编译错误 | 缺少 `HashAlg` 参数 | 必须传 `HashAlg::Sha256` |
| SSH 认证死循环 | `Auth::Reject` 未设 `partial_success: false` | 必须带 `partial_success: false` |
| FTS5 触发器语法错误 | 用了不正确的 SQL 语法 | 必须用 `DELETE FROM fts WHERE rowid = old.id` |
| 级联编译错误 | `mod.rs` 缺少子模块声明 | 检查 `mod.rs` 是否列出了所有子模块 |

---

## 与其他 AI 工具协作说明

本项目同时维护多份 AI 上下文文件，不同工具读取不同文件：

### 文档定位

| 文件 | 定位 | 适用场景 |
|------|------|---------|
| `AGENT.md` | **所有 AI 工具的轻量统一入口** | 快速了解项目概览、技术栈、关键文件速查 |
| `CLAUDE.md` | **Claude Code 默认入口 + 所有 AI 的深度参考** | 完整的踩坑记录、依赖版本、常见错误排查、实现现状清单 |
| `ironforge-docs/architecture/project-architecture-2026-07.md` | 当前代码事实架构总览 | 设计新功能、核验模块边界和运行模型 |
| `ironforge-docs/architecture/frontend-backend-structure-2026-07.md` | 当前前后端结构分布 | 修改前端页面、API client、HTTP handler 时 |
| `ironforge-docs/architecture/architecture-followups-2026-07.md` | 已修复项、P2 长期方向和旧口径修正 | 判断风险、技术债和后续方向 |
| `ARCHITECTURE.md` | 历史架构设计文档 | 了解技术选型和早期设计背景；当前事实以 2026-07 架构文档为准 |
| `CONTRIBUTING.md` | 开发规范 | 写新代码时遵循编码规范 |

### Claude Code
- **自动读取**: `CLAUDE.md`（本文件）+ `AGENT.md`
- **特点**: Claude Code 会优先读取 `CLAUDE.md`，同时会读取 `AGENT.md` 作为补充
- **建议**: 以本文件为主，AGENT.md 为辅

### Codex / Trae / CodeBuddy / 其他 AI 工具
- **自动读取**: `AGENT.md`（优先）+ `CLAUDE.md`（同时）
- **特点**: 这些工具通常优先读取 `AGENT.md` 作为统一入口，同时会读取 `CLAUDE.md` 获取深度上下文
- **建议**: 以 AGENT.md 为概览，本文件为深度参考

### WorkBuddy（本项目的主要 AI 协作工具）
- **自动读取**: `.workbuddy/memory/MEMORY.md`（长期经验） + `.workbuddy/memory/YYYY-MM-DD.md`（每日日志）
- **建议补充**: `AGENT.md` + `CLAUDE.md`（本文件）+ 2026-07 架构文档
- WorkBuddy 在每次会话开始时会自动读取记忆文件，保持跨会话的上下文连续性

### 记忆文件位置
```
$WORKSPACE/.workbuddy/memory/
├── MEMORY.md                           # 长期经验积累（踩坑、架构决策、文档阅读指南）
├── doc-code-inconsistencies.md         # 文档与代码不一致问题追踪
└── YYYY-MM-DD.md                       # 每日工作日志
```

### 分析报告位置
```
$WORKSPACE/ironforge-docs/
├── README.md                                  # 文档索引（单一事实来源）
├── architecture/                              # 架构总览/前后端结构/followups/分模块/DB多后端
│   ├── project-architecture-2026-07.md
│   ├── frontend-backend-structure-2026-07.md
│   ├── architecture-followups-2026-07.md
│   ├── system-analysis-by-module-2026-07.md
│   └── db-multi-backend-design-2026-07.md
├── analysis/improvement-analysis.md            # 改进与优化整合报告
├── comparison/                                 # Gitea 对比 + 差距清单
├── ci/ci-runner-architecture.md                # CI Runner 架构
├── testing/                                    # 功能测试 + 审计
└── archive/                                    # 过程文档与过时报告（追溯）
    ├── ARCHIVE.md                              # 归档索引
    ├── project-architecture-analysis-notes-2026-07.md  # 架构重盘过程记录
    ├── project-architecture-analysis-plan-2026-07.md   # 架构重盘分析步骤
    ├── architecture-remediation-plan-2026-07.md        # P0/P1 修复执行过程
    ├── ironforge-improvement-analysis-2026-06-09.md    # 全方位改进空间分析
    ├── source-optimization-analysis-2026-06-17.md      # 源码优化空间分析
    ├── p0-update-2026-06-08.md                         # P0 包注册表更新
    ├── frontend-layout-audit.md / plan.md / defect-*.md # 散落文件归位
    ├── gitea-feature-gap-analysis.md                   # (v1, 已由 v2.0 替代)
    ├── gix-migration-status-report.md                  # (数据已过时)
    └── gix-migration-feasibility-analysis.md           # (参考用，已归档)
```

### 项目文档位置
```
$WORKSPACE/ironforge/docs/
├── git-protocol.md                     # Git 协议实现细节与踩坑记录
├── p0-prd.md                           # P0 功能 PRD（产品需求）
├── p0-system-design.md                 # P0 系统设计 + 任务分解
├── p0-completion-plan.md               # P0 完善方案 — 剩余缺口与实施计划
├── ai-agent-integration.md             # AI Agent 集成方案（三层架构）
└── project-audit-2026-06.md            # 项目审计与进度报告

$WORKSPACE/.ai/
└── README.md                           # AI Agent 接入指南（MCP + REST API）

$WORKSPACE/deploy/
└── README.md                           # Observability Stack 部署说明
```
