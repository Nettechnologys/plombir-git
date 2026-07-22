# IronForge 项目架构重盘分析记录

**创建日期**: 2026-07-05  
**分析计划**: `ironforge-docs/project-architecture-analysis-plan-2026-07.md`  
**目标**: 为新版项目架构文档和前后端结构分布文档沉淀逐轮分析结果

---

## 第 0 轮：确定分析基线

### 读取文件和命令

已读取或采集：

- `ironforge-docs/project-architecture-analysis-plan-2026-07.md`
- `AGENT.md`
- `AGENTS.md`
- `ARCHITECTURE.md`
- `README.md`
- `ironforge-docs/README.md`
- `Cargo.toml`
- `web/package.json`

已执行命令：

```bash
git status --short
git branch --show-current
git rev-parse --short HEAD
git log -1 --format='%h %ci %s'
rg --files -g 'Cargo.toml' -g 'package.json' -g '*.md'
cargo metadata --format-version 1 --no-deps
find . -maxdepth 2 -type d
find crates -maxdepth 2 -name Cargo.toml
find web -maxdepth 2 -type f \( -name 'package.json' -o -name 'svelte.config.*' -o -name 'vite.config.*' -o -name 'tsconfig.json' \)
find crates/rg-http/tests -maxdepth 1 -type f
```

### 分析基线

| 项目 | 当前值 | 证据 |
|------|--------|------|
| 分支 | `main` | `git branch --show-current` |
| 基线提交 | `9088e2a` | `git rev-parse --short HEAD` |
| 最新提交 | `9088e2a 2026-07-04 02:05:50 +0800 Merge origin/main: H-2/M-3/M-4/M-5 安全修复（CSP nonce + JWT HttpOnly + fetch timeout + WS subprotocol auth）` | `git log -1` |
| 分析日期 | 2026-07-05 | 本轮记录创建时间 |
| 后端 workspace | 9 个 Rust crate | `Cargo.toml` + `cargo metadata --no-deps` |
| 前端项目 | SvelteKit/Vite 项目，目录为 `web/` | `web/package.json` |
| 主要测试目录 | `crates/rg-http/tests/`，当前发现 16 个 Rust 集成测试文件 | `find crates/rg-http/tests` |
| 脚本目录 | `scripts/`，包含前后端契约检查、冒烟、全量回归脚本 | `find scripts` |
| 部署目录 | `deploy/`，包含 Docker Compose、Prometheus、Grafana、Alertmanager 配置 | `find deploy` |

### 工作区状态

当前 `git status --short`：

```text
 M .workbuddy/memory/2026-07-04.md
 M ironforge-docs/README.md
?? ironforge-docs/project-architecture-analysis-plan-2026-07.md
```

说明：

- `.workbuddy/memory/2026-07-04.md` 是本轮开始前已存在的未提交变更，本轮不纳入架构分析事实源。
- `ironforge-docs/README.md` 和 `ironforge-docs/project-architecture-analysis-plan-2026-07.md` 是上一轮新增分析步骤时产生的文档变更。
- 本轮新增当前记录文件后，后续基线应同时记录该文件为分析过程产物。

### Workspace 包和二进制

`cargo metadata --format-version 1 --no-deps` 显示当前 workspace 包：

| 包 | 版本 | Manifest | 二进制 |
|----|------|----------|--------|
| `rg-cli` | `0.1.0` | `crates/rg-cli/Cargo.toml` | `ironforge` |
| `rg-ci` | `0.1.0` | `crates/rg-ci/Cargo.toml` | - |
| `rg-core` | `0.1.0` | `crates/rg-core/Cargo.toml` | - |
| `rg-db` | `0.1.0` | `crates/rg-db/Cargo.toml` | - |
| `rg-git` | `0.1.0` | `crates/rg-git/Cargo.toml` | - |
| `rg-http` | `0.1.0` | `crates/rg-http/Cargo.toml` | - |
| `rg-ssh` | `0.1.0` | `crates/rg-ssh/Cargo.toml` | - |
| `rg-runner` | `0.1.0` | `crates/rg-runner/Cargo.toml` | `ironforge-runner` |
| `rg-mcp` | `0.1.0` | `crates/rg-mcp/Cargo.toml` | `ironforge-mcp` |

初步事实：

- 当前后端不是单 crate 项目，而是由 9 个 workspace crate 组成。
- 对外可运行二进制至少有 3 个：主服务 `ironforge`、Runner Agent `ironforge-runner`、MCP Server `ironforge-mcp`。
- 下一轮需要从 `crates/rg-cli/src/main.rs`、`crates/rg-runner/src/main.rs`、`crates/rg-mcp/src/main.rs` 验证各二进制的真实启动路径。

### 代码范围清单

| 范围 | 路径 | 第 0 轮定位 |
|------|------|-------------|
| Rust workspace 根 | `Cargo.toml` | 后端包、共享依赖、release profile 的事实源 |
| 主 CLI/服务入口 | `crates/rg-cli/` | 主二进制 `ironforge`，下一轮重点读取 |
| 核心业务层 | `crates/rg-core/` | 业务 service、认证、仓库、Issue、PR、包注册表等领域逻辑，后续逐模块核验 |
| 数据库层 | `crates/rg-db/` | SeaORM entities、ops、migrations，数据库事实源 |
| Git 协议层 | `crates/rg-git/` | pkt-line、sideband、Protocol V1/V2、Git CLI gateway，Git 链路事实源 |
| SSH 服务端 | `crates/rg-ssh/` | russh 入口和 SSH Git 命令处理，协议入口事实源 |
| HTTP 服务端 | `crates/rg-http/` | Axum 路由、REST API、Git HTTP、WebSocket、OCI、OpenAPI，外部 HTTP 面事实源 |
| CI 引擎 | `crates/rg-ci/` | pipeline 解析、Runner 执行、Gitea Actions 兼容层，CI 事实源 |
| Runner Agent | `crates/rg-runner/` | 独立 Runner 二进制，外部执行节点事实源 |
| MCP Server | `crates/rg-mcp/` | AI Agent 集成入口，tools/resources 事实源 |
| 前端应用 | `web/` | SvelteKit routes、components、stores、API client，前端结构事实源 |
| 部署运维 | `deploy/`、`Dockerfile`、`ironforge.example.toml` | 部署拓扑和运维配置事实源 |
| 自动化脚本 | `scripts/` | 契约检查、前后端冒烟、全量回归事实源 |
| 集成测试 | `crates/rg-http/tests/` | HTTP/API 行为回归事实源 |

### 文档可信度分级

| 文档 | 本轮定位 | 使用方式 |
|------|----------|----------|
| `Cargo.toml` | 一级事实源 | workspace 包、共享依赖、release profile 以此为准 |
| `web/package.json` | 一级事实源 | 前端框架版本、脚本和 npm 依赖以此为准 |
| `crates/**/Cargo.toml` | 一级事实源 | crate 依赖和二进制 target 后续以各 manifest 为准 |
| `crates/**/src/**` | 一级事实源 | 架构结论最终必须回指源码 |
| `web/src/**` | 一级事实源 | 前端路由、组件、状态、API client 最终必须回指源码 |
| `crates/rg-db/src/migrations/**` | 一级事实源 | 数据库表结构和迁移链路以代码为准 |
| `crates/rg-http/tests/**`、`scripts/**` | 一级验证源 | 用于确认功能行为和契约覆盖，不替代源码事实 |
| `AGENTS.md` | 二级上下文源 | AI 协作规范、踩坑记录和历史实现现状有参考价值，涉及版本/进度必须代码核验 |
| `AGENT.md` | 二级入口源 | 快速理解项目结构和阅读顺序，不能作为最终事实唯一来源 |
| `README.md` | 二级使用说明源 | 命令、API 示例、功能列表可参考，涉及当前实现状态需源码核验 |
| `ironforge-docs/README.md` | 二级索引源 | 分析报告目录和历史状态入口，状态数字需重新核验 |
| `ARCHITECTURE.md` | 三级历史设计源 | 更接近早期目标架构/设计说明，不能直接作为当前架构结论 |
| `docs/*.md`、`ironforge-docs/archive/*.md` | 三级背景源 | 可用于理解历史决策和问题背景，不直接继承结论 |

### 第 0 轮发现的版本和口径差异

| 主题 | 当前代码事实 | 历史/入口文档口径 | 处理方式 |
|------|--------------|-------------------|----------|
| `gix` 版本 | `Cargo.toml` 中 workspace 依赖为 `0.84` | `ARCHITECTURE.md`、`README.md` 部分段落仍提 `0.83` 或 `0.83+` | 最终文档以 `Cargo.toml` 为准，历史文档仅作背景 |
| gix 迁移比例 | 第 0 轮尚未核验源码调用点 | 文档中存在 `~70%`、`~85%` 等不同描述 | 第 5 轮通过 `GitCommandGateway` 和 `Command::new("git")` 搜索后再定稿 |
| Phase 状态 | 第 0 轮尚未按模块核验 | 多份文档写 Phase 1-21 完成、后续增强完成 | 不直接继承 Phase 作为架构事实，后续按模块源码确认 |
| 前端覆盖 | 第 0 轮仅确认 `web/` 存在 SvelteKit 项目 | 文档写 Web UI 已覆盖大量功能 | 第 6 轮用 routes 与 API client 做映射矩阵 |

### 架构解读

第 0 轮只确认分析基线，不形成最终架构判断。当前可以确定：

- 本次架构重盘应以 `main@9088e2a` 的代码为基线，但需要标注工作区存在文档类未提交变更。
- 后端分析必须覆盖 9 个 Rust crate，不能只围绕 `rg-http` 或 `rg-core`。
- 前端分析必须单独处理 `web/`，并最终和后端 API、stores、routes 做映射。
- 旧架构文档中的技术选型和 Phase 叙述有历史价值，但已经出现版本/进度口径差异，后续必须以源码和 manifest 复核。

### 待核验项

| 问题 | 原因 | 下一步 |
|------|------|--------|
| 主服务 `ironforge` 的完整启动链路是什么？ | 第 0 轮只确认二进制存在，未读 `main.rs` | 第 1 轮读取 `crates/rg-cli/src/main.rs` |
| HTTP/SSH/CI/MCP 是否在同一进程内运行？ | 需要从入口和 spawn 逻辑确认 | 第 1 轮读取 `rg-cli`、`rg-http`、`rg-ssh`、`rg-runner`、`rg-mcp` |
| `AppState` 当前包含哪些共享依赖？ | 这是 HTTP 层真实边界的核心 | 第 1 轮读取 `crates/rg-http/src/lib.rs` |
| 当前实际路由和 OpenAPI 覆盖如何？ | README 的 API 示例可能不完整 | 第 4 轮从 router 和 `openapi.rs` 核验 |
| 前端实际页面覆盖哪些后端能力？ | 文档写“已完成”不等于 UI 已覆盖 | 第 6 轮建立页面/API 映射 |
| 测试覆盖是否能代表功能完整性？ | 当前只列出测试文件，未读测试内容 | 第 9 轮整理测试覆盖矩阵 |

### 可进入最终文档的内容

可直接进入最终文档的基线段落：

> 本次架构重盘基于 `main@9088e2a`（2026-07-04 02:05:50 +0800）展开。IronForge 当前是一个 Rust workspace + SvelteKit 前端项目：后端 workspace 包含 9 个 crate，对外二进制至少包括 `ironforge`、`ironforge-runner`、`ironforge-mcp`；前端位于 `web/`，由 SvelteKit/Vite 构建。最终架构结论以源码、manifest、数据库迁移和测试脚本为事实源，历史 Phase 文档仅作为背景材料。

---

## 第 1 轮：整体系统分层和运行入口

### 读取文件和命令

已读取：

- `crates/rg-cli/src/main.rs`
- `crates/rg-http/src/lib.rs`
- `crates/rg-ssh/src/lib.rs`
- `crates/rg-runner/src/main.rs`
- `crates/rg-mcp/src/main.rs`
- `crates/rg-mcp/src/lib.rs`
- `crates/rg-http/src/api/mod.rs`
- `crates/rg-core/src/ci/mod.rs`
- `crates/rg-cli/Cargo.toml`
- `crates/rg-http/Cargo.toml`
- `crates/rg-ssh/Cargo.toml`
- `crates/rg-runner/Cargo.toml`
- `crates/rg-mcp/Cargo.toml`

已执行命令：

```bash
sed -n '1,260p' crates/rg-cli/src/main.rs
sed -n '260,620p' crates/rg-cli/src/main.rs
sed -n '620,1040p' crates/rg-cli/src/main.rs
sed -n '1040,1355p' crates/rg-cli/src/main.rs
sed -n '1,260p' crates/rg-http/src/lib.rs
sed -n '260,620p' crates/rg-http/src/lib.rs
sed -n '620,1040p' crates/rg-http/src/lib.rs
sed -n '1100,1245p' crates/rg-http/src/lib.rs
sed -n '2220,2285p' crates/rg-http/src/lib.rs
sed -n '1,260p' crates/rg-ssh/src/lib.rs
sed -n '260,620p' crates/rg-ssh/src/lib.rs
sed -n '1,620p' crates/rg-runner/src/main.rs
sed -n '1,320p' crates/rg-mcp/src/main.rs
rg 'tokio::spawn|rg_http::run|start_ssh_server|run_migrations|connect\(|CiEngine|HttpServerConfig|SshServerConfig' -n crates/rg-cli/src/main.rs crates/rg-http/src/lib.rs crates/rg-ssh/src/lib.rs
```

### 代码事实

| 主题 | 事实 | 证据 |
|------|------|------|
| 主二进制 | `rg-cli` 声明 `[[bin]] name = "ironforge"`，入口为 `crates/rg-cli/src/main.rs` | `crates/rg-cli/Cargo.toml` |
| Runner 二进制 | `rg-runner` 声明 `[[bin]] name = "ironforge-runner"`，入口为 `crates/rg-runner/src/main.rs` | `crates/rg-runner/Cargo.toml` |
| MCP 二进制 | `rg-mcp` 声明 `[[bin]] name = "ironforge-mcp"`，入口为 `crates/rg-mcp/src/main.rs` | `crates/rg-mcp/Cargo.toml` |
| 主服务命令 | `ironforge serve` 由 `Commands::Serve` 进入 `run_serve(...)`，职责是初始化并运行 HTTP + SSH | `crates/rg-cli/src/main.rs` |
| 数据库启动 | `run_serve` 使用 `rg_db::connect_with_timeouts(...)` 连接数据库，然后执行 `rg_db::run_migrations(&db)` | `crates/rg-cli/src/main.rs` |
| Git CLI gateway | `run_serve` 在数据库初始化前调用 `rg_git::cli_gateway::init_global_gateway(...)`，以配置 Git 命令超时 | `crates/rg-cli/src/main.rs` |
| HTTP 启动 | `run_serve` 构造 `rg_http::HttpServerConfig`，通过 `tokio::spawn` 调用 `rg_http::run(http_config)` | `crates/rg-cli/src/main.rs` |
| SSH 启动 | `run_serve` 构造 `rg_ssh::SshServerConfig`，通过 `tokio::spawn` 调用 `rg_ssh::start_ssh_server(ssh_config)` | `crates/rg-cli/src/main.rs` |
| 主任务等待 | `run_serve` 只 `await` HTTP task；SSH task 变量为 `_ssh_handle`，SSH 错误只记录日志，不影响 HTTP | `crates/rg-cli/src/main.rs` |
| HTTP state | `AppState` 包含 repo root、DB、JWT secret、Docker/external runner 开关、rate limiter、notification hub、SMTP、OCI storage、CI log queue、external URL、job timeout、CI engine trait object | `crates/rg-http/src/lib.rs` |
| HTTP 后台任务 | `rg_http::run` 创建 rate limiter cleanup、notification hub、Prometheus registry、OCI storage、CI log write queue，并 spawn runner watchdog | `crates/rg-http/src/lib.rs` |
| HTTP 监听 | 无 TLS 时使用 `tokio::net::TcpListener` + `axum::serve`；有 TLS 时使用 `axum_server::bind_rustls` | `crates/rg-http/src/lib.rs` |
| HTTP router | 生产 router 包含 `/git`、标准根级 Git HTTP 路由、`/api/v1`、`/v2` OCI、`/health`、`/metrics`、`/api-docs`，并以 `web/build` 作为 SPA fallback | `crates/rg-http/src/lib.rs` |
| HTTP 中间件 | 生产 router 包含 metrics、security headers、request id、trace、CORS、ConnectInfo、rate limit、maintenance middleware | `crates/rg-http/src/lib.rs` |
| 健康检查 | `/health` 检查 DB、repo filesystem、metrics registry、Git gateway、SMTP，并返回 `phase: 22` | `crates/rg-http/src/lib.rs` |
| SSH 认证 | SSH 支持 public key 和 password；有 DB 时查 `ssh_keys` 或 `users`，无 DB 时保留开放兼容路径 | `crates/rg-ssh/src/lib.rs` |
| SSH host key | SSH 启动前调用 `ensure_host_key`，host key 不存在时生成 ed25519 key | `crates/rg-ssh/src/lib.rs` |
| SSH Git 分发 | SSH `exec_request` 只接受 `git-upload-pack` 和 `git-receive-pack`，解析 repo path 后按 Protocol V2 或 V1 分发到 `rg-git` | `crates/rg-ssh/src/lib.rs` |
| Runner Agent | 独立 `ironforge-runner` 支持 `register` 和 `run`；通过 HTTP 调用 `/api/v1/runners/...` 注册、心跳、轮询、上传日志、完成 job | `crates/rg-runner/src/main.rs` |
| 主 CLI Runner | `ironforge runner` 也内置一套 runner polling/execution loop，用 reqwest 调同一组 runner API | `crates/rg-cli/src/main.rs` |
| MCP Server | `ironforge-mcp` 默认 stdio JSON-RPC，`--sse` 返回非零错误；通过 `IRONFORGE_URL` 和 `IRONFORGE_PAT` 调 IronForge REST API | `crates/rg-mcp/src/main.rs`、`crates/rg-mcp/src/lib.rs` |
| CI 解耦 | `rg-http` 不直接依赖 `rg-ci`，而通过 `rg_core::ci::CiTrigger` trait；`rg-cli` 注入 `Arc::new(rg_ci::CiEngine)` | `crates/rg-core/src/ci/mod.rs`、`crates/rg-cli/src/main.rs` |

### 入口和进程模型

当前对外运行入口可以分为三类：

| 入口 | 进程 | 主要职责 | 依赖方向 |
|------|------|----------|----------|
| `ironforge serve` | 主服务进程 | 启动 HTTP + SSH，连接 DB，执行迁移，初始化 Git gateway、日志、CI engine、HTTP state | `rg-cli -> rg-http / rg-ssh / rg-db / rg-git / rg-ci` |
| `ironforge migrate` / `rebuild-fts` / `create-repo` / `import` / `index-repo` / `package` | 主 CLI 的一次性任务进程 | 数据库迁移、FTS 重建、裸仓库创建、导入、代码索引、包操作 | `rg-cli -> rg-db / rg-core / rg-git / gix / reqwest` |
| `ironforge runner` | 主 CLI 的 Runner 模式 | 注册或使用现有 runner 凭据，轮询 CI job 并执行 | `rg-cli -> IronForge HTTP API` |
| `ironforge-runner run/register` | 独立 Runner Agent 进程 | Runner 注册、心跳、轮询、执行本地 shell 或 Docker job、回传日志与状态 | `rg-runner -> IronForge HTTP API` |
| `ironforge-mcp` | MCP 子进程 | 面向 AI Agent 暴露 MCP tools/resources，通过 REST API 读取 IronForge 数据 | `rg-mcp -> IronForge HTTP API` |

主服务 `ironforge serve` 的启动链路：

```text
Cli::parse
  -> Commands::Serve
    -> run_serve
      -> load optional TOML config
      -> resolve JWT/config/logging/timeouts
      -> init tracing
      -> create repo_root directory
      -> init rg_git::cli_gateway
      -> rg_db::connect_with_timeouts
      -> rg_db::run_migrations
      -> build HttpServerConfig
      -> build SshServerConfig
      -> spawn rg_http::run(...)
      -> spawn rg_ssh::start_ssh_server(...)
      -> await HTTP task
```

主服务内的长期任务：

| 任务 | 创建位置 | 生命周期 |
|------|----------|----------|
| HTTP server | `run_serve` spawn `rg_http::run` | 主任务，被 `run_serve` await |
| SSH server | `run_serve` spawn `rg_ssh::start_ssh_server` | 独立任务，错误仅日志记录，HTTP 不受影响 |
| Rate limiter cleanup | `rg_http::run` | HTTP state 初始化时启动 |
| CI log write queue | `rg_http::run` 构造 `AppState` 时启动 | HTTP state 持有队列句柄 |
| Runner watchdog | `rg_http::run` spawn `run_runner_watchdog` | 每 60 秒检查 stuck jobs 和 offline runners |
| SSH Git session task | `rg-ssh` `exec_request` 中 spawn | 每个 SSH Git 会话一个异步任务 |

### 顶层系统分层草图

```text
Clients
  ├─ Browser / REST clients
  ├─ git CLI over HTTP(S)
  ├─ git CLI over SSH
  ├─ Runner Agent
  └─ AI Agent via MCP

Process Entrypoints
  ├─ ironforge serve
  │   ├─ rg-http::run    -> /api/v1, /git, root Git HTTP, /v2 OCI, /ws, /health, /metrics, static web/build
  │   └─ rg-ssh::start_ssh_server -> SSH git-upload-pack / git-receive-pack
  ├─ ironforge-runner run/register -> runner REST API
  └─ ironforge-mcp -> stdio JSON-RPC -> IronForge REST API

Core Libraries
  ├─ rg-core: business services and cross-cutting domain logic
  ├─ rg-db: SeaORM entities, ops, migrations
  ├─ rg-git: Git protocol, pkt-line, sideband, gix/Git CLI gateway
  └─ rg-ci: CI engine implementation injected behind rg_core::ci::CiTrigger

Runtime Resources
  ├─ repo_root bare repositories
  ├─ database
  ├─ web/build static assets
  ├─ Git CLI gateway
  ├─ optional Docker
  ├─ optional SMTP
  ├─ optional TLS cert/key
  └─ optional OCI storage under repo_root/oci
```

### 运行时依赖表

| 依赖 | 使用位置 | 是否必需 | 说明 |
|------|----------|----------|------|
| Repo root | `run_serve`、`rg-http`、`rg-ssh` | 必需 | 存储裸仓库；启动时创建并做可写校验 |
| Database | `rg-db`、`rg-http`、`rg-ssh`、CLI 子命令 | 必需 | 主服务启动时连接并自动迁移 |
| JWT secret | `run_serve`、`rg-http` | 必需 | 来自 `IRONFORGE_JWT_SECRET`、CLI 或 config；默认缺失则拒绝启动 |
| Git CLI gateway | `rg-git`、`rg-http` health | 基本必需 | 启动时初始化，`/health` 将其作为关键检查 |
| SSH host key | `rg-ssh` | SSH 必需 | 未配置时默认 `~/.ssh/id_ed25519`；文件不存在会自动生成 |
| TLS cert/key | `rg-http` | 可选 | 同时提供 cert/key 才启用 HTTPS |
| SMTP | `rg-http`、`rg-core::email` | 可选 | host/user/pass/from 齐全时启用，`/health` 会做 TCP 连通性检查 |
| Docker | `rg-ci`、runner execution | 可选 | `docker_enabled` 控制内置 CI；独立 runner 遇到 job image 时尝试 Docker，失败回退本地执行 |
| Static frontend | `web/build` | 可选但影响 Web UI | HTTP fallback 使用 `ServeDir::new("web/build")`；缺少 `index.html` 时 SPA fallback 返回 404 |
| OCI storage | `rg-http::oci` | 可选配置 | 未设置时默认 `{repo_root}/oci` |

### 第 1 轮发现的口径和风险点

| 主题 | 发现 | 后续处理 |
|------|------|----------|
| Config 优先级实现 | 注释写“CLI args > config”，但 `repo_root`、`http_addr`、`ssh_addr`、`db_url` 在 `run_serve` 中直接使用 clap 默认后的 CLI 值，未回退到 config 中的对应字段 | 第 7 或第 9 轮分析配置时复核 `ironforge.example.toml` 和实际行为，必要时进入 followups |
| `external_url` CLI | `ConfigFile` 支持 top-level 和 `[server] external_url`，但 `Serve` 命令没有 `--external-url` 参数；代码注释仍写 CLI takes precedence | 后续配置章节标注真实入口，避免文档写不存在的 CLI 参数 |
| Runner 双入口 | `ironforge runner` 和 `ironforge-runner` 都实现了 runner loop，但代码不完全共用 | 第 8 轮分析 CI/Runner 时判断是否属于重复实现技术债 |
| HTTP Git 路由双入口 | HTTP 同时提供 `/git/{owner}/{repo}/...` 和根级 `/{owner}/{repo}/...` Git Smart HTTP 路由 | 第 4/5 轮分别核验文档应如何描述兼容路由 |
| MCP SSE | `rg-mcp` 仅支持 stdio；`--sse` 分支明确返回非零错误 | 最终文档应写“stdio 可用，SSE 未实现” |
| Health phase | `/health` 返回 `phase: 22`，历史入口文档多写 Phase 21 或更早 | 后续最终架构状态以健康检查和源码为准，历史 Phase 仅背景 |

### 架构解读

第 1 轮可以形成以下初步判断：

- IronForge 的主服务进程是“HTTP 主任务 + SSH 附属任务”的组合。HTTP task 是 `serve` 生命周期的主等待对象；SSH task 与 HTTP 生命周期解耦，SSH 启动或运行失败不会直接终止 HTTP。
- HTTP 层是绝大多数平台能力的聚合入口：REST API、Git HTTP、OCI registry、OpenAPI、Prometheus metrics、SvelteKit 静态资源、runner watchdog 和 CI log queue 都在 `rg-http` 初始化链路附近汇合。
- SSH 层只处理 Git over SSH，不承载普通业务 API；认证通过 DB，命令分发到 `rg-git` 的 upload-pack、receive-pack 或 Protocol V2 handler。
- `rg-cli` 是组合层：它依赖 `rg-http`、`rg-ssh`、`rg-db`、`rg-git`、`rg-ci` 并完成运行时装配；`rg-http` 通过 `rg_core::ci::CiTrigger` trait 间接调用 CI engine，避免直接依赖 `rg-ci`。
- `ironforge-runner` 与 `ironforge-mcp` 是独立进程，它们都通过 HTTP/API 连接主服务，而不是直接共享主服务内存状态。

### 待核验项

| 问题 | 原因 | 下一步 |
|------|------|--------|
| `rg-http` 的全部 API 分组和模块职责是什么？ | 第 1 轮只建立入口图，没有逐个 handler 分析 | 第 4 轮读取 `crates/rg-http/src/api/*.rs` 和 OpenAPI |
| `AppState` 字段分别由哪些 handler 使用？ | 这决定 HTTP 层真实边界和模块耦合 | 第 2/4 轮结合 `rg` 引用分析 |
| Git HTTP 和 SSH 是否完全复用 `rg-git` 协议实现？ | 第 1 轮已看到 SSH 复用，HTTP 细节未完全分析 | 第 5 轮深入 Git/SSH/HTTP 协议链路 |
| config 文件字段和 CLI 参数是否一致？ | 第 1 轮发现若干字段未实际回退到 config | 第 7/9 轮分析配置与部署时核验 |
| Runner 双入口是否行为等价？ | 主 CLI runner 和独立 runner 有重复逻辑 | 第 8 轮分析 CI/Runner 后判断是否需要 followup |

### 可进入最终文档的内容

可直接进入最终文档的入口段落：

> IronForge 当前至少包含三个对外二进制：`ironforge`、`ironforge-runner` 和 `ironforge-mcp`。主服务由 `ironforge serve` 启动，负责装配数据库、迁移、Git CLI gateway、HTTP server、SSH server 和 CI engine；HTTP server 是主生命周期任务，SSH server 作为独立 task 启动并与 HTTP 生命周期解耦。`ironforge-runner` 是独立 CI Runner Agent，通过 `/api/v1/runners/...` 与主服务通信；`ironforge-mcp` 是 stdio MCP server，通过 `IRONFORGE_URL` 和 `IRONFORGE_PAT` 调用 IronForge REST API。

可直接进入最终文档的 HTTP 入口段落：

> HTTP 层由 `rg-http` 提供，`AppState` 聚合 repo root、数据库连接、JWT secret、runner/CI 开关、限流器、通知 hub、SMTP、OCI storage、CI log queue、external URL、job timeout 和注入的 CI engine。生产 router 暴露 `/api/v1` REST API、`/git` Git Smart HTTP、根级 Git Smart HTTP 兼容路由、`/v2` OCI registry、`/health`、`/metrics`、受保护的 `/api-docs`，并通过 `web/build` 提供 SvelteKit SPA 静态资源。

---

## 第 2 轮：后端 crate 职责和依赖关系

### 读取文件和命令

已读取或采集：

- `crates/*/Cargo.toml`
- `crates/*/src/lib.rs`
- `crates/rg-core/src/*/mod.rs`
- `crates/rg-http/src/api/mod.rs`
- `crates/rg-db/src/entities/mod.rs`
- `crates/rg-db/src/ops/mod.rs`
- `crates/rg-db/src/migrations/mod.rs`
- `crates/rg-core/src/audit/mod.rs`
- `crates/rg-core/src/audit/audit.rs`
- `crates/rg-core/src/audit/service.rs`
- `crates/rg-core/src/audit/archiver.rs`

已执行命令：

```bash
cargo metadata --format-version 1 --no-deps
rg '^pub mod|^mod |^pub use|^pub struct|^pub trait|^pub enum' crates/*/src/lib.rs crates/*/src/main.rs crates/rg-core/src/*/mod.rs crates/rg-db/src/lib.rs crates/rg-http/src/api/mod.rs crates/rg-mcp/src/lib.rs -n
find crates/rg-core/src -maxdepth 2 -type f | sort
find crates/rg-db/src -maxdepth 2 -type f | sort
rg 'rg_http|rg_ssh|rg_ci' crates/rg-core/src crates/rg-db/src crates/rg-git/src crates/rg-ci/src crates/rg-ssh/src -n
rg 'Command::new|tokio::process::Command|std::process::Command|global_gateway|GitCommandGateway' crates/rg-core/src crates/rg-git/src crates/rg-ci/src crates/rg-http/src crates/rg-ssh/src -n
cargo check -p rg-core
```

说明：`cargo check -p rg-core` 等待超过两分钟后没有新输出，已中断；本轮不把它作为验证通过依据。

### Crate 依赖矩阵

`cargo metadata --format-version 1 --no-deps` 中的本地 crate 依赖关系：

| Crate | 本地依赖 | 对外 target |
|-------|----------|-------------|
| `rg-cli` | `rg-ci`、`rg-core`、`rg-db`、`rg-git`、`rg-http`、`rg-ssh` | `ironforge` |
| `rg-ci` | `rg-core`、`rg-db` | library |
| `rg-core` | `rg-db`、`rg-git` | library |
| `rg-db` | 无本地 crate 依赖 | library |
| `rg-git` | 无本地 crate 依赖 | library |
| `rg-http` | `rg-core`、`rg-db`、`rg-git` | library + integration test targets |
| `rg-ssh` | `rg-core`、`rg-db`、`rg-git` | library |
| `rg-runner` | 无本地 crate 依赖 | `ironforge-runner` |
| `rg-mcp` | 无本地 crate 依赖 | `ironforge-mcp` |

依赖方向草图：

```text
rg-cli
  ├─ rg-http ──┬─ rg-core ──┬─ rg-db
  │            │            └─ rg-git
  │            ├─ rg-db
  │            └─ rg-git
  ├─ rg-ssh ───┬─ rg-core
  │            ├─ rg-db
  │            └─ rg-git
  ├─ rg-ci ────┬─ rg-core
  │            └─ rg-db
  ├─ rg-core
  ├─ rg-db
  └─ rg-git

rg-runner -> IronForge HTTP API
rg-mcp    -> IronForge HTTP API
```

### Crate 职责说明

| Crate | 当前职责 | 不应承担的职责 |
|-------|----------|----------------|
| `rg-cli` | 进程组合层和 CLI 任务入口：解析命令、加载配置、初始化日志/DB/迁移/Git gateway、装配 HTTP/SSH/CI、执行一次性管理任务 | 不应沉淀长期业务规则；复杂业务应下沉到 `rg-core` 或专门 crate |
| `rg-http` | Axum HTTP 面：REST handlers、Git Smart HTTP、OCI registry、OpenAPI、WebSocket、metrics、安全/限流/维护模式、静态前端服务 | 不应直接持有 CI engine 具体实现；当前已通过 `CiTrigger` trait 解耦 |
| `rg-ssh` | SSH 协议入口：russh server、SSH public key/password auth、Git exec command 解析、转发到 `rg-git` | 不应承载普通 REST/API 业务；不应直接实现 Git pack 协议细节 |
| `rg-core` | 业务领域层：Identity、仓库协作、Issue/PR/Wiki/Review、CI 抽象、Package、Import/Mirror、Audit、Email、Search、Platform | 不应依赖 HTTP framework、Axum handler、CLI 参数解析或进程启动逻辑 |
| `rg-db` | 数据库层：SeaORM entities、ops、migrations、连接池和 SQLite PRAGMA 设置、FTS 重建 | 不应调用 `rg-core` 或协议层；不应包含业务流程编排 |
| `rg-git` | Git 协议和 Git 操作基础设施：pkt-line、sideband、upload-pack、receive-pack、Protocol V2、GitCommandGateway | 不应依赖 HTTP/SSH；传输层只应把流交给它 |
| `rg-ci` | CI engine 实现：CI 配置解析、Gitea Actions 转换、pipeline/job 创建、内置 runner 执行 | 不应被 `rg-http` 直接依赖；当前由 `rg-cli` 注入到 `rg-http` |
| `rg-runner` | 独立外部 Runner Agent：注册、心跳、轮询 job、本地/Docker 执行、上传日志和状态 | 不应直接读 DB 或本地业务 crate；当前通过 REST API 解耦 |
| `rg-mcp` | MCP stdio server：暴露 tools/resources，使用 `IRONFORGE_URL`/`IRONFORGE_PAT` 调主服务 REST API | 不应绕过主服务直接读 DB 或仓库文件；当前通过 HTTP API 解耦 |

### `rg-core` 模块分组

`rg-core/src/lib.rs` 已显式按四组组织模块：

| 组 | 模块 | 说明 |
|----|------|------|
| Identity & Auth | `auth`、`org`、`user` | 用户、组织、认证、SSO、MFA、Token、SSH key 等 |
| Collaboration | `board`、`branch_protection`、`collaborator`、`issue`、`label`、`notification`、`pull_request`、`repo`、`review`、`time_tracking`、`webhook`、`wiki` | 仓库协作和项目管理主域 |
| Delivery & CI | `ci`、`import`、`mirror`、`package_registry`、`release` | 交付、迁移、镜像、包注册表和 Release |
| Infrastructure | `audit`、`email`、`lfs`、`platform`、`search`、`error` | 横切基础能力 |

模块规模观察：

- `rg-core/src` 当前约 82 个 Rust 文件。
- `package_registry` 最大，约 19 个 Rust 文件，包含多协议 adapter 与 OCI 子模块。
- 大多数业务域使用 `mod.rs + service.rs` 形态；`org`、`notification`、`email` 是单文件服务形态。
- `ci` 模块在 `rg-core` 中只放 `CiTrigger` 抽象、参数和 log write queue，不放完整 engine；完整 engine 在 `rg-ci`。

### `rg-db` 结构

`rg-db` 当前约 145 个 Rust 文件，分为三组：

| 目录 | 职责 | 说明 |
|------|------|------|
| `entities/` | SeaORM 实体 | `entities/mod.rs` 统一 re-export，覆盖 user/repository/issue/pull_request/package/runner/oci/audit/wiki_revision 等 |
| `ops/` | 数据访问操作 | 每个业务域一组 ops，例如 `repo_ops`、`issue_ops`、`package_ops`、`pipeline_ops`、`runner_ops` |
| `migrations/` | 迁移链路 | `Migrator::migrations()` 显式列出所有迁移，最新包含 `m20260629_000001_rename_import_task_plural` |

本轮只确认结构，实体和迁移的一致性留到第 3 轮处理。

### `rg-http` 结构

`rg-http/src/api/mod.rs` 当前导出 32 个 API 模块：

```text
admin, ai, archive, artifacts, audit, auth, boards, branch_protection,
ci, collaborators, imports, issues, labels, lfs, mfa, mirrors,
notifications, orgs, packages, pulls, releases, repo_content, repos,
reviews, runners, search, sso, time_tracking, users, webhooks,
webhooks_external, wiki
```

`rg-http` 顶层模块除 `api` 外，还包括：

```text
error, git_v2, instance, metrics, middleware, oci, openapi,
pagination, rate_limit, security, ws
```

初步边界判断：

- `api/*` 是 REST resource handler 层。
- `git_v2.rs` 和 `lib.rs` 中的 Git HTTP handlers 属于 Git Smart HTTP 入口。
- `oci.rs` 是 OCI Distribution v2 入口，挂载在 `/v2`。
- `middleware/security/rate_limit/metrics/ws/openapi/pagination` 是 HTTP 横切能力。

### 边界检查结果

| 检查项 | 结果 | 处理 |
|--------|------|------|
| `rg-core` 是否依赖 `rg-http` / `rg-ssh` / `rg-ci` | 未发现 active 引用 | 分层方向基本成立 |
| `rg-db` 是否依赖业务/协议 crate | 本地依赖为空 | 数据库层保持底层位置 |
| `rg-http` 是否直接依赖 `rg-ci` | 本地依赖中没有 `rg-ci`，通过 `rg_core::ci::CiTrigger` 间接触发 | 属于良好解耦点 |
| `rg-runner` 是否直接依赖业务 crate | 本地依赖为空 | Runner 通过 HTTP API 解耦 |
| `rg-mcp` 是否直接依赖业务 crate | 本地依赖为空 | MCP 通过 HTTP API 解耦 |
| `rg-core/src/audit/service.rs` | 文件中引用 `axum::http::HeaderMap` 和 `crate::entities::audit_log`，但未被 `audit/mod.rs` 导出；当前 active 模块是 `audit/audit.rs` | 记录为疑似陈旧/孤立文件，后续进入 cleanup/followup 核验 |
| raw `Command::new("git")` | `rg-core/src/repo/service.rs` 仍存在多处直接 `Command::new("git")`，同时也有 `GitCommandGateway` 调用 | 与历史“全部 raw git 调用已统一”的口径冲突；第 5 轮深入核验 |
| `rg-core` 中网络 client | `import/github_client.rs`、`import/gitlab_client.rs`、`webhook/service.rs` 使用 `reqwest` | 属于业务层外部集成，不是 HTTP handler 泄漏，但最终文档需说明 core 包含外部 API client |

### 架构解读

第 2 轮可以形成以下判断：

- 当前 Rust 后端分层大体清晰：`rg-cli` 是 composition root，`rg-http`/`rg-ssh` 是传输入口，`rg-core` 是业务域，`rg-db` 是持久化，`rg-git` 是 Git 协议基础设施，`rg-ci` 是 CI engine 实现，`rg-runner`/`rg-mcp` 是外部进程客户端。
- `rg-core` 不是纯领域模型层，它同时包含业务 service、外部平台 client、包协议 adapter、邮件发送、搜索索引、平台进程工具等。最终架构文档应称它为“核心业务与平台服务层”，而不是狭义 DDD domain。
- `rg-http` handlers 有时直接调用 `rg_db::ops`，并不总是严格经 `rg-core` service。这是当前代码事实，后续 API 分析应按 handler 逐项区分“经 core service”与“直接 DB ops”。
- `rg-ci` 与 `rg-http` 的解耦方式较明确：CI 抽象放在 `rg-core::ci`，实现放在 `rg-ci`，由 `rg-cli` 在启动时注入。
- `rg-runner` 和 `rg-mcp` 不共享主服务内部 crate，当前更像“外部客户端进程”，这有利于部署边界说明。

### 待核验项

| 问题 | 原因 | 下一步 |
|------|------|--------|
| `rg-http` 哪些 handler 直接调用 DB ops，哪些走 core service？ | 第 2 轮只看了依赖和模块，未做 handler 映射 | 第 4 轮 API 映射时统计 |
| `rg-db` entities、ops、migrations 是否一一对应？ | 当前只确认结构和导出 | 第 3 轮数据库模型分析 |
| `rg-core/src/audit/service.rs` 是否确认为死代码？ | 文件未导出但存在不一致引用 | 后续可用 `cargo check` 或专门 dead-code 搜索确认，进入 followups |
| `rg-core/src/repo/service.rs` raw git 调用是否仍有效路径？ | 与历史文档口径冲突 | 第 5 轮 Git/gix 迁移分析 |
| `rg-core` 是否应继续聚合 package/import/webhook/email 等外部集成？ | 当前 core 职责偏宽 | 最终 followups 可提出未来拆分建议，但不影响当前文档事实 |

### 可进入最终文档的内容

可直接进入最终文档的后端分层段落：

> IronForge 后端由 9 个 Rust workspace crate 组成。`rg-cli` 是 composition root 和主 CLI；`rg-http` 提供 Axum HTTP 面；`rg-ssh` 提供 Git over SSH 入口；`rg-core` 承载核心业务与平台服务；`rg-db` 承载 SeaORM 实体、ops 和迁移；`rg-git` 承载 Git Smart Protocol、pkt-line、sideband 和 Git CLI gateway；`rg-ci` 是 CI engine 实现，并通过 `rg_core::ci::CiTrigger` 注入 HTTP 层；`rg-runner` 与 `rg-mcp` 是独立客户端进程，分别通过 REST API 与主服务通信。

可直接进入最终文档的依赖边界段落：

> 当前依赖方向整体从入口层流向业务/基础设施层：`rg-cli -> rg-http/rg-ssh/rg-ci/rg-core/rg-db/rg-git`，`rg-http -> rg-core/rg-db/rg-git`，`rg-ssh -> rg-core/rg-db/rg-git`，`rg-core -> rg-db/rg-git`，`rg-ci -> rg-core/rg-db`。`rg-db` 与 `rg-git` 不依赖上层 crate，`rg-runner` 和 `rg-mcp` 不依赖本地业务 crate，而是通过 HTTP API 连接主服务。

---

## 第 3 轮：领域模型、数据库和迁移链路

### 读取文件和命令

已读取或采集：

- `crates/rg-db/src/entities/*.rs`
- `crates/rg-db/src/entities/mod.rs`
- `crates/rg-db/src/ops/*.rs`
- `crates/rg-db/src/ops/mod.rs`
- `crates/rg-db/src/migrations/*.rs`
- `crates/rg-db/src/migrations/mod.rs`
- 重点实体：`user`、`repository`、`issue`、`pull_request`、`pipeline*`、`package*`、`organization*`、`team*`、`board*`、`time_entry`
- 重点迁移：FTS5、代码搜索、组织/团队表名修正、看板/工时表名修正、导入任务表名修正

已执行命令：

```bash
find crates/rg-db/src/entities -maxdepth 1 -type f -name '*.rs'
find crates/rg-db/src/ops -maxdepth 1 -type f -name '*.rs'
find crates/rg-db/src/migrations -maxdepth 1 -type f -name 'm*.rs'
rg '#\[sea_orm\(table_name = "|create_table\(|CREATE VIRTUAL TABLE|rename_table|alter_table|has_table|has_column' crates/rg-db/src/entities crates/rg-db/src/migrations -n
rg 'belongs_to =|has_many|Relation|DeriveRelation|Related<|_id:' crates/rg-db/src/entities -n
rg '^pub async fn|^pub fn|^async fn' crates/rg-db/src/ops/*.rs -n
```

尝试执行 fresh DB 迁移验证：

```bash
./target/release/ironforge migrate --db-url 'sqlite:///tmp/ironforge_arch_round3.db?mode=rwc'
```

该命令超过约 90 秒未返回且无输出，已中断；未生成可用临时 DB 文件。本轮不把 fresh DB 迁移作为验证通过依据，结论以源码、实体和迁移文件为准。

### 实体总览

当前 `rg-db/src/entities` 中除 `mod.rs` 外共有 52 个实体文件。实体到表名映射如下：

| 领域 | 实体 | 表名 |
|------|------|------|
| Identity/Auth | `user` | `users` |
| Identity/Auth | `ssh_key` | `ssh_keys` |
| Identity/Auth | `access_token` | `access_tokens` |
| Identity/Auth | `password_reset_token` | `password_reset_tokens` |
| Identity/Auth | `oauth_account` | `oauth_accounts` |
| Identity/Auth | `mfa_backup_code` | `mfa_backup_codes` |
| Identity/Auth | `login_log` | `login_logs` |
| Identity/Auth | `sso_provider` | `sso_providers` |
| Repository | `repository` | `repositories` |
| Repository | `repo_collaborator` | `repo_collaborators` |
| Repository | `repo_star` | `repo_stars` |
| Repository | `repo_watch` | `repo_watches` |
| Repository | `protected_branch` | `protected_branches` |
| Repository | `commit_status` | `commit_statuses` |
| Issue/PR/Review | `issue` | `issues` |
| Issue/PR/Review | `issue_comment` | `issue_comments` |
| Issue/PR/Review | `label` | `labels` |
| Issue/PR/Review | `issue_label` | `issue_labels` |
| Issue/PR/Review | `milestone` | `milestones` |
| Issue/PR/Review | `pull_request` | `pull_requests` |
| Issue/PR/Review | `pr_review` | `pr_reviews` |
| Issue/PR/Review | `review_comment` | `review_comments` |
| Wiki/LFS/Webhook | `wiki_page` | `wiki_pages` |
| Wiki/LFS/Webhook | `wiki_revision` | `wiki_revisions` |
| Wiki/LFS/Webhook | `lfs_object` | `lfs_objects` |
| Wiki/LFS/Webhook | `webhook` | `webhooks` |
| Wiki/LFS/Webhook | `webhook_delivery` | `webhook_deliveries` |
| CI/Runner | `pipeline` | `pipelines` |
| CI/Runner | `pipeline_stage` | `pipeline_stages` |
| CI/Runner | `pipeline_job` | `pipeline_jobs` |
| CI/Runner | `runner` | `runners` |
| CI/Runner | `artifact` | `artifacts` |
| Organization | `organization` | `organizations` |
| Organization | `organization_member` | `organization_members` |
| Organization | `team` | `teams` |
| Organization | `team_member` | `team_members` |
| Package Registry | `package_registry` | `package_registry` |
| Package Registry | `package` | `packages` |
| Package Registry | `package_version` | `package_versions` |
| Package Registry | `package_file` | `package_files` |
| OCI Registry | `oci_repository` | `oci_repository` |
| OCI Registry | `oci_blob` | `oci_blob` |
| OCI Registry | `oci_manifest` | `oci_manifest` |
| OCI Registry | `oci_upload` | `oci_upload` |
| Project Management | `board` | `boards` |
| Project Management | `board_column` | `board_columns` |
| Project Management | `board_card` | `board_cards` |
| Project Management | `time_entry` | `time_entries` |
| Platform/Extension | `notification` | `notifications` |
| Platform/Extension | `release` | `releases` |
| Platform/Extension | `release_asset` | `release_assets` |
| Platform/Extension | `mirror` | `mirrors` |
| Platform/Extension | `import_task` | `import_tasks` |
| Platform/Extension | `audit_log` | `audit_log` |

### 核心关系模型

主要 ER 关系可概括为：

| 领域 | 关系 |
|------|------|
| 用户与仓库 | `users` has many `repositories`；`repositories.owner_id -> users.id`，`repositories.org_id -> organizations.id` 可为空 |
| 组织与团队 | `organizations.owner_id -> users.id`；`organization_members.org_id/user_id`；`teams.org_id -> organizations.id`；`team_members.team_id/user_id` |
| 仓库协作 | `repo_collaborators.repo_id/user_id`；`repo_stars.repo_id/user_id`；`repo_watches.repo_id/user_id` |
| Issue | `issues.repo_id -> repositories.id`，`issues.author_id -> users.id`；`issue_comments.issue_id/author_id`；`issue_labels.issue_id/label_id` |
| PR/Review | `pull_requests.repo_id -> repositories.id`，`pull_requests.author_id -> users.id`；`pr_reviews.pr_id/repo_id/reviewer_id`；`review_comments.review_id/pr_id/author_id` |
| Wiki/LFS/Webhook | `wiki_pages.repo_id`，`wiki_revisions.wiki_page_id`；`lfs_objects.repo_id`；`webhooks.repo_id`，`webhook_deliveries.webhook_id` |
| CI/Runner | `pipelines.repo_id`；`pipeline_stages.pipeline_id`；`pipeline_jobs.stage_id`，可关联 `runner_id`；`artifacts.job_id` |
| Package Registry | `package_registry.repo_id`；`packages.package_registry_id/owner_id`；`package_versions.package_id/author_id`；`package_files.version_id` |
| OCI Registry | `oci_repository.repo_id/owner_id`；`oci_blob`、`oci_manifest`、`oci_upload` 通过 `oci_repository_id` 挂接 |
| Board/Time | `boards.repo_id` 或 `boards.org_id`；`board_columns.board_id`；`board_cards.column_id` 和可选 `issue_id`；`time_entries.issue_id/user_id` |
| Audit/Notification | `audit_log.user_id` 可选；`notifications.user_id` 且可选 `repo_id` |

### Ops 覆盖

`rg-db/src/ops` 当前除 `mod.rs` 外共有 39 个 ops 文件。覆盖情况：

| 领域 | Ops |
|------|-----|
| 用户/认证 | `user_ops`、`ssh_key_ops`、`token_ops`、`password_reset_token_ops`、`oauth_account_ops`、`mfa_backup_code_ops`、`login_log_ops`、`sso_provider_ops` |
| 仓库 | `repo_ops`、`repo_collaborator_ops`、`repo_star_ops`、`repo_watch_ops`、`protected_branch_ops`、`commit_status_ops` |
| Issue/PR/Review | `issue_ops`、`issue_comment_ops`、`issue_label_ops`、`label_ops`、`milestone_ops`、`pull_request_ops`、`pr_review_ops`、`review_comment_ops` |
| Wiki/LFS/Webhook | `wiki_page_ops`、`wiki_revision_ops`、`lfs_object_ops`、`webhook_ops` |
| CI/Runner | `pipeline_ops`、`runner_ops`、`artifact_ops` |
| Org/Project | `org_ops`、`board_ops`、`time_entry_ops` |
| Package/OCI | `package_registry_ops`、`package_ops`、`package_version_ops`、`package_file_ops`、`oci_ops` |
| Platform/Extension | `notification_ops`、`release_ops`、`mirror_ops`、`import_task_ops`、`audit_log_ops` |

观察：

- 大多数实体都有对应 ops，少数关联表被合并到聚合 ops 中，例如 `organization`、`organization_member`、`team`、`team_member` 统一由 `org_ops` 管理。
- OCI 四张表统一由 `oci_ops` 管理。
- `access_token` 的 ops 文件名是 `token_ops`，不是 `access_token_ops`。

### 迁移时间线

`Migrator::migrations()` 当前显式列出 46 个迁移：

| 时间段 | 迁移 | 主要内容 |
|--------|------|----------|
| 2026-04-24 | `m20260424_000001` 到 `000009` | 用户、仓库、SSH keys/PAT、Issue、PR、Wiki/LFS/Webhook、Pipeline、Phase6/Phase8 扩展 |
| 2026-04-27 | `m20260427_000001` | LFS 压缩字段 |
| 2026-05-08 | `m20260508_000001` 到 `000006` | Star/Watch、Release、Label、Commit Status、FTS5、Repo soft delete |
| 2026-05-10 | `m20260510_000001` 到 `000004` | Runner、pipeline job runner 字段、updated_at、Artifacts |
| 2026-05-11 | `m20260511_000001` 到 `000003` | PR head repo、缺失索引、FTS5 trigger 修正 |
| 2026-05-12 | `m20260512_000001` | code_fts 代码搜索索引 |
| 2026-06-07 | `m20260607_000001` 到 `000011` | Mirror、Board、Time、Import、Package Registry、LDAP/SSO/MFA、Audit Log |
| 2026-06-08 | `m20260608_000001` 到 `000003` | OCI 表、OAuth 唯一约束、Job tags |
| 2026-06-16 | `m20260616_000001`、`0000015`、`000002` | Password reset、org/team/notification 表名修正、soft delete columns |
| 2026-06-17 | `m20260617_000001`、`000002` | Wiki revisions、board/time/wiki_revision 表名修正 |
| 2026-06-21 | `m20260621_000001` | PR labels/milestone |
| 2026-06-29 | `m20260629_000001` | import_task -> import_tasks 表名修正 |

注意：文件名排序和 `Migrator::migrations()` 实际顺序不完全相同。例如 `m20260508_000006_add_repo_soft_delete` 在 migrator 中排在 release/labels/status/FTS 之前。最终文档应以 `Migrator::migrations()` 顺序为准。

### FTS 和索引模型

当前 FTS 模型包括：

| FTS 表 | 来源 | 同步方式 |
|--------|------|----------|
| `repos_fts` | `repositories.name/description` | SQLite triggers |
| `issues_fts` | `issues.title/body` | SQLite triggers |
| `wiki_pages_fts` | `wiki_pages.title/content` | SQLite triggers |
| `code_fts` | Git repository files | `CodeIndexer` 服务扫描 Git 对象写入，无 DB trigger |

迁移风险点：

- `m20260508_000005_create_fts5_indexes` 最初使用 FTS5 `'delete'` 特殊命令并传入内容列。
- `m20260511_000003_fix_fts5_triggers` 改成 `DELETE FROM ... WHERE rowid = old.id`，避免 FTS5 delete 命令语义问题。
- `code_fts` 没有触发器，因为 Git blob 内容不在普通 DB 表中。

### 表名纠偏迁移

当前迁移链中有多处纠偏迁移，说明历史上曾出现 `#[derive(Iden)]` 生成单数表名而实体声明复数表名的问题：

| 迁移 | 修正 |
|------|------|
| `m20260616_0000015_rename_org_team_plural` | `organization -> organizations`、`organization_member -> organization_members`、`team -> teams`、`team_member -> team_members`，并在 plural `notifications` 存在时删除死的 singular `notification` |
| `m20260617_000002_rename_board_time_tables_plural` | `board_card -> board_cards`、`board_column -> board_columns`、`board -> boards`、`time_entry -> time_entries`、`wiki_revision -> wiki_revisions` |
| `m20260629_000001_rename_import_task_plural` | `import_task -> import_tasks` |

这些纠偏迁移是最终架构文档和 followups 中必须保留的数据库维护背景：新增迁移时不能依赖默认 `Iden` 单数表名，必须与实体 `table_name` 完全一致。

### 领域解读

第 3 轮可以形成以下判断：

- IronForge 的数据库模型已经覆盖从 Git 托管核心到平台扩展的大部分领域：用户/认证、仓库、协作、Issue/PR/Review、Wiki/LFS/Webhook、CI/Runner、Package/OCI、组织团队、看板工时、审计、通知、导入和镜像。
- 仓库是大多数业务域的聚合根之一：Issue、PR、Wiki、LFS、Webhook、CI、Package Registry、Mirror、Board、Release、Commit Status 都直接或间接关联 `repositories`。
- 用户是身份和行为归属的根：仓库 owner、Issue/PR author、reviewer、runner registration 之外的 audit/notification/auth records 都通过 user 关联。
- Package Registry 与 OCI 是两套并行模型：通用 package registry 使用 `package_registry/packages/package_versions/package_files`；OCI Distribution v2 使用 `oci_repository/oci_blob/oci_manifest/oci_upload`。
- 部分模型保留了兼容/冗余字段，例如 `issues.labels` 和 `pull_requests.labels` 存 JSON label names，同时也存在 `labels`/`issue_labels` 规范表。最终文档应说明“真实模型中既有规范关联表，也有历史/展示用冗余字段”。

### 待核验项

| 问题 | 原因 | 下一步 |
|------|------|--------|
| fresh DB 迁移为何长时间无输出 | 使用既有 release binary 迁移到 `/tmp` 超时未返回且被中断 | 第 9 轮测试/部署时重新用当前构建产物验证 |
| 实体表名与实际 fresh DB 表是否完全一致 | 本轮基于源码和纠偏迁移推断，未完成运行态验证 | 第 3 轮后续或第 9 轮用可控超时命令复测 |
| `issues.labels` / `pull_requests.labels` 与 `issue_labels` 的真实使用边界 | 存在冗余字段与规范关联表 | 第 4 轮 API 和第 6 轮前端映射时核验 |
| OCI 实体关系未完整声明 `Related` | `oci_blob`、`oci_manifest`、`oci_upload` 有 `oci_repository_id` 但 Relation enum 为空 | 第 8 轮 Package/OCI 分析时判断是否影响使用 |
| `artifact.job_id` Relation enum 为空 | 实体有 `job_id`，但未声明 SeaORM relation | 第 8/9 轮 CI artifacts 分析时核验是否为技术债 |

### 可进入最终文档的内容

可直接进入最终文档的数据库总览段落：

> IronForge 的数据库层由 `rg-db` 提供，包含 SeaORM entities、ops 和 migrations。当前实体模型覆盖 52 张业务表，并通过 46 个显式排序的迁移构建。核心聚合根是 `users`、`repositories`、`organizations`、`issues`、`pull_requests`、`pipelines` 和 package/OCI registry 表。大多数领域拥有独立 ops 文件，组织/团队和 OCI 等聚合通过单个 ops 模块统一管理。

可直接进入最终文档的迁移注意事项：

> 迁移链中包含多处表名单复数纠偏迁移，历史原因是 `#[derive(Iden)] enum X { Table }` 默认生成单数表名，而实体使用复数 `#[sea_orm(table_name = "...")]`。新增迁移时必须显式保证迁移表名与实体 `table_name` 一致，并优先使用 `has_table` / `has_column` 做幂等保护。

---

## 第 4 轮：HTTP API、Git HTTP 与实时通道

### 分析范围

本轮对应分析计划中的第 4 步，重点确认 `rg-http` 的外部入口、REST API 模块分布、Git Smart HTTP 路由、OCI Distribution v2 路由、WebSocket 通道、跨域/限流/错误处理/OpenAPI 等横切结构。

分析对象：

- `crates/rg-http/src/lib.rs`
- `crates/rg-http/src/api/`
- `crates/rg-http/src/ws.rs`
- `crates/rg-http/src/oci.rs`
- `crates/rg-http/src/openapi.rs`
- `crates/rg-http/src/error.rs`
- `crates/rg-http/src/pagination.rs`
- `crates/rg-http/src/rate_limit.rs`
- `crates/rg-http/tests/`

### HTTP 服务总入口

`rg-http` 的主路由由 `build_production_router()` 和 `build_routes()` 组合。生产路由的大体结构如下：

```text
/
├── /git/*                         # Git Smart HTTP，带 /git 前缀
├── /{owner}/{repo}/info/refs      # Git Smart HTTP，根路径兼容入口
├── /{owner}/{repo}/git-upload-pack
├── /{owner}/{repo}/git-receive-pack
├── /api/v1/*                      # REST API + WebSocket
├── /v2/*                          # OCI Distribution v2 API
├── /health
├── /metrics
├── /api-docs/*                    # OpenAPI JSON + Swagger UI
└── SPA fallback                   # web/build 静态文件 + index.html fallback
```

测试路由 `build_test_router()` 与生产路由接近，但没有 `/v2`、`/metrics`、静态文件服务和 rate limiter。测试路由仍保留 `/git`、根路径 Git HTTP、`/api/v1`、`/health` 和 OpenAPI docs，适合做 API/鉴权集成测试。

### 中间件与横切能力

生产路由当前叠加的主要中间件：

| 能力 | 实现位置 | 说明 |
|------|----------|------|
| HTTP metrics | `middleware::http_metrics_middleware` | 记录请求指标，配合 `/metrics` 暴露 |
| 安全响应头 | `security::security_headers_middleware` | CSP、安全 header 等 |
| Request ID | `middleware::request_id_middleware` | 注入/传播请求 ID |
| tracing | `TraceLayer` | 创建 `http_request` span |
| CORS | `build_cors_layer()` | 允许方法/头固定；origin 由 `IRONFORGE_CORS_ORIGINS` 或请求 origin 决定 |
| ConnectInfo | `into_make_service_with_connect_info::<SocketAddr>()` | 给限流/IP 识别使用 |
| Rate limit | `rate_limit::rate_limit_middleware` | per-IP token bucket，返回 429 |
| Maintenance mode | `middleware::maintenance_middleware` | 维护模式拦截 |
| PAT 兼容 | `pat_auth_middleware` | 在 `/api/v1` 层把 Personal Access Token 转换成 Bearer JWT 语义 |

`rate_limit.rs` 使用 `X-Forwarded-For`、`X-Real-IP` 或 socket addr 识别客户端 IP，并以 token bucket 方式限流。被限流时返回结构化 `AppError::rate_limited()`。

`error.rs` 提供统一 `AppError`，主要错误码包括：

- `NOT_FOUND`
- `BAD_REQUEST`
- `UNAUTHORIZED`
- `FORBIDDEN`
- `CONFLICT`
- `INTERNAL_ERROR`
- `RATE_LIMITED`

当前 `IntoResponse` 会对内部错误做脱敏，响应体中也预留了 `request_id` 字段；但从代码看该字段在错误响应中默认仍是 `None`，最终文档可描述为“错误模型预留 request_id，实际注入链路需后续核验”。

### REST API 模块分布

`crates/rg-http/src/api/mod.rs` 当前导出 32 个 API 模块：

```text
admin, ai, archive, artifacts, audit, auth, boards, branch_protection,
ci, collaborators, imports, issues, labels, lfs, mfa, mirrors,
notifications, orgs, packages, pulls, releases, repo_content, repos,
reviews, runners, search, sso, time_tracking, users, webhooks,
webhooks_external, wiki
```

这些模块可以按领域归类为：

| API 领域 | 模块 |
|----------|------|
| 身份/用户 | `users`, `auth`, `sso`, `mfa` |
| 仓库核心 | `repos`, `repo_content`, `archive`, `branch_protection`, `collaborators` |
| 协作 | `issues`, `labels`, `pulls`, `reviews`, `wiki`, `releases` |
| Git 扩展对象 | `lfs`, `webhooks`, `webhooks_external`, `mirrors` |
| CI/CD | `ci`, `runners`, `artifacts` |
| 组织/通知/搜索 | `orgs`, `notifications`, `search` |
| 管理与审计 | `admin`, `audit` |
| 项目管理 | `boards`, `time_tracking` |
| 包与导入 | `packages`, `imports` |
| AI Agent | `ai` |

### `/api/v1` 路由分组

`/api/v1` 是 REST API 与 WebSocket 的主要前缀。按业务流可整理为以下入口：

| 分组 | 代表路由 |
|------|----------|
| Runner agent | `/runners/register`、`/runners/{id}/heartbeat`、`/runners/{id}/jobs/poll`、`/runners/{id}/jobs/{job_id}/start|log|finish|artifacts` |
| 用户与认证 | `/users/register`、`/users/login`、`/users/logout`、`/users/me`、`/users/forgot-password`、`/users/reset-password`、`/users/tokens` |
| MFA/SSO | `/users/mfa/*`、`/auth/sso/providers`、`/auth/sso/{provider}/login`、`/auth/sso/{provider}/callback` |
| 仓库 | `/repos`、`/repos/explore`、`/repos/{owner}`、`/repos/{owner}/{name}` |
| 仓库模板 | `/repos/templates/gitignores`、`/repos/templates/licenses`、`/repos/templates/readmes`、`/repos/templates/labels` |
| Issue/Label/Milestone | `/repos/{owner}/{name}/issues/*`、`/repos/{owner}/{name}/labels/*`、`/repos/{owner}/{name}/milestones/*` |
| Pull Request/Review | `/repos/{owner}/{name}/pulls/*`、`/repos/{owner}/{name}/pulls/{number}/reviews/*` |
| Wiki | `/repos/{owner}/{name}/wiki/*`、`/repos/{owner}/{name}/wiki/{slug}/history`、`/revisions/*` |
| Repository content | `/tree/*`、`/blob/*`、`/log/*`、`/branches`、`/tags`、`/contents/*`、`/commits/{sha}/signature` |
| Git data extension | `/lfs/*`、`/webhooks/*`、`/archive/*`、`/mirror/*` |
| CI/CD | `/pipelines/*`、`/jobs/*`、`/artifacts/*` |
| Release/Asset | `/releases/*`、`/releases/{id}/assets/*` |
| Social/metadata | `/star`、`/watch`、`/stargazers`、`/forks`、`/transfer`、`/statuses/*` |
| Org/notification | `/orgs/*`、`/notifications/*` |
| Project management | `/boards/*`、`/time/*` |
| Admin | `/admin/users/*`、`/admin/orgs/*`、`/admin/sso/providers/*`、`/admin/audit/logs/*`、`/admin/settings` |
| Search/AI | `/search`、`/ai/repos/{owner}/{name}/summary|issues|prs|tree|search/code` |
| WebSocket | `/ws/notifications`、`/ws/job/{job_id}` |

注意：`/ai/repos/{owner}/{name}/index` 在源码中被注释，注释说明是 Axum Handler trait 问题，改用 CLI 命令。这意味着代码索引触发入口与 AI 查询入口并不完全对称。

### Git Smart HTTP 路由

Git HTTP 协议有两套入口：

```text
/git/{owner}/{repo}/info/refs
/git/{owner}/{repo}/git-upload-pack
/git/{owner}/{repo}/git-receive-pack

/{owner}/{repo}/info/refs
/{owner}/{repo}/git-upload-pack
/{owner}/{repo}/git-receive-pack
```

从历史文档看，推荐路由前缀是 `/git/`；但当前生产和测试路由都保留了根路径兼容入口。最终架构文档应明确：

- `/git/<owner>/<repo>` 是项目约定的 Git HTTP clone/push 前缀。
- 根路径 `/<owner>/<repo>` 的 Git HTTP 入口目前存在，可能用于兼容 Git 托管平台常见 URL。
- 由于前端 SPA fallback 也位于根路径，根级 Git HTTP 路由必须保持在 fallback 之前注册。

Git HTTP handler 与 `rg-git`、`rg-core` 仓库权限逻辑相关，Git Protocol V2 的详细行为放到第 5 轮继续分析。

### OCI Distribution v2 路由

OCI Registry 不在 `/api/v1` 下，而是独立挂载到 `/v2`，符合 Docker/OCI 客户端约定。当前入口包括：

```text
/v2/
/v2/auth/token
/v2/{owner}/{repo}/tags/list
/v2/{owner}/{repo}/manifests/{reference}
/v2/{owner}/{repo}/blobs/{digest}
/v2/{owner}/{repo}/blobs/uploads/
/v2/{owner}/{repo}/blobs/uploads/{uuid}
```

`oci.rs` 实现 Docker Distribution 风格的鉴权挑战和响应头。`/v2/` 在未认证时返回 `WWW-Authenticate`，用于触发 Docker 客户端获取 token。鉴权支持：

- 常规 JWT；
- OCI bearer token；
- pull action 的匿名访问逻辑。

Package Registry 的 REST/协议接口还包含在 `/api/v1/repos/{owner}/{name}/packages/*` 下；OCI 是另一个协议面。最终前后端结构文档应把 package REST 管理面与 OCI 协议面分开描述。

### WebSocket 通道

当前 WebSocket 有两个入口：

```text
/api/v1/ws/notifications
/api/v1/ws/job/{job_id}
```

通知 WebSocket 的鉴权优先级：

1. HttpOnly cookie：`ironforge_token`
2. `Sec-WebSocket-Protocol`：`bearer.<jwt>`
3. query 参数：`?token=...`

这个设计用于兼容浏览器 WebSocket 无法设置任意 Authorization header 的限制，同时保留 query token fallback。`NotificationHub` 维护 per-user channel 和 global channel，可向指定用户或全局广播通知事件。

Job log WebSocket 使用 `JobLogHub` 广播 `job_log` 事件。当前接口按 `job_id` 订阅过滤消息，但事件源是全局 broadcast channel。第 8 轮分析 CI/Runner 时需要继续核验该通道的权限边界，尤其是 job log 是否需要用户/仓库权限校验。

### OpenAPI 覆盖情况

`openapi.rs` 使用 `#[derive(OpenApi)]` 维护 REST API 文档，包含大量 `paths(...)` 和 `tags(...)`。当前 tag 覆盖：

```text
Users, Repositories, Issues, Labels, Pull Requests, Reviews, Wiki,
LFS, Webhooks, CI/CD, Releases, Organizations, Notifications, Search,
Branch Protection, Collaborators, Repository Content, Runners, Artifacts,
Admin, AI, Mirrors, Boards, Time Tracking, Imports, SSO, MFA, Audit,
Packages
```

OpenAPI 与实际路由之间存在合理边界：

- Git Smart HTTP 路由不属于 REST OpenAPI。
- OCI `/v2` 路由不在 `/api/v1` 内，不应强行混入普通 REST API 文档。
- WebSocket、`/metrics`、SPA fallback 也不适合作为常规 REST OpenAPI。

需要后续核验的覆盖差异：

- `packages.rs` 中多种协议适配路由是否全部进入 OpenAPI。当前 `openapi.rs` 对 Packages 有 tag，但从代码片段看协议型端点可能不是全量列入。
- `repo_content` 中 `/contents/*` 创建/更新/删除文件的路径是否完整进入 OpenAPI。已确认 tree/blob/log/branches/tags/signature 在 OpenAPI 中出现，但内容写入路径还需要继续对拍。

### API 测试覆盖入口

`crates/rg-http/tests/` 当前有 16 个集成测试文件：

```text
admin_org_tests, admin_settings_tests, admin_sso_audit_tests,
admin_user_tests, api_tests, board_tests, collaborator_tests,
git_auth_tests, issue_tests, notification_tests, openapi_docs_auth_tests,
org_tests, pat_api_tests, release_tests, time_tracking_tests, wiki_tests
```

这些测试覆盖了管理、组织、SSO/审计、基础 API、看板、协作者、Git 鉴权、Issue、通知、OpenAPI docs 鉴权、PAT、Release、工时、Wiki 等关键 REST 层能力。CI/Runner、Package Registry、OCI、WebSocket job log 的测试入口在文件名层面不明显，后续第 8/9 轮需要继续确认覆盖深度。

### 领域解读

第 4 轮可以形成以下判断：

- `rg-http` 是项目最大的组合层：它既承载 REST API，也承载 Git Smart HTTP、OCI Distribution v2、WebSocket、OpenAPI、静态前端托管和可观测入口。
- REST API 的领域分布已经非常宽，`rg-http/src/api/` 更接近“HTTP adapter per domain”，而不是单一业务边界。最终前后端结构文档应以业务域重新归类，而不是简单按文件名罗列。
- Git HTTP 与 OCI 都是“协议面”，不是普通 REST 管理面。它们面向 Git/Docker 客户端，对 content-type、header、认证挑战和路径格式更敏感，应在架构文档中单独成章。
- `/api/v1` 下 WebSocket 使用了专门的浏览器鉴权策略，说明当前安全模型已经考虑 HttpOnly JWT 迁移后的实时通道兼容问题。
- OpenAPI 是 REST API 的主要契约，但不是全部外部接口契约。最终文档需要同时列出 REST OpenAPI、Git Smart HTTP、OCI、WebSocket、metrics/health 这几类入口。

### 待核验项

| 问题 | 原因 | 下一步 |
|------|------|--------|
| Root-level Git HTTP 是否是长期支持入口 | 当前代码保留，但项目约定文档强调 `/git/` 前缀 | 第 5 轮 Git 协议分析时结合 clone/push 测试确认 |
| Job log WebSocket 权限边界 | 当前按 `job_id` 过滤全局广播，未在本轮完整追踪鉴权 | 第 8 轮 CI/Runner 或第 7 轮安全分析继续核验 |
| OpenAPI 是否覆盖所有 REST 写路径 | `openapi.rs` 与路由表可能有局部差异 | 第 9 轮用 OpenAPI 测试和路由对拍补充 |
| Package 协议端点的 OpenAPI 边界 | Packages 有 REST 管理面和多协议面，覆盖策略需区分 | 第 8 轮 Package Registry 详细分析 |
| `AppError.request_id` 是否真实返回 | 错误体字段当前默认 `None` | 第 7 轮横切能力分析中核验 request-id 注入链路 |

### 可进入最终文档的内容

可直接进入最终文档的 HTTP 总览段落：

> `rg-http` 是 IronForge 的 HTTP 组合层，负责 REST API、Git Smart HTTP、OCI Distribution v2、WebSocket、OpenAPI、静态前端托管和健康/指标入口。REST API 统一挂载在 `/api/v1`，Git Smart HTTP 同时提供 `/git/<owner>/<repo>` 和根路径兼容入口，OCI Registry 按 Docker/OCI 约定挂载在 `/v2`，实时通知和 CI job log 通过 `/api/v1/ws/*` 提供。

可直接进入最终文档的接口分类：

```text
REST API      /api/v1/*
Git HTTP      /git/{owner}/{repo}/* and /{owner}/{repo}/*
OCI Registry  /v2/*
WebSocket     /api/v1/ws/notifications, /api/v1/ws/job/{job_id}
Observability /health, /metrics
Docs          /api-docs/*
Frontend      web/build SPA fallback
```

可直接进入前后端结构文档的 API 模块归类：

> 前端应主要通过 `web/src/lib/api/client.ts` 访问 `/api/v1` REST API；Git 客户端直接访问 Git Smart HTTP；Docker/OCI 客户端直接访问 `/v2`；浏览器实时通知通过 `/api/v1/ws/notifications`，CI 日志通过 `/api/v1/ws/job/{job_id}`。这些通道不是同一种 API 契约，文档中需要分别描述认证方式和调用者。

---

## 第 5 轮：Git、SSH 与协议层

### 分析范围

本轮对应分析计划中的第 5 步，重点确认 IronForge 的 Git 协议实现边界、HTTP Git 与 SSH Git 的差异、Protocol V1/V2 能力、pkt-line/sideband 处理、权限校验、post-push hooks，以及 gix 与 git CLI 的职责分布。

分析对象：

- `crates/rg-git/src/lib.rs`
- `crates/rg-git/src/pkt_line.rs`
- `crates/rg-git/src/sideband.rs`
- `crates/rg-git/src/protocol/upload_pack.rs`
- `crates/rg-git/src/protocol/receive_pack.rs`
- `crates/rg-git/src/protocol/v2.rs`
- `crates/rg-git/src/cli_gateway.rs`
- `crates/rg-ssh/src/lib.rs`
- `crates/rg-http/src/lib.rs` 中 Git Smart HTTP handlers
- `crates/rg-http/src/git_v2.rs`
- `crates/rg-http/tests/git_auth_tests.rs`
- `crates/rg-core/src/repo/service.rs` 中仍存在的 raw git CLI 调用

### 协议栈分层

当前 Git/SSH 协议相关代码可以分成 4 层：

| 层级 | 位置 | 职责 |
|------|------|------|
| 传输入口 | `rg-http`, `rg-ssh` | HTTP Smart Git、SSH exec/session、鉴权、路径解析、响应 Content-Type、生命周期 |
| 协议编解码 | `rg-git::pkt_line`, `rg-git::sideband` | pkt-line、flush/delim/response-end、sideband-64k band 1/2/3 |
| Git 服务协议 | `rg-git::protocol::{upload_pack, receive_pack, v2}` | V1 upload-pack/receive-pack、V2 ls-refs/fetch/object-info、pack 读写 |
| Git 对象操作 | `gix` + `GitCommandGateway` | refs/head/object 查询更新用 gix；pack-objects/index-pack/rebase/diff 等能力仍依赖 git CLI |

核心调用路径：

```text
HTTP clone/fetch:
  rg-http handle_info_refs / handle_git_upload_pack
    -> rg_git::protocol::upload_pack::handle_upload_pack_http
    -> git pack-objects via GitCommandGateway

HTTP push:
  rg-http handle_info_refs / handle_git_receive_pack
    -> rg_git::protocol::receive_pack::handle_receive_pack_http
    -> git index-pack --fix-thin via GitCommandGateway
    -> gix update ref
    -> rg-http post_push_hooks

SSH clone/fetch:
  rg-ssh exec_request
    -> parse git-upload-pack
    -> rg_git::protocol::upload_pack::handle_upload_pack_stream

SSH push:
  rg-ssh exec_request
    -> parse git-receive-pack
    -> rg_git::protocol::receive_pack::handle_receive_pack_stream

Protocol V2:
  HTTP: Git-Protocol: version=2 header
    -> rg_git::protocol::v2::handle_v2_http
  SSH: env_request GIT_PROTOCOL=version=2
    -> rg_git::protocol::v2::handle_v2_stream
```

### `rg-git` 模块职责

`rg-git` 当前只有 8 个源码文件，结构较集中：

```text
cli_gateway.rs
lib.rs
pkt_line.rs
sideband.rs
protocol/mod.rs
protocol/upload_pack.rs
protocol/receive_pack.rs
protocol/v2.rs
```

模块职责：

| 模块 | 职责 |
|------|------|
| `pkt_line.rs` | Git pkt-line 编解码，支持 `Data`、`Flush`、`Delim`、`ResponseEnd` |
| `sideband.rs` | sideband-64k 输出，band 1 data、band 2 progress、band 3 error |
| `upload_pack.rs` | V1 clone/fetch：ref advertisement、want/have 简化协商、packfile 输出 |
| `receive_pack.rs` | V1 push：ref advertisement、update command 解析、thin pack index、ref update、report-status 输出 |
| `v2.rs` | Protocol V2：capabilities、`ls-refs`、`fetch`、`object-info` |
| `cli_gateway.rs` | 统一 git CLI 网关，提供版本检查、同步超时、异步 pipe、tracing |

`pkt_line` 和 `sideband` 是协议正确性的基础，历史踩坑中提到的 `read_line`、sideband report-status、flush 顺序都已经在实现注释中固化。

### HTTP Git Smart Protocol

HTTP Git 入口由 `rg-http/src/lib.rs` 中的 handlers 实现。第 4 轮已确认路由同时存在：

```text
/git/{owner}/{repo}/info/refs
/git/{owner}/{repo}/git-upload-pack
/git/{owner}/{repo}/git-receive-pack

/{owner}/{repo}/info/refs
/{owner}/{repo}/git-upload-pack
/{owner}/{repo}/git-receive-pack
```

HTTP handler 的关键职责：

| 阶段 | 行为 |
|------|------|
| repo 解析 | 去掉 `.git` 后缀，校验 owner/repo path，定位 `repo_root/{owner}/{repo}.git` |
| 认证 | `Authorization: Bearer` 支持 JWT 或 PAT；`Authorization: Basic` 支持 `user:token` 或 `token:x-oauth-basic` |
| 授权 | upload-pack 调 `can_read`；receive-pack 调 `can_write` |
| 未认证拒绝 | 匿名访问受限仓库返回 `401` + `WWW-Authenticate: Basic realm="IronForge"` |
| 无权限拒绝 | 已认证但无权限返回 `403` |
| V1 advertisement | `build_info_refs()` 用 gix 枚举 refs/HEAD，并手工构造 Smart HTTP pkt-line |
| V2 detection | `Git-Protocol: version=2` header 触发 V2 capability advertisement / V2 POST handler |
| POST body bridging | 使用 `tokio::io::duplex` 把 HTTP body 转成 AsyncRead/AsyncWrite |
| push 后处理 | `receive-pack` 成功后异步触发 CI、webhook、通知、邮件和分支/tag 事件 |

`git_auth_tests.rs` 当前专门覆盖私有仓库 Git HTTP 认证：

- 匿名访问私有仓库 `info/refs` 返回 401；
- PAT 通过 Basic password 字段可访问；
- PAT 通过 Bearer 可访问；
- 无效 PAT 被拒绝。

这说明 HTTP Git 的 PAT 认证是有回归测试保护的。

### SSH Git Server

`rg-ssh` 使用 `russh` 实现 SSH 服务端，主要流程集中在 `crates/rg-ssh/src/lib.rs`：

| 阶段 | 行为 |
|------|------|
| host key | 启动时若 host key 缺失，生成 ed25519 key 并设为 `0600` |
| publickey auth | DB 模式下按 SSH key fingerprint 查 `ssh_keys` |
| password auth | DB 模式下按 username 查 user，再用 Argon2 校验密码 |
| no DB mode | 兼容早期模式，接受所有 publickey/password |
| env request | 接受 `GIT_PROTOCOL=version=2`，记录协议版本 |
| exec request | 只接受 `git-upload-pack` 和 `git-receive-pack` |
| path safety | 对 repo path 调 `rg_core::platform::validate_repo_path()` |
| repo lookup | 尝试 `repo_root/{path}`，不存在则尝试 `repo_root/{path}.git` |
| session lifecycle | git 处理完成后先发 exit-status，再 shutdown stream |

需要重点记录的差异：SSH 当前只在连接层做身份认证和 repo path 校验，未在 `exec_request` 中看到针对具体仓库的 `can_read` / `can_write` 授权检查。也就是说：

- HTTP Git 会区分 clone/fetch 和 push 权限；
- SSH Git 当前看起来只要 SSH 身份认证通过且路径存在，就会进入 upload-pack / receive-pack。

这应作为第 7 轮安全分析的重点核验项。如果不是已有设计意图，就属于高优先级安全 followup。

### Protocol V1：upload-pack

`upload_pack.rs` 支持 HTTP split reader/writer 和 SSH single stream 两种模式。

当前 V1 upload-pack 行为：

- 用 gix 枚举 refs，并尝试解析 HEAD；
- advertisement capability 包含 `side-band-64k ofs-delta agent=ironforge/0.1`；
- HTTP `build_info_refs()` 中 advertised upload-pack capability 更宽，包含 `multi_ack_detailed no-done side-band-64k thin-pack ofs-delta agent=ironforge/0.1`；
- negotiation 解析 `want` / `have` / `done`，支持第一条 want 里的 NUL capability，也兼容空格分隔 capability；
- 不实现完整多轮 negotiation，基本是收到 wants 后写 `NAK`，然后发 pack；
- `send_packfile()` 当前用 `git pack-objects --all --stdout` 生成 pack。

重要实现边界：

- V1 upload-pack 的 pack 生成没有根据 `wants/haves` 做精确对象裁剪，而是 `--all` 打包所有对象。这实现简单但对大仓库性能和带宽不友好。
- 注释明确 `TODO(gix): Replace with gix pack generation when available`，说明 pack 生成仍被 gix 上游能力阻塞。

### Protocol V1：receive-pack

`receive_pack.rs` 同样支持 HTTP 和 SSH 两种模式。

当前 V1 receive-pack 行为：

- 用 gix 枚举 refs/HEAD 做 advertisement；
- capability 包含 `report-status report-status-v2 side-band-64k agent=ironforge/0.1`；
- 解析 update command：`old_sha new_sha refname[\0capabilities]`；
- 删除 ref 当前被显式拒绝，返回 `deletion not supported`；
- 收到 pack data 后通过 `git index-pack --fix-thin --stdin` 写入 object database；
- 每个 ref update 用 gix `repo.reference(..., PreviousValue::Any, ...)` 更新引用；
- report-status 先在内存里构造内部 pkt-lines，再整体作为 sideband band 1 输出，最后发送 sideband flush。

关键正确性点：

- `--fix-thin` 是必须项，因为 Git push 默认可能发送 thin pack；
- response sideband 编码已经按历史踩坑修正；
- ref update 当前是 `PreviousValue::Any`，没有基于 `old_sha` 做 compare-and-swap，也没有在 receive-pack 层执行分支保护拒绝。

HTTP push 的分支保护当前只在 `post_push_hooks()` 中做审计日志式 warning：

```text
Post-push: push to protected branch detected (should be enforced by pre-receive hook)
```

最终文档应避免表述为“Git push 已强制分支保护”。更准确的说法是：PR merge path 有分支保护检查；Git receive-pack push 后当前只检测并记录 protected branch 命中，尚未在 pre-receive 阶段阻断。

### Protocol V2

`rg-git/src/protocol/v2.rs` 支持：

- capability advertisement；
- `ls-refs`；
- `fetch`；
- `object-info`。

HTTP V2 通过 `Git-Protocol: version=2` header 触发：

- `GET info/refs` 返回 V2 capability advertisement；
- `POST git-upload-pack` 调 `handle_v2_http()` 处理 V2 command。

SSH V2 通过客户端 env request：

```text
GIT_PROTOCOL=version=2
```

触发 `handle_v2_stream()`。

V2 的能力和边界：

| 能力 | 当前实现 |
|------|----------|
| `ls-refs` | 支持 ref-prefix、symrefs、peel、unborn |
| `fetch` | 支持 wants/haves/shallow/deepen/filter 参数解析；packfile 通过 sideband 输出 |
| `object-info` | 内部可处理 `oid` 并返回 size |
| pack 生成 | 用 `git pack-objects --revs --stdout --thin` |
| capability advertisement | 当前写出 `version 2`、`agent`、`ls-refs`、`fetch=shallow`、`object-format=sha1`、`server-option` |

待核验差异：

- `object-info` handler 存在，但 capability advertisement 没有看到 `object-info`。客户端是否会主动使用该命令需要进一步确认。
- `crates/rg-http/src/git_v2.rs` 定义了一套 `handle_info_refs_v2`、`handle_git_upload_pack_v2`、`handle_git_receive_pack_v2`，但代码搜索未发现它们被路由挂载。当前实际 HTTP V2 路径走 `lib.rs` 内的 `git_v2::wants_protocol_v2()` + `rg_git::protocol::v2::handle_v2_http()`。
- `git_v2.rs` 中 receive-pack V2 wrapper 不做 HTTP Git 鉴权和 post-push hooks；虽未挂路由，但如果未来接入，需要补齐与 `lib.rs` 主 handler 的一致性。

### gix 与 git CLI 边界

当前 Git 操作不是纯 gix，也不是散落裸 CLI，而是三种形态并存：

| 形态 | 代表场景 |
|------|----------|
| gix 原生 | refs 枚举、HEAD/object 查询、ref update、PR diff numstat、merge/squash merge |
| `GitCommandGateway` | pack-objects、index-pack、PR unified diff、rebase、archive 等统一入口 |
| raw `Command::new("git")` | `rg-core/src/repo/service.rs` 中 auto-init commit/push/head、contents create/update/delete 的 clone/add/commit/push/rm |

`cli_gateway.rs` 自带防回归测试，目标是禁止除网关外的 raw `Command::new("git")`。但测试当前显式跳过 `repo/service.rs`：

```text
TODO: refactor repo/service.rs to use GitCommandGateway
```

实际搜索仍发现 `rg-core/src/repo/service.rs` 有 13 处 raw `Command::new("git")`。这与历史文档中“全部 raw git 子进程调用统一走 GitCommandGateway”的表述不一致，应在最终 followups 里修正为：

> Git 子进程调用已大部分统一到 `GitCommandGateway`，但 `repo/service.rs` 的 auto-init 和 contents 写入/删除路径仍有 raw `Command::new("git")`，且当前防回归测试显式排除了该文件。

### HTTP Push 后处理

HTTP `handle_git_receive_pack()` 成功后会异步执行 `post_push_hooks()`。该函数当前处理：

- 查询 repo id / owner id；
- protected branch 命中 warning；
- 若新 commit 有 CI 配置，触发 pipeline；
- 通过 WebSocket 推送 CI/push 通知；
- 可选 SMTP 邮件通知；
- 触发 `push` webhook；
- 根据 ref 类型触发 `branch.created`、`branch.deleted`、`tag.created`、`tag.deleted` webhook。

需要注意：

- post-push hooks 只在 HTTP receive-pack handler 中看到；SSH receive-pack 直接调用 `rg-git`，没有接入同一套 post-push hooks。
- 因此 SSH push 可能不会触发 CI/Webhook/通知。第 8 轮 CI/Runner 和第 7 轮安全分析需要继续确认是否另有入口。

### 已覆盖测试

本轮确认到的直接测试入口：

| 测试 | 覆盖 |
|------|------|
| `rg-git` 内部 unit tests | pkt-line、sideband、V2 capability 常量、GitCommandGateway |
| `crates/rg-http/tests/git_auth_tests.rs` | Git HTTP 私有仓库 PAT/JWT 鉴权入口 |
| `cli_gateway.rs::test_no_raw_git_command_in_crates` | raw git CLI 防回归，但排除 `repo/service.rs` |

文件名层面未看到专门的 SSH clone/push 集成测试，也未看到完整 Protocol V2 clone/fetch/push 的端到端测试文件。最终测试/部署分析时需要继续核验实际 CI 是否覆盖这些路径。

### 领域解读

第 5 轮可以形成以下判断：

- IronForge 的 Git 协议层采用“自实现协议框架 + gix 读取/更新 refs + git CLI pack 能力”的混合模式。pkt-line、sideband、V1/V2 命令处理在 Rust 中实现；pack 生成和 thin pack indexing 仍依赖成熟的 git CLI。
- HTTP Git 是当前更完整的协议入口：它包含 repository 权限检查、PAT/JWT 认证、Content-Type 管控、V2 header 检测和 push 后业务事件。
- SSH Git 更像纯 Git transport：完成 SSH 登录、路径解析和 protocol handler 调用，但本轮未看到仓库级 read/write 授权和 post-push business hooks。
- `rg-git` 的 receive-pack 能够写入 objects 和 refs，但没有实现 pre-receive hook 体系；分支保护在 Git push 层目前只是 post-push warning，不是强制拒绝。
- V2 fetch 已有主要骨架，但 capability advertisement 与 handler 能力之间存在 `object-info` 口径差异；HTTP V2 helper 文件也存在未挂载实现，需要最终文档标注真实入口。

### 待核验项

| 问题 | 原因 | 下一步 |
|------|------|--------|
| SSH Git 是否缺少仓库级授权 | `exec_request` 未看到 `can_read/can_write` | 第 7 轮安全分析中按 private repo SSH clone/push 场景确认 |
| SSH push 是否触发 CI/Webhook/通知 | post-push hooks 只在 HTTP receive-pack handler 中看到 | 第 8 轮 CI/Runner 分析确认事件触发入口 |
| Git push 分支保护是否应在 receive-pack 前置拒绝 | 当前只 post-push warning | followups 中列为安全/一致性风险 |
| V1 upload-pack `--all` 是否可接受 | 无视 wants/haves 精确裁剪，可能影响大仓库性能 | 第 9 轮性能/部署风险中评估 |
| V2 `object-info` 是否应 advertise | handler 存在但 capability 未公告 | 第 5 轮后续或测试轮用 Git V2 客户端对拍 |
| `git_v2.rs` 未挂路由代码是否保留 | 存在一套可能过期 handler | 最终架构文档标注真实入口，followups 建议清理或接入 |
| raw git CLI 剩余调用 | `repo/service.rs` 仍有 13 处 raw `Command::new("git")` | 技术债文档修正历史状态，并排期迁移到 gateway |

### 可进入最终文档的内容

可直接进入最终架构文档的 Git 协议段落：

> IronForge 的 Git 协议实现由 `rg-git`、`rg-http` 和 `rg-ssh` 共同承担。`rg-git` 实现 pkt-line、sideband-64k、Protocol V1 upload-pack/receive-pack 和 Protocol V2 ls-refs/fetch/object-info；`rg-http` 提供 Smart HTTP 入口并负责 JWT/PAT 认证、仓库权限和 push 后业务事件；`rg-ssh` 基于 russh 提供 SSH Git exec transport。底层 Git 对象操作采用 gix 与 git CLI 混合模式：refs/object 查询与 ref update 尽量使用 gix，pack-objects、index-pack、rebase、archive 等仍通过统一 GitCommandGateway 或少量 legacy raw CLI 调用完成。

可直接进入最终文档的协议入口表：

| 入口 | 路径/命令 | 调用者 | 权限模型 |
|------|-----------|--------|----------|
| HTTP clone/fetch | `/git/{owner}/{repo}/git-upload-pack` | Git HTTP client | `can_read` |
| HTTP push | `/git/{owner}/{repo}/git-receive-pack` | Git HTTP client | `can_write` + post-push hooks |
| SSH clone/fetch | `git-upload-pack '/owner/repo'` | Git SSH client | SSH auth；仓库级权限需核验 |
| SSH push | `git-receive-pack '/owner/repo'` | Git SSH client | SSH auth；仓库级权限和 hooks 需核验 |
| HTTP V2 | `Git-Protocol: version=2` | Git HTTP client | 复用 HTTP Git auth/access |
| SSH V2 | `GIT_PROTOCOL=version=2` env | Git SSH client | 复用 SSH transport |

可直接进入 followups 的风险表述：

> 当前 HTTP Git 与 SSH Git 的业务完整性不完全一致：HTTP Git 包含仓库级 read/write 授权和 push 后 CI/Webhook/通知触发；SSH Git 本轮未看到同等授权和 post-push hook 接入。若确认无其他路径补齐，应优先补上 SSH Git 的 per-repo 授权与 push 后事件，以避免 private repo 权限和自动化触发在两种协议间表现不一致。

---

## 第 6 轮：前端路由、状态与前后端映射

### 分析范围

本轮对应分析计划中的第 6 步，重点确认 `web/` 前端工程结构、SvelteKit 路由、API client、认证状态、i18n、组件分布、前端页面与后端 `/api/v1` 的映射，以及前端对 Git HTTP、WebSocket、Package Registry 等非普通 REST 通道的使用方式。

分析对象：

- `web/package.json`
- `web/svelte.config.js`
- `web/src/routes/`
- `web/src/lib/api/`
- `web/src/lib/stores/`
- `web/src/lib/components/`
- `web/src/lib/i18n/`
- `web/src/lib/utils/`
- `web/src/lib/app.css`
- `web/src/lib/packageFormats.ts`

### 前端工程定位

`web/` 是 SvelteKit 2 + Svelte 5 前端工程，使用 `@sveltejs/adapter-static` 构建为静态 SPA：

```text
adapter-static
pages/assets: build
fallback: index.html
ssr: false
prerender: false
```

关键结论：

- 前端是浏览器端 SPA，不依赖 SvelteKit SSR 数据加载。
- 后端 `rg-http` 通过 `ServeDir::new("web/build").fallback(index.html)` 托管构建产物。
- 所有业务数据主要在 Svelte 组件生命周期中通过 fetch 调后端 API 获取。
- 页面路由由 SvelteKit 文件系统路由负责，刷新/直达路径由 SPA fallback 兜底。

依赖和脚本：

| 类别 | 内容 |
|------|------|
| 框架 | Svelte 5、SvelteKit 2、Vite 8、TypeScript 6 |
| 适配器 | `@sveltejs/adapter-static` |
| 运行依赖 | `marked`、`highlight.js` |
| 脚本 | `dev`、`build`、`preview`、`check`、`smoke:integration`、`smoke:admin-browser`、`smoke:admin` |

### 目录结构

当前前端源码主要分布：

```text
web/src/
├── routes/                 # SvelteKit 页面路由，当前 53 个 +page.svelte
├── lib/
│   ├── api/                # API client，当前 28 个文件
│   ├── components/         # 复用组件，当前 8 个文件
│   ├── stores/             # Svelte 5 rune 状态
│   ├── i18n/               # 中英文翻译与 t()
│   ├── utils/              # markdown/search/diff 工具
│   ├── app.css             # 全局 token 和基础样式
│   └── packageFormats.ts   # Package Registry format label
├── app.html
└── app.d.ts
```

当前组件文件：

```text
Button.svelte
Dropdown.svelte
FileEditor.svelte
InstanceBanner.svelte
Layout.svelte
Navbar.svelte
PipelineBadge.svelte
RepoHeader.svelte
```

### 路由总览

当前有 53 个 `+page.svelte` 页面。按业务域整理如下：

| 领域 | 页面 |
|------|------|
| 首页/探索/搜索 | `/`、`/explore`、`/search`、`/help` |
| 登录与账号恢复 | `/login`、`/register`、`/forgot-password`、`/reset-password` |
| 用户工作台 | `/dashboard`、`/notifications`、`/settings/security`、`/settings/tokens` |
| 组织 | `/orgs`、`/orgs/[name]` |
| 导入 | `/imports` |
| 仓库 owner 页 | `/[owner]` |
| 仓库首页/代码 | `/[owner]/[repo]`、`/blob/[...path]`、`/new`、`/edit/[...path]` |
| commits | `/commits`、`/commits/[sha]` |
| issues | `/issues`、`/issues/[number]`、`/issues/board` |
| pulls/reviews | `/pulls`、`/pulls/[number]` |
| wiki | `/wiki`、`/wiki/[title]`、`/wiki/[title]/history` |
| CI/CD | `/pipelines` |
| releases | `/releases`、`/releases/new`、`/releases/edit/[id]` |
| packages | `/packages`、`/packages/upload`、`/packages/[format]`、`/packages/[format]/[...name]` |
| project management | `/boards`、`/time_tracking` |
| repo settings | `/settings`、`/settings/labels`、`/settings/branches`、`/settings/mirror`、`/settings/webhooks`、`/settings/collaborators`、`/settings/runners` |
| admin | `/admin`、`/admin/users`、`/admin/orgs`、`/admin/runners`、`/admin/settings`、`/admin/audit` |

只有两个 layout 文件：

```text
web/src/routes/+layout.svelte
web/src/routes/+layout.ts
web/src/routes/[owner]/[repo]/settings/+layout.svelte
```

根 layout 提供全局 Navbar、InstanceBanner、Layout 容器、i18n 初始化、认证初始化、快捷键注册和后端健康检查。仓库 settings layout 提供 RepoHeader、设置侧边栏和设置页 breadcrumb。

### 根布局和启动流程

根布局 `+layout.svelte` 的启动逻辑：

1. 引入全局 CSS；
2. 初始化 locale；
3. 调 `fetchUser()` 获取当前登录用户；
4. 注册全局键盘快捷键；
5. 调 `/health` 检查后端连通性；
6. 在 `isAuthReady()` 之前显示 loading；
7. 渲染 Navbar、InstanceBanner、Layout 和页面 children。

`+layout.ts` 明确：

```ts
export const ssr = false;
export const prerender = false;
```

这意味着前端不会通过 server load 预取数据，也不会生成静态 HTML route 内容。最终文档应把它描述为“由后端托管的 SPA 前端”，不是 SSR 前端。

### API client 结构

`web/src/lib/api/` 当前有 28 个文件：

```text
_base.svelte.ts
_base.ts
admin.ts
auth.ts
boards.ts
branchProtections.ts
client.svelte.ts
collaborators.ts
imports.ts
issues.ts
labels.ts
mfa.ts
milestones.ts
mirrors.ts
notifications.ts
orgs.ts
packages.ts
pipelines.ts
pulls.ts
releases.ts
repos.ts
runners.ts
search.ts
timeTracking.ts
tokens.ts
webhooks.ts
websockets.ts
wiki.ts
```

实际路由和组件基本仍从 `$lib/api/client.svelte` 导入 API；`client.svelte.ts` 当前是 38 行的纯兼容 re-export 聚合入口。领域实现已分布到 `repos.ts`、`auth.ts`、`issues.ts`、`pulls.ts`、`admin.ts`、`packages.ts` 等独立模块，避免主入口继续承载具体业务逻辑。

历史文档中提到 `web/src/lib/api/client.ts`，当前实际文件是：

```text
web/src/lib/api/client.svelte.ts
web/src/lib/api/_base.svelte.ts
```

这是一个需要在最终文档中修正的口径差异。

### API 基础层

`_base.svelte.ts` 是请求基础层：

| 能力 | 实现 |
|------|------|
| API base | `VITE_API_BASE`，默认 `/api/v1` |
| 后端 base | `withBackendBase()` 从 `/api/v1` 去掉后缀，用于 `/git`、`/health` |
| SSH clone URL | `VITE_SSH_HOST`、`VITE_SSH_PORT`，默认 port `2222` |
| token | 只保留内存 token，清理 legacy `localStorage.ironforge_token` |
| 请求认证 | `credentials: 'include'` 自动携带 HttpOnly cookie；若内存 token 存在则加 Bearer |
| 超时 | 普通请求 30 秒；文件下载 5 分钟 |
| 错误处理 | 解析后端 `{ error: { code, message, request_id } }` 结构，抛出 message |
| 下载 | `downloadApiFile()` 解析 `Content-Disposition` 并触发浏览器下载 |

这与后端 H-2/M-3/M-4 安全修复口径一致：浏览器主认证依赖 HttpOnly cookie，JS 不再持久化 JWT。

### 领域 API 映射

`client.svelte.ts` 当前暴露的主要 API 对象：

| 前端对象 | 后端路径 |
|----------|----------|
| `auth` | `/users/*`、`/users/mfa/verify`、`/auth/sso/*` |
| `repos` | `/repos`、`/repos/{owner}/{repo}`、tree/blob/contents/log/branches/tags/star/watch/fork/transfer/statuses |
| `issues` | `/repos/{owner}/{repo}/issues/*` |
| `pulls` / `reviews` | `/repos/{owner}/{repo}/pulls/*`、reviews/comments/diff/merge |
| `pipelines` | `/repos/{owner}/{repo}/pipelines/*` |
| `wiki` | `/repos/{owner}/{repo}/wiki/*`、history/revisions |
| `collaborators` | `/repos/{owner}/{repo}/collaborators/*` |
| `branchProtections` | `/repos/{owner}/{repo}/branches/protection/*` |
| `mirrors` | `/repos/{owner}/{repo}/mirror/*` |
| `webhooks` | `/repos/{owner}/{repo}/hooks/*` |
| `imports` | `/imports/*` |
| `orgs` | `/orgs/*` |
| `notifications` | `/notifications/*` |
| `releases` | `/repos/{owner}/{repo}/releases/*` |
| `labels` / `milestones` | `/repos/{owner}/{repo}/labels/*`、`/milestones/*` |
| `tokens` | `/users/tokens/*` |
| `mfa` | `/users/mfa/*` |
| `admin` | `/admin/users`、`/admin/orgs`、`/admin/audit/logs`、`/admin/settings`、`/admin/sso/providers` |
| `search` | `/search` |
| `packages` | `/repos/{owner}/{repo}/packages/*` |
| `runners` | `/admin/runners/*`、`/runners/register` |
| `timeTracking` | `/repos/{owner}/{repo}/issues/{number}/time/*` |
| `boards` | `/repos/{owner}/{repo}/boards/*` |

前端还通过 `withBackendBase()` 直接构造非 `/api/v1` 地址：

| 用途 | 地址 |
|------|------|
| HTTP clone URL | `/git/{owner}/{repo}` |
| 后端健康检查 | `/health` |

### WebSocket 使用

前端当前只看到通知 WebSocket 的直接使用：

```text
connectNotificationWebSocket()
  -> ws(s)://<host>/<api-base>/ws/notifications
```

调用页面：

```text
web/src/routes/notifications/+page.svelte
```

该函数不拼接 token，也不使用 subprotocol；注释说明依赖 same-origin WebSocket 自动携带 HttpOnly cookie。后端在第 4 轮已确认支持 cookie、subprotocol bearer、query token 三种方式。

待核验差异：

- 后端提供 `/api/v1/ws/job/{job_id}`；
- 前端 pipelines 页面当前通过 `pipelines.job()` REST 拉取 job log，并用 5 秒 interval 刷新 running/pending pipeline；
- 本轮未看到前端接入 job log WebSocket。

### 认证和状态管理

`auth.svelte.ts` 使用 Svelte 5 runes 管理全局认证状态：

```text
currentUser
isLoading
error
authReady
pendingMfaUsername
```

主要流程：

- `login()` 调 `auth.login()`；
- 如果 `mfa_required`，清空 token，记录 `pendingMfaUsername`；
- MFA 验证成功后设置内存 token，再调 `/users/me`；
- `fetchUser()` 总是尝试 `/users/me`，依赖 HttpOnly cookie；
- `logout()` 调后端 `/users/logout` 清 cookie，并清本地内存状态。

`instance.svelte.ts` 管理：

- instance banner；
- 全局快捷键；
- 搜索输入聚焦。

注意：`instance.svelte.ts` 的快捷键注释提到 `g i` / `g p`，但代码实际只实现了 `?` 聚焦搜索；`search/+page.svelte` 另实现了 `Ctrl/Cmd+K` 聚焦搜索。最终文档可只写已实现快捷键，避免按注释扩展。

### i18n

i18n 位于 `web/src/lib/i18n/`：

| 文件 | 说明 |
|------|------|
| `index.ts` | locale store、`t()`、`createT()`、日期格式化 |
| `translations/en.json` | 英文翻译，865 行 |
| `translations/zh-CN.json` | 中文翻译，865 行 |

行为：

- `localStorage.locale` 持久化；
- 浏览器语言 `zh*` 自动选择 `zh-CN`；
- 缺失翻译会 `console.warn` 并返回 fallback 或 key；
- 支持 `{name}` 形式插值；
- `formatDate()` / `formatDateTime()` 根据 locale 使用 `zh-CN` 或 `en-US`。

### UI 与样式体系

全局样式在 `app.css` 中定义，风格接近 GitHub dark：

| 类别 | 变量/能力 |
|------|-----------|
| 颜色 | `--bg-primary`、`--bg-secondary`、`--border`、`--text-*`、`--accent`、`--green/red/yellow/purple/orange` |
| 字体 | system sans + SFMono/Consolas mono |
| 圆角 | `--radius: 6px`、`--radius-lg: 8px` |
| 布局 | `--layout-gutter`、`--layout-max`、`--layout-narrow-max` |
| 通用组件类 | `.btn`、`.btn-primary`、`.btn-outline`、`.btn-danger`、`.error-banner`、`.success-banner`、`.gh-card`、`.gh-list` |
| 代码高亮 | highlight.js class 配色 |

复用组件分工：

| 组件 | 职责 |
|------|------|
| `Navbar` | 全局导航、搜索、语言切换、用户菜单、Admin 入口 |
| `Layout` | 页面容器 |
| `InstanceBanner` | 全局后端状态/维护提示 |
| `RepoHeader` | 仓库页 header、tabs、clone URL、star/watch/fork/archive |
| `FileEditor` | 文件新建/编辑，支持 edit/preview/diff、markdown 预览、highlight.js |
| `PipelineBadge` | CI 状态徽章 |
| `Dropdown` | 通用菜单 |
| `Button` | 通用按钮 |

### Markdown、搜索和 diff 工具

`utils/markdown.ts` 使用 `marked` 渲染 Markdown，并做前端 HTML sanitizer：

- 通过 DOMParser 遍历 HTML，非 allowlist 标签展开或移除；
- 只允许 Markdown 常用标签和少量安全属性；
- 移除 `on*`、`style`、危险 URL 协议等属性；
- 非浏览器 fallback 也按标签/属性/URL allowlist 收紧。

`utils/search.ts` 会先 escape HTML，再用 `<mark class="search-highlight">` 高亮查询词。

`utils/diff.ts` 实现基于 LCS 的行 diff，并设置 `MAX_DETAILED_DIFF_CELLS = 90000`，大文件退化为删除+新增 fallback。

这些工具说明前端在展示 Markdown、搜索摘要和编辑器 diff 时有基本的 XSS/性能保护。后续修复已将 Markdown sanitizer 从正则清洗升级为 DOMParser + 标签/属性/URL allowlist，并补了 `smoke:markdown-sanitizer` 覆盖 server fallback。

### Package Registry 前端口径

`packageFormats.ts` 当前列出 17 种格式：

```text
cargo, npm, maven, pypi, docker, nuget, rubygems, go, helm, composer,
conan, conda, alpine, debian, rpm, swift, generic
```

后端第 21 阶段主要覆盖 Cargo/npm/PyPI/Maven/NuGet/Helm/RubyGems/Docker/Generic/Composer 等 native adapter 或协议端点。当前前端仍保留 17 种 package type 供上传选择；其中 Cargo/npm/Maven/PyPI/Docker/NuGet/RubyGems/Helm/Composer/Generic 标注为 Native adapter，`go/conan/conda/alpine/debian/rpm/swift` 标注为 Generic fallback，避免把 type 枚举误读为 17 种完整专用协议实现。

### 前后端映射解读

第 6 轮可以形成以下判断：

- 前端是真正的 SPA 管理台和仓库工作台，后端负责 API 和静态托管；没有 SSR 边界，也没有 server load 数据层。
- 前端页面基本按 Git 托管产品的信息架构组织：全局导航、用户工作台、仓库页、仓库 settings、admin、package/import/search 等扩展页面。
- `client.svelte.ts` 是 38 行的纯 re-export 聚合入口。API 真实实现已拆到 `repos.ts`、`auth.ts`、`admin.ts`、`packages.ts`、`issues.ts`、`pulls.ts`、`websockets.ts` 等领域模块；后续新增领域应继续优先独立建模块，再从主入口转导。
- 认证状态已从 localStorage token 迁到 HttpOnly cookie 主导；内存 token 只用于登录后立即调用或兼容 Bearer 场景。
- 前端使用 REST 覆盖了绝大多数 `/api/v1` 能力；非 REST 协议面直接使用 Git HTTP clone URL、notification WebSocket 和 CI job log WebSocket。OCI `/v2`、Git protocol、Runner agent API 都不是浏览器页面的主要调用者。

### 待核验项

| 问题 | 原因 | 下一步 |
|------|------|--------|
| 旧拆分 API 文件是否仍需保留 | 已处理：`auth.ts/repos.ts/...` 改为从 `client.svelte.ts` re-export | 保留兼容路径，避免重复实现漂移 |
| `client.svelte.ts` 是否过大 | 已降至 38 行纯 re-export；领域模块已全部抽出 | 后续新增领域优先独立模块 |
| Package formats 前后端是否一致 | 已核验：17 种 type 枚举中部分为 native adapter，其余走 Generic fallback | 已在前端列表/上传页和文档中标注 |
| Markdown sanitizer 安全边界 | 已处理：DOMParser + allowlist sanitizer，并补 fallback smoke | 后续如引入 DOMPurify，可替换当前实现 |
| `instance.svelte.ts` 注释与实现不一致 | 注释提到 `g i/g p`，实际只实现 `?` | followups 视需要修正文档或实现 |

### 可进入最终文档的内容

可直接进入最终前后端结构文档的前端总览：

> IronForge 前端位于 `web/`，是 SvelteKit 2 + Svelte 5 构建的静态 SPA。构建产物输出到 `web/build`，由 `rg-http` 静态托管并通过 `index.html` fallback 支持前端路由。前端关闭 SSR 和 prerender，页面数据通过浏览器端 API client 调用 `/api/v1` 获取。

可直接进入最终文档的前端结构表：

| 目录 | 职责 |
|------|------|
| `web/src/routes` | SvelteKit 页面路由，覆盖首页、登录、dashboard、仓库、Issue、PR、Wiki、CI、Package、Settings、Admin 等页面 |
| `web/src/lib/api` | API client 与请求基础层，事实主入口为 `client.svelte.ts` 和 `_base.svelte.ts` |
| `web/src/lib/stores` | 认证、实例 banner、快捷键等全局状态 |
| `web/src/lib/components` | Navbar、RepoHeader、FileEditor、PipelineBadge 等复用 UI |
| `web/src/lib/i18n` | 中英文翻译、locale store、日期格式化 |
| `web/src/lib/utils` | Markdown 渲染/清洗、搜索高亮、行 diff |

可直接进入最终文档的前后端调用关系：

```text
Svelte routes/components
  -> $lib/api/client.svelte.ts
  -> $lib/api/_base.svelte.ts request()
  -> /api/v1 REST API with credentials: include

RepoHeader clone UI
  -> withBackendBase('/git/{owner}/{repo}')
  -> Git Smart HTTP endpoint

Notifications page
  -> connectNotificationWebSocket()
  -> /api/v1/ws/notifications

Root layout health check
  -> withBackendBase('/health')
```

---

## 第 7 轮：安全、认证、配置与横切能力

### 分析范围

本轮围绕安全与横切能力核验以下内容：

- 启动配置、密钥来源、配置校验和运行时开关；
- JWT、HttpOnly Cookie、PAT、MFA、SSO、LDAP 等认证能力；
- HTTP 安全中间件、CORS、CSP、Request-ID、Rate Limit、维护模式；
- Admin、OpenAPI 文档、审计日志等管理面鉴权；
- 与前端认证模型、Git/SSH 协议面相关的安全边界。

主要源码入口：

| 主题 | 文件 |
|------|------|
| 启动配置与校验 | `crates/rg-cli/src/main.rs` |
| 认证 helper | `crates/rg-http/src/api/auth.rs` |
| 登录、注册、PAT、密码重置 | `crates/rg-http/src/api/users.rs`、`crates/rg-core/src/user/service.rs` |
| JWT / Password / MFA / SSO / LDAP | `crates/rg-core/src/auth/` |
| MFA HTTP API | `crates/rg-http/src/api/mfa.rs` |
| SSO HTTP API | `crates/rg-http/src/api/sso.rs` |
| 安全 headers / CSP | `crates/rg-http/src/security.rs` |
| CORS / Request-ID / 维护模式 | `crates/rg-http/src/middleware.rs` |
| 限流 | `crates/rg-http/src/rate_limit.rs` |
| Admin / Audit / Docs auth | `crates/rg-http/src/api/admin.rs`、`crates/rg-http/src/api/audit.rs`、`crates/rg-http/src/lib.rs` |

### 启动配置与密钥模型

`ironforge serve` 中 JWT secret 的解析顺序为：

```text
IRONFORGE_JWT_SECRET
  -> --jwt-secret
  -> config auth.jwt_secret
  -> fatal error
```

`validate_jwt_secret` 会拒绝 `change-me-in-production`，并对长度小于 16 的 secret 发出警告。`validate_config` 还会检查：

- JWT secret 是否可接受；
- `repo_root` 是否存在且可写；
- TLS cert/key 文件是否存在；
- 其他启动参数的基础一致性。

本轮确认的配置边界：

| 项 | 当前实现 |
|----|----------|
| JWT secret | 无默认值，必须通过环境变量、CLI 或配置文件提供 |
| Git CLI timeout | 启动时注入 `rg_git::cli_gateway::init_global_gateway` |
| 日志 | 支持 stdout/file/both，文件日志使用 daily rolling appender |
| TLS | 通过 `--tls-cert` / `--tls-key` 启用 HTTPS |
| external_url | 主要来自配置文件；未看到对应 CLI 参数 |
| log_max_size_mb | 参数存在，但当前 daily rolling appender 不按大小轮转 |

第 1 轮已记录过一个配置合并风险：`repo_root`、`http_addr`、`ssh_addr`、`db_url` 等 clap 字段有默认值，当前 `run_serve` 中直接使用 clap 值，可能导致配置文件同名字段无法覆盖默认值。最终文档应将其列为“当前实现注意事项”，不要写成完整的 config-first 模型。

### 认证模型总览

后端认证能力分为几类：

| 类型 | 实现位置 | 主要用途 |
|------|----------|----------|
| JWT 用户 token | `rg-core/src/auth/jwt.rs` | Web 登录、REST API、WebSocket |
| HttpOnly Cookie | `rg-http/src/api/users.rs`、`api/auth.rs` | 浏览器会话，cookie 名为 `ironforge_token` |
| Personal Access Token | `rg-http/src/api/users.rs`、`rg-http/src/lib.rs` | API client、Git HTTP Basic/Bearer |
| CI job token | `rg-core/src/auth/ci_token.rs` | CI job 作用域 token，HTTP 侧 extractor 仍标注待接入 |
| OCI token | `rg-core/src/auth/oci_token.rs` | `/v2/` 容器镜像仓库授权 |
| MFA / TOTP | `rg-core/src/auth/totp.rs`、`rg-http/src/api/mfa.rs` | 两步验证与 backup codes |
| OAuth2 SSO | `rg-core/src/auth/sso.rs`、`rg-http/src/api/sso.rs` | OAuth provider 登录 |
| LDAP | `rg-core/src/auth/ldap.rs` | LDAP bind 认证能力；本轮未发现登录 API 直接接入 |

JWT 使用 HS256，claims 包含 `sub`、`username`、`iat`、`exp`。常规登录、注册、密码重置和 MFA verify 后生成 7 天 token；PAT 中间件在识别 PAT 后会临时转换为 1 天 JWT，供 REST handler 复用 Bearer 鉴权逻辑。

密码使用 Argon2 hash。注册和密码重置都会调用 `PasswordValidator::standard()`，规则包括长度、大小写、数字、特殊字符、空白字符、常见弱口令和用户名包含检查。

### HttpOnly Cookie 与 Bearer JWT 分裂

`api/auth.rs::extract_user_id` 支持：

```text
ironforge_token HttpOnly cookie
  -> Authorization: Bearer <jwt>
```

但 `extract_bearer_claims` 只读取 `Authorization: Bearer`。本轮通过 `rg` 看到大量 API handler 仍直接调用 `extract_bearer_claims`，其中包括：

- `GET /users/me`；
- PAT token 管理相关接口；
- admin 基础鉴权 `require_admin`；
- audit 查询；
- SSO refresh / unlink；
- 部分 repos、labels、releases、mirrors、boards、time_tracking、imports、lfs、repo_content 等接口；
- `/api-docs` 文档鉴权中间件。

这与第 6 轮前端观察形成了一个关键风险：前端 `_base.svelte.ts` 设置 `credentials: include`，并且注释上已经切到 HttpOnly Cookie 主导；登录后当前内存中还会保留 token，因此同一浏览器会话内 Bearer 仍可工作。但页面刷新后内存 token 丢失，如果 `/users/me` 仍只接受 Bearer，则登录状态恢复会失败。Admin 页面、API docs 和其他 bearer-only 接口也可能出现同类问题。

这一点应进入最终 followups，优先级高于普通文档整理。合理修复方向是统一 REST 用户鉴权入口：面向浏览器的 API 尽量使用 `extract_user_id` 或 `AuthUser` extractor；仅真正需要纯 Bearer 的机器接口保留 `extract_bearer_claims`。

### Cookie 行为

登录、MFA verify、密码重置成功后都会返回 JSON token，并设置：

```text
Set-Cookie: ironforge_token=<jwt>; HttpOnly; Path=/; SameSite=Strict; Max-Age=604800
```

当请求头 `x-forwarded-proto: https` 时追加 `Secure`。登出会清空同名 cookie。

需要注意：

- Secure 判断依赖 `x-forwarded-proto`，没有看到基于实际 TLS listener 的直接判断；
- 如果反向代理没有正确设置该 header，HTTPS 部署下 cookie 可能缺少 `Secure`；
- 如果外部是 HTTP，本地测试则不会设置 `Secure`，便于开发。

### PAT 与 Git HTTP 鉴权

PAT 生成逻辑：

```text
raw token: ifp_<time entropy>
stored token: sha256(raw token)
```

raw token 只在创建时返回；后续验证时对输入 token 做 SHA-256 后查库，并检查过期时间。

PAT 进入系统有两条主要路径：

| 路径 | 行为 |
|------|------|
| REST API | `pat_auth_middleware` 接收 Bearer、query token 或 Basic 形式 PAT，解析成功后转换为临时 JWT |
| Git HTTP | `extract_actor_id` 支持 Bearer JWT/PAT，也支持 Basic username/password token 或 token-as-username 形式 |

这种设计使 PAT 可以复用 JWT handler，但也造成“某些路由是否经过 PAT middleware”的边界需要最终确认。`/api-docs` 注释写着 JWT/PAT bearer token，但当前 docs auth middleware 只调用 `extract_bearer_claims`，并不直接解析 PAT；如果没有上游 PAT middleware 覆盖，PAT 访问 docs 会失败。

### MFA / TOTP

MFA 能力包括：

- setup：生成 TOTP secret、otpauth URL、QR SVG；
- enable：验证 TOTP 后启用 MFA，并生成 8 个 backup codes；
- verify：用户名 + TOTP 或 backup code，通过后签发 JWT 并设置 HttpOnly cookie；
- disable：要求当前密码；
- backup status：只返回 used/created 元数据，不泄露明文 backup codes。

TOTP 使用 SHA1、6 位、30 秒步长。TOTP secret 使用由 JWT secret 派生出的 AES-256-GCM key 加密存储。

本轮发现一个高优先级实现风险：`disable_mfa` 调用 `verify_password(&req.password, &user.password_hash)` 后只处理 `Err`，没有检查返回的 `bool`。如果 `verify_password` 对错误密码返回 `Ok(false)`，当前代码仍会继续执行 `disable_mfa`。这应作为安全修复项，而不是只进入架构文档备注。

### SSO 与 LDAP

SSO 使用自实现 OAuth2 流程：

```text
GET /auth/sso/{provider}/authorize
  -> 生成 state + PKCE verifier
  -> 设置 ironforge_sso_state / ironforge_sso_code_verifier cookie
  -> redirect provider authorize URL

GET /auth/sso/{provider}/callback
  -> 校验 state cookie
  -> 使用 code + PKCE verifier 换 token
  -> 拉取 userinfo
  -> link/create user
  -> 返回 LoginResponse JSON
```

当前观察到的注意点：

- SSO state cookie 使用 `Path=/auth/sso; HttpOnly; SameSite=Lax; Max-Age=600`；
- state cookie 的签名是 `sha256(secret:value)` 形式，不是 HMAC；
- callback 在 query 中缺少 `state` 时会记录 warning 并继续，这是兼容旧实现的逻辑，但会削弱 CSRF 防护；
- callback 返回 JSON `LoginResponse`，本轮未看到同时设置 `ironforge_token` cookie 或 redirect 回前端页面，浏览器端 SSO 登录闭环需要单独实测；
- SSO refresh / unlink 使用 bearer-only claims，存在与 HttpOnly cookie 模型不一致的问题。

LDAP 模块实现了 service account bind、搜索用户、再用用户 DN bind 验证密码的流程。但本轮搜索未发现 `ldap::authenticate` 或 `LdapConfig` 在登录 API 中被直接调用，因此应写成“LDAP 认证能力已存在，登录集成需继续核验”。另外，`ldap.rs` 使用 `LdapConnSettings::new().set_no_tls_verify(true)`，无论是否 TLS 都禁用了证书校验；生产 LDAP/LDAPS 场景需要修正或提供显式配置开关。

### HTTP 安全 Headers 与 CSP

`security_headers_middleware` 会为每个请求生成 nonce，并注入以下安全 header：

| Header | 当前策略 |
|--------|----------|
| `X-Content-Type-Options` | `nosniff` |
| `X-Frame-Options` | `DENY` |
| `X-XSS-Protection` | `0` |
| `Referrer-Policy` | `strict-origin-when-cross-origin` |
| `Strict-Transport-Security` | 仅 HTTPS 或 `x-forwarded-proto=https` 时设置 |
| `Content-Security-Policy` | `default-src 'self'; script-src 'self' 'nonce-...'; style-src 'self' 'unsafe-inline'; img-src 'self' data: https:; font-src 'self' data:; connect-src ...; frame-ancestors 'none'; base-uri 'self'; form-action 'self'` |
| `Permissions-Policy` | 禁用 camera/microphone/geolocation/payment/usb 等 |
| `Cross-Origin-Opener-Policy` | `same-origin` |
| `Cross-Origin-Resource-Policy` | `same-origin` |

SPA fallback handler 会读取 `web/build/index.html`，把 nonce 注入 `<script>` 标签后返回，配合 CSP `script-src 'nonce-...'` 使用。

需要进入 followups 的边界：

- CSP `style-src 'unsafe-inline'` 是 Svelte/样式兼容取舍，安全性低于纯 nonce/hash；
- CSP `connect-src` 默认只有 `'self'`；若配置 `IRONFORGE_CORS_ORIGINS` 或 `IRONFORGE_CSP_CONNECT_SRC`，会追加跨域 API origin 和对应 WS origin；
- Markdown 渲染已从正则清洗升级为前端 DOMParser + allowlist sanitizer；`style/on*`、危险 URL 和非 allowlist 标签会被移除或展开，server fallback 有 smoke 覆盖。

### CORS、Request-ID、Rate Limit 与维护模式

| 能力 | 当前实现 |
|------|----------|
| CORS | 若 `IRONFORGE_CORS_ORIGINS` 存在则使用白名单；否则 warning 并 mirror request origin，同时允许 credentials |
| Request-ID | 读取或生成 `X-Request-Id`，写入 request extensions 和 response header，并向 JSON error body 注入 `request_id` |
| Rate Limit | token bucket per-IP，`max_requests=0` 时禁用 |
| IP 识别 | 优先 `X-Forwarded-For` 第一段，再 `X-Real-IP`，最后 socket addr |
| 维护模式 | 允许 GET/HEAD/OPTIONS 和 `/api/v1/admin/`，拒绝 mutating 请求 |

限流的部署边界：当前直接信任 `X-Forwarded-For` / `X-Real-IP`。如果服务直接暴露到公网，客户端可以伪造 IP 绕过 per-IP 限流；生产部署应依赖可信反向代理剥离/重写这些 header，或在后端增加 trusted proxy 配置。

维护模式返回的是独立 JSON：

```json
{"error":"Instance is in maintenance mode. Writes are temporarily disabled.","code":"MAINTENANCE_MODE"}
```

它不完全走 `AppError` 统一 envelope。最终 API 文档可以记录这一点，或者 followups 建议统一错误结构。

### Admin、审计与 OpenAPI 文档

Admin 和 Audit 的鉴权集中在 `require_admin` 与 audit API handler。`require_admin` 当前读取 Bearer claims，再查用户 `is_admin`。这意味着 Admin API 与第 6 轮前端的 cookie-first 模型也有差异：前端刷新后如果没有内存 Bearer token，admin 页面可能无法继续访问。

审计日志 API 是 admin-only，支持按 actor/action/resource/time/window 等条件查询。审计记录还会采集 IP 和 User-Agent。

OpenAPI 文档路由 `/api-docs` 当前挂了 `docs_auth_middleware`，只接受 Bearer JWT claims。注释提到 JWT/PAT bearer token，但实现上没有直接解析 PAT，也没有读取 HttpOnly cookie。最终文档应写成“受认证保护”，并在 followups 中核验 PAT/cookie 兼容。

### 上传与 body size

本轮确认：

- OCI `/v2` 上传子路由和 LFS upload 路由有 `RequestBodyLimitLayer::new(10 GiB)`；
- 普通 REST API 未在本轮看到统一的大 body limit 约束；
- 这类限制应在部署文档中和反向代理 `client_max_body_size` 等配置一起描述。

### 安全风险与后续修复清单

| 优先级 | 问题 | 影响 | 建议 |
|--------|------|------|------|
| P0 | `GET /users/me` 等大量接口仍只读 Bearer，不读 HttpOnly cookie | 页面刷新后前端登录态恢复、Admin、Docs、部分领域 API 可能失败 | 统一浏览器 API 鉴权入口，优先使用 `extract_user_id` / `AuthUser` |
| P0 | MFA disable 未检查 `verify_password` 返回的 `bool` | 错误密码可能禁用 MFA | 显式要求 `verify_password(...) == true` |
| P0 | SSO callback 缺少 state 时继续执行 | OAuth CSRF 防护被削弱 | 将缺失/不匹配 state 改为拒绝，必要时做迁移兼容开关 |
| P1 | SSO callback 返回 JSON，未见设置 auth cookie 或回跳前端 | 浏览器 SSO 登录闭环可能不可用 | 实测并统一为 redirect + HttpOnly cookie 或前端 callback 页面 |
| P1 | LDAP TLS 证书校验被禁用 | LDAPS 易受中间人攻击 | 默认校验证书，允许显式 insecure 开关仅用于测试 |
| P1 | Rate limit 信任 XFF/X-Real-IP | 直连部署可绕过 IP 限流 | 增加 trusted proxy 配置或仅在可信代理后读取 |
| P1 | `/api-docs` 注释与实现不一致 | PAT/cookie 访问文档可能失败 | 明确 docs auth 策略并修正 middleware |
| P1 | SSH Git 鉴权缺少 repo-level can_read/can_write | 第 5 轮已记录，SSH 可能绕过仓库权限 | 在 SSH exec path 接入 repo permission 检查 |
| 已修复 | CSP `connect-src 'self'` 与跨域 API base 不兼容 | 已支持从 `IRONFORGE_CORS_ORIGINS` / `IRONFORGE_CSP_CONNECT_SRC` 生成跨域 API/WS origin | 继续保持部署文档同步 |
| 已修复 | Markdown sanitizer allowlist | 已改为 DOMParser + allowlist sanitizer，并补 smoke 测试 | 后续可视需要替换为 DOMPurify |

### 可进入最终文档的内容

可直接进入最终架构文档的安全总览：

> IronForge 的用户认证以 HS256 JWT 为核心，浏览器会话通过 `ironforge_token` HttpOnly Cookie 承载，API client 和 Git HTTP 通过 Personal Access Token 访问。后端还包含 TOTP MFA、OAuth2 SSO、LDAP bind、CI job token、OCI registry token 等扩展认证能力。HTTP 层统一注入安全 headers、CSP nonce、Request-ID、CORS、Rate Limit 和维护模式中间件。

可直接进入最终前后端结构文档的认证关系：

```text
Browser login/register/MFA/reset
  -> /api/v1/users/*
  -> JWT + Set-Cookie ironforge_token
  -> frontend request(..., credentials: include)

API client / Git HTTP
  -> Authorization: Bearer <jwt or PAT>
  -> PAT middleware or Git auth resolver

WebSocket
  -> cookie ironforge_token
  -> or Sec-WebSocket-Protocol bearer.<jwt>
  -> or query token
```

最终 followups 应单独列出“cookie-first 迁移未完全落地”和“MFA disable 密码校验 bug”，这两项已经超出文档准确性问题，属于需要代码修复的安全/可用性缺口。

---

## 第 8 轮：CI/Runner、Package Registry、MCP 与扩展能力

### 分析范围

本轮分析项目中的扩展平台能力，重点覆盖：

- CI/CD 引擎、pipeline API、内置 runner 与外部 runner；
- artifact、job log WebSocket 与 CI job token；
- Package Registry 通用模型、协议 adapter、OCI `/v2/` registry；
- MCP server 对 AI Agent 暴露的 tools/resources；
- 前端对 pipelines、packages、runners 的接入程度；
- 这些扩展能力与第 7 轮安全模型之间的权限边界。

主要源码入口：

| 能力 | 文件 |
|------|------|
| CI 引擎 | `crates/rg-ci/src/lib.rs`、`config.rs`、`gitea_actions.rs`、`runner.rs` |
| Pipeline REST API | `crates/rg-http/src/api/ci.rs` |
| Runner REST API | `crates/rg-http/src/api/runners.rs` |
| Artifact API | `crates/rg-http/src/api/artifacts.rs` |
| 独立 Runner Agent | `crates/rg-runner/src/main.rs` |
| 主 CLI Runner 模式 | `crates/rg-cli/src/main.rs` |
| Package Registry 核心 | `crates/rg-core/src/package_registry/` |
| Package Registry HTTP | `crates/rg-http/src/api/packages.rs` |
| OCI Distribution API | `crates/rg-http/src/oci.rs` |
| MCP Server | `crates/rg-mcp/src/` |
| 前端接入 | `web/src/routes/[owner]/[repo]/pipelines`、`packages`、`web/src/routes/admin/runners`、`web/src/lib/api/client.svelte.ts` |

### CI/CD 引擎结构

CI 引擎位于 `rg-ci`，但 HTTP 层并不直接依赖 `rg-ci`。依赖关系是：

```text
rg-core::ci::CiTrigger trait
  <- rg-ci::CiEngine implements trait
  <- rg-cli::run_serve injects Arc<rg_ci::CiEngine> into AppState
  <- rg-http calls state.ci_engine
```

这条链路保持了第 2 轮记录的 crate 解耦：`rg-http` 只依赖 `rg-core` 中的 trait，不依赖具体 CI 引擎 crate。

CI 配置读取顺序：

```text
.gitea/workflows/*.yml
  -> Gitea/GitHub Actions 兼容转换
  -> internal CiConfig

.ironforge-ci.yml
  -> native CiConfig
```

`CiConfig` 支持：

- `stages`；
- `concurrency.group` / `cancel_in_progress`；
- job `stage`、`script`、`image`、`only`、`variables`、`when`、`allow_failure`、`tags`。

Gitea Actions 兼容层支持：

- `on: push`、`pull_request` 及分支过滤；
- `jobs.<id>.runs-on` 到 runner labels；
- `steps[].run` 到 script；
- `steps[].uses` 中 `actions/checkout` 隐式支持，其他 action 忽略；
- `container.image` 到 Docker image；
- job/workflow env；
- `needs` 生成 stage 顺序；
- 基础 `${{ }}` 表达式替换。

### Pipeline 执行模型

pipeline 创建流程：

```text
trigger_pipeline
  -> read_ci_config(repo_path, commit_sha, ref_name, event)
  -> concurrency check
  -> create pipeline
  -> create stages
  -> create jobs
  -> if external_runners=false: spawn embedded PipelineRunner
  -> if external_runners=true: leave jobs pending for external runners
```

内置 `PipelineRunner` 的执行模型：

- 按 stage 顺序执行；
- 同一 stage 内按 jobs 列表顺序执行；
- stage 失败后后续 stage 标记 skipped；
- local job 使用 `sh -c` 或 PowerShell；
- Docker job 使用 `docker run --rm --name ironforge-job-{id} -v repo:/workspace -w /workspace image sh -c script`；
- job 默认超时 3600 秒；
- local job 使用 `env_clear()`，只保留 CI 标记、`CI_PIPELINE_ID`、可选 `CI_JOB_TOKEN`、`PATH`、`LANG` 和临时 `HOME`；
- Docker job 如果 Docker 不可用会失败，不再回退到本地执行。

CI job token：

```text
sub = ci:job:{job_id}
repo_id = triggering repo id
scope = repo:read packages:read
iss = ironforge-ci
ttl = 1 hour
```

但 HTTP 侧 `extract_ci_job_claims` 标注为 TODO，尚未看到 CI token 被接入实际 repo/package handler。因此当前 token 生成能力已经存在，API 授权闭环仍需继续接入。

### Runner Agent 与 Runner API

Runner API 分两类：

| 类型 | 路由 | 鉴权 |
|------|------|------|
| 注册 | `POST /api/v1/runners/register` | 需要 Bearer JWT |
| Runner 自身操作 | `/api/v1/runners/{id}/heartbeat`、`deregister`、`jobs/poll`、`jobs/{job_id}/start/log/finish/artifacts` | `authenticate_runner` 校验 runner token 与 path id 一致 |
| Admin 管理 | `/api/v1/admin/runners`、`/admin/runners/{id}` | admin Bearer JWT |

外部 runner 执行链路：

```text
ironforge-runner run/register
  -> POST /api/v1/runners/register
  -> GET /api/v1/runners/{id}/jobs/poll?timeout=30
  -> POST /start
  -> run local shell or docker
  -> POST /log
  -> POST /finish
```

当前存在两套 runner client：

- `crates/rg-runner/src/main.rs` 独立二进制 `ironforge-runner`；
- `crates/rg-cli/src/main.rs` 中的 `ironforge runner` 子命令。

两者都调用同一组 HTTP runner API。需要注意的是，独立 runner 和主 CLI runner 的注册请求没有附带 Authorization header，而后端 `runners::register` 当前要求 Bearer JWT。也就是说，命令行“自动注册 runner”的设计与后端鉴权实现不一致；除非前端 admin 页面先注册并手动提供 runner id/token，否则命令行自注册路径可能失败。

另一个执行安全差异：内置 `PipelineRunner` 在 Docker 不可用时会失败；独立 `ironforge-runner` 和主 CLI runner 的 `run_job_docker` 会在 Docker 不可用时回退到 local execution。这与内置 runner 的安全策略不一致，可能让写给容器环境的 job 在宿主机权限下执行。

### Pipeline / Artifact / Log 前端接入

前端 pipelines 页面接入：

```text
web/src/routes/[owner]/[repo]/pipelines/+page.svelte
  -> pipelines.list/get/trigger/retry/cancel/job
  -> /api/v1/repos/{owner}/{repo}/pipelines...
```

页面会对 running pipeline 做轮询刷新；job log 弹窗会先通过 REST `get_job` 获取已有日志，再订阅 `/api/v1/ws/job/{job_id}` 实时追加 runner log chunk。

Artifact API 当前更像 metadata 管理：

- runner 上传 artifact 时提交 `name`、`file_path`、`size`；
- 后端校验 job 是否属于该 runner；
- API 支持 list/get/delete artifact metadata；
- 本轮未看到 artifact 文件字节上传、服务端文件保存或下载 endpoint。

因此最终文档应写成“Artifact metadata API 已有”，不要写成完整 artifact 文件存储/下载链路。

### Package Registry 核心模型

Package Registry 的领域模型分为：

```text
repository
  -> package_registry(repo_id, package_type)
    -> package(author_id, name, metadata)
      -> package_version(version, semver, metadata, size, sha256, yanked)
        -> package_file(filename, size, sha256, storage_path)
```

文件存储路径：

```text
{repo_root}/{owner}/{repo}/packages/{type}/{name}/{version}/{filename}
```

通用发布流程：

```text
POST /api/v1/repos/{owner}/{repo}/packages/{type}/publish
  -> extract user id from cookie or Bearer
  -> validate package type is in package_types::ALL
  -> infer filename from Content-Disposition
  -> adapter.extract_metadata(...)
  -> resolve name/version from adapter or query
  -> PackageStorage.store_file
  -> DB package/version/file records
```

需要注意：`PackageAdapter` trait 有 `validate()`，但本轮在 `packages.rs::publish` 中只看到 `extract_metadata()` 被调用，未看到 `validate()` 被显式执行。如果某些 adapter 的 `extract_metadata()` 不等价于完整校验，发布入口可能弱于设计意图。

### Package 类型与协议端点

`package_types::ALL` 和前端 `PACKAGE_FORMATS` 均列出 17 种：

```text
cargo, npm, maven, pypi, docker, nuget, rubygems, go, helm, composer,
conan, conda, alpine, debian, rpm, swift, generic
```

但真正有专用 adapter 的类型是：

```text
cargo, npm, nuget, pypi, rubygems, maven, docker, generic, helm, composer
```

其余 `go/conan/conda/alpine/debian/rpm/swift` 当前会落到 `GenericAdapter`。因此最终文档应区分：

- “注册表通用存储层支持的 package_type 枚举”；
- “已有专用解析/协议适配的生态”；
- “前端可选但后端按 generic 存储处理的格式”。

HTTP protocol-specific endpoints：

| 生态 | 端点 |
|------|------|
| Cargo | `/packages/cargo/index/{pkg}` |
| npm | `/packages/npm/{pkg_name}` |
| PyPI | `/packages/pypi/simple/{pkg_name}` |
| Maven | `/packages/maven/{group_id}/{artifact_id}/maven-metadata.xml` |
| NuGet | `/packages/nuget/index.json`、`registration/{id}/index.json`、`query` |
| RubyGems | `/packages/rubygems/api/v1/dependencies`、`gems/{gem_name}` |
| Helm | `/packages/helm/index.yaml` |
| Composer | `/packages/composer/packages.json` |
| Generic REST | publish/list/get/version/delete/yank/download |

Docker/OCI 不走 `/api/v1/repos/.../packages/docker/publish`，而是走独立 `/v2/` OCI Distribution API。

### OCI `/v2/` Registry

OCI 路由挂载在顶层 `/v2`：

```text
GET  /v2/
GET  /v2/auth/token
GET  /v2/{owner}/{repo}/tags/list
GET  /v2/{owner}/{repo}/manifests/{reference}
HEAD /v2/{owner}/{repo}/manifests/{reference}
PUT  /v2/{owner}/{repo}/manifests/{reference}
GET  /v2/{owner}/{repo}/blobs/{digest}
HEAD /v2/{owner}/{repo}/blobs/{digest}
POST /v2/{owner}/{repo}/blobs/uploads/
PATCH /v2/{owner}/{repo}/blobs/uploads/{uuid}
PUT  /v2/{owner}/{repo}/blobs/uploads/{uuid}
```

OCI token：

- HS256 JWT；
- issuer `ironforge`；
- audience `ironforge-registry`；
- scope 形如 `repository:owner/repo:pull,push`；
- authenticated TTL 300 秒，anonymous TTL 60 秒。

OCI 存储路径位于：

```text
{repo_root}/{owner}/{repo}/oci/
```

OCI 访问检查有一个需要单独修复的边界：`check_repo_access` 在没有 token 时对 `pull` 返回 true，注释写的是“public pull”。但本轮没有看到它校验 IronForge repository visibility。如果私有仓库也能走匿名 pull，则是严重权限问题；至少需要在 OCI handler 中结合 repo visibility 或 `repo::service::can_read`。

### MCP Server

`ironforge-mcp` 是独立二进制，默认 stdio JSON-RPC：

```text
IRONFORGE_URL=http://localhost:8080
IRONFORGE_PAT=<token>
ironforge-mcp
```

支持方法：

- `initialize`；
- `tools/list`；
- `tools/call`；
- `resources/list`；
- `resources/read`；
- `notifications/initialized`；
- `notifications/cancelled`。

Tools：

| Tool | 行为 |
|------|------|
| `list_repos` | 调 `/api/v1/repos` |
| `read_file` | 调 `/api/v1/repos/{owner}/{repo}/contents/{path}` |
| `read_dir` | 调 `/api/v1/repos/{owner}/{repo}/tree/{ref}?path=...` |
| `get_issue` | 调 `/api/v1/repos/{owner}/{repo}/issues/{number}` |
| `get_pr` | 调 `/api/v1/repos/{owner}/{repo}/pulls/{number}` |

Resources：

| URI | 行为 |
|-----|------|
| `repo://{owner}/{name}` | 仓库元数据 JSON |
| `file://{owner}/{name}/{path}` | 文件内容 |
| `issue://{owner}/{name}/{number}` | Issue JSON |

本轮发现一个实现风险：`rg-mcp` 的 `main` 是同步函数，`run_stdio` 也不在 Tokio runtime 内，但 tools/resources 内部使用 `tokio::runtime::Handle::current().block_on(...)` 调异步 reqwest client。若运行时不存在，tool/resource 调用会 panic。合理修复是改成 `#[tokio::main]`，或在 MCP 内部显式创建 runtime，或改用 blocking reqwest。

`--sse` 参数目前返回 “SSE transport is not implemented; use stdio...”，不能写成可用 transport。

### 前端接入范围

| 能力 | 前端页面 |
|------|----------|
| Pipelines | `/{owner}/{repo}/pipelines` |
| Packages 列表/详情/上传/删除 | `/{owner}/{repo}/packages`、`packages/upload`、`packages/{format}`、`packages/{format}/{name}` |
| Runner Admin | `/admin/runners` |
| Repo runners settings | `/{owner}/{repo}/settings/runners` 仅跳转到 admin runners |

前端 package UI 使用 `PACKAGE_FORMATS` 17 种格式，并按后端 registry 列表聚合显示。上传使用通用 `/packages/{type}/publish`，文件名通过 `Content-Disposition` 传递。

### 扩展能力风险与后续修复清单

| 优先级 | 问题 | 影响 | 建议 |
|--------|------|------|------|
| P0 | Pipeline list/get/job/trigger/retry/cancel 本轮未看到认证或仓库权限检查 | 未授权用户可能查看 CI 日志、手动触发/取消 pipeline | 接入 `extract_user_id` + `can_read/can_write` |
| P0 | Package list/download/protocol endpoints 未看到 repo read 权限，publish/delete/yank 只要求登录未见 can_write | 私有包泄露或任意登录用户向他人仓库发包/删包 | 按 read/write 权限区分所有 package endpoint |
| P0 | OCI anonymous pull 未见 repo visibility 校验 | 私有镜像可能匿名拉取 | pull 时校验 public visibility 或 can_read |
| P0 | Runner CLI 自动注册不带 Authorization，但后端注册要求 Bearer JWT | `ironforge-runner register/run` 自注册路径可能不可用 | 设计注册 token / admin-created token / PAT 参数 |
| P1 | 独立 runner Docker 不可用时回退 local，与内置 runner 安全策略不一致 | 容器预期 job 可能在宿主机执行 | 外部 runner 也应默认 fail closed |
| P1 | MCP tools/resources 在无 Tokio runtime 的同步 main 中 `Handle::current().block_on` | MCP tool 调用可能 panic | 改为 tokio main 或显式 runtime |
| P1 | CI_JOB_TOKEN 生成后 HTTP handler 未接入验证 | CI 内访问 API 的最小权限模型未闭环 | 将 `extract_ci_job_claims` 接入 repo/package 读路径 |
| P1 | Artifact API 只管理 metadata，未见文件上传/下载 | 用户可能误以为 artifact 已完整支持 | 明确范围或补文件存储/下载链路 |
| P2 | PackageAdapter::validate 未在 publish 路径显式调用 | 包格式校验弱于 trait 设计 | publish 时先 validate，再 extract metadata |
| P2 | 17 种 package type 与专用协议支持范围不一致 | 文档和 UI 易误导 | 已修复：UI/文档标注 Generic fallback 类型 |

### 可进入最终文档的内容

可直接进入最终架构文档的 CI 总览：

> IronForge 的 CI/CD 能力由 `rg-ci` 实现，通过 `rg-core::ci::CiTrigger` trait 注入到 HTTP 层。CI 支持原生 `.ironforge-ci.yml` 和 `.gitea/workflows/*.yml` 兼容转换。pipeline 记录、stage 和 job 存入数据库，执行可由服务端内置 runner 直接完成，也可在 `external_runners` 模式下交给独立 `ironforge-runner` 长轮询执行。

可直接进入最终架构文档的 Package Registry 总览：

> Package Registry 由通用 DB 模型、文件系统存储和 per-ecosystem adapter 组成。REST 层提供通用 publish/list/get/delete/yank/download API，并为 Cargo、npm、PyPI、Maven、NuGet、RubyGems、Helm、Composer 暴露协议元数据端点。Docker/OCI 镜像通过独立的 `/v2/` OCI Distribution API 处理。

可直接进入最终前后端结构文档的扩展能力映射：

```text
Repo pipelines page
  -> /api/v1/repos/{owner}/{repo}/pipelines...

Admin runners page
  -> /api/v1/admin/runners
  -> /api/v1/runners/register

Repo packages pages
  -> /api/v1/repos/{owner}/{repo}/packages...

Container clients
  -> /v2/auth/token
  -> /v2/{owner}/{repo}/...

AI Agent
  -> ironforge-mcp stdio
  -> IronForge REST API via IRONFORGE_PAT
```

第 8 轮结论中，CI/package/OCI 权限边界和 runner 注册不一致应进入最终 followups 的高优先级修复清单。

---

## 第 9 轮：测试、构建、部署与运维

### 分析范围

本轮分析项目从开发验证到生产运行的工程化链路，重点覆盖：

- Rust workspace 构建、release profile、覆盖率配置；
- 前端 SvelteKit 构建、类型检查和静态 SPA 输出；
- Rust 单元测试、HTTP 集成测试、脚本化冒烟/契约检查；
- Dockerfile、compose、配置文件与启动命令；
- 数据库连接、迁移、FTS rebuild、健康检查、metrics、Prometheus/Alertmanager/Grafana；
- 运维缺口：备份恢复、生产配置安全、CI 编排文件等。

主要文件：

| 主题 | 文件 |
|------|------|
| Rust workspace / release profile | `Cargo.toml` |
| 覆盖率 | `cargo-llvm-cov.toml` |
| 前端构建 | `web/package.json`、`web/svelte.config.js`、`web/vite.config.ts` |
| Docker 镜像 | `Dockerfile` |
| Docker Compose | `deploy/docker-compose.yml` |
| 观测栈 | `deploy/docker-compose.observability.yml`、`deploy/prometheus/*`、`deploy/alertmanager/*` |
| 测试 helper | `crates/rg-http/tests/common/mod.rs` |
| 全量回归脚本 | `scripts/full-interface-regression.mjs` |
| OpenAPI/前后端契约 | `scripts/openapi-interface-smoke.mjs`、`scripts/api-client-contract-check.mjs`、`scripts/frontend-backend-smoke.mjs` |
| 健康检查/metrics | `crates/rg-http/src/lib.rs`、`crates/rg-http/src/metrics.rs` |
| DB 连接/迁移 | `crates/rg-db/src/lib.rs`、`crates/rg-db/src/migrations/` |

### 构建模型

Rust workspace 仍是 9 个 crate：

```text
rg-cli, rg-core, rg-git, rg-ssh, rg-http, rg-db, rg-ci, rg-runner, rg-mcp
```

release profile：

```toml
[profile.release]
lto = true
strip = true
codegen-units = 1
panic = "abort"
opt-level = 3
```

这符合项目“单二进制、小体积部署”的目标。根 `Dockerfile` 只构建并复制主二进制 `ironforge`，没有复制 `ironforge-runner` 或 `ironforge-mcp`，因此容器镜像默认只包含主服务。

前端构建：

```text
web/package.json
  npm run check  -> svelte-check
  npm run build  -> vite build

web/svelte.config.js
  adapter-static
  fallback = index.html
  output = web/build
```

`Dockerfile` 先用 `node:22-alpine` 构建前端，再用 `rust:1.95.0-slim-bookworm` 构建后端，最后复制到 `debian:bookworm-slim` runtime。runtime 安装 `git`、`curl`、`libsqlite3-0`、`openssh-client`，以支撑 Git CLI gateway、healthcheck 和运行期依赖。

### 测试分布

本轮粗略统计：

- `crates/rg-http/tests/*.rs` 集成测试文件：17 个；
- Rust 源码中 `#[test]` / `#[tokio::test]` 标记：约 233 个；
- `scripts/` 下 contract/smoke/check 脚本：40+ 个。

后端测试结构：

```text
crates/rg-http/tests/common/mod.rs
  -> setup_test_db()
    -> temp sqlite file
    -> rg_db::run_migrations()
  -> build_test_app_state()
  -> create_router_for_test()
  -> spawn_test_app()
```

这说明 HTTP 集成测试是真正启动 Axum router 的黑盒/灰盒测试，不只是 handler 单测。测试覆盖主题包括 users、admin、org、git auth、issues、wiki、collaborators、releases、boards、time tracking、notifications、PAT、OpenAPI docs auth、SSO/audit/settings 等。

源码单测集中在：

- `rg-git`: pkt-line、sideband、Git CLI gateway、Protocol V2；
- `rg-ci`: native CI config、Gitea Actions 转换；
- `rg-core`: auth、platform、repo templates、package adapters、code indexer、CI log queue；
- `rg-db`: ops 和 per-connection PRAGMA；
- `rg-http`: rate limit、pagination、security headers、search parser 等。

### 脚本化回归体系

`scripts/full-interface-regression.mjs` 是当前最完整的本地/CI 回归入口，串联：

```text
backend:
  cargo test -p rg-http -- --nocapture
  cargo test --workspace -- --nocapture

frontend:
  cd web && npm run check
  cd web && npm run build

runtime:
  openapi-interface-smoke.mjs
  api-client-contract-check.mjs
  frontend-backend-smoke.mjs
  console-smoke.mjs
  browser-admin-smoke.mjs
```

脚本支持：

- `SKIP_BACKEND_TESTS`；
- `SKIP_FRONTEND_STATIC`；
- `SKIP_RUNTIME_SMOKES`；
- `FULL_REGRESSION_ONLY=backend|frontend|runtime|all`；
- 阶段级和步骤级 timeout；
- 阶段级和步骤级 retries；
- JSON/Markdown 回归报告。

此外，`scripts/` 中还存在大量领域 contract checks，例如 package publish/search/detail、runner registration、mfa settings、audit log、board、time tracking、repo settings、webhooks、release assets、notification websocket 等。这些脚本更像“接口契约/回归守护”，最终测试文档应按“核心自动化入口 + 领域 contract 脚本库”描述。

### OpenAPI 与前后端契约

OpenAPI 相关链路：

```text
/api-docs/openapi.json
  -> scripts/openapi-interface-smoke.mjs
  -> scripts/api-client-contract-check.mjs
```

`openapi-interface-smoke.mjs` 会读取 OpenAPI spec，生成示例参数/请求体，对路径级接口做可用性请求，并默认要求 OpenAPI 文档鉴权。`api-client-contract-check.mjs` 对比前端 API client 和 OpenAPI 路由声明、方法、参数签名。

第 7 轮曾把 `/api-docs` PAT 支持列为待核验风险。本轮通过 `crates/rg-http/tests/openapi_docs_auth_tests.rs` 修正：docs route 同时挂了 `pat_auth_middleware` 和 `docs_auth_middleware`，测试覆盖了 JWT 与 PAT 均可访问 `/api-docs/openapi.json`。因此最终 followups 不应再把“docs PAT 不支持”列为风险；仍可保留“docs 不读 HttpOnly cookie”的浏览器体验边界。

### 数据库与迁移运维

数据库当前是 SQLite-only：

```text
sea-orm = sqlx-sqlite
sea-orm-migration = sqlx-sqlite
db_url = sqlite://...?...mode=rwc
```

连接层 `rg-db/src/lib.rs::connect_with_pool` 通过 sqlx `SqliteConnectOptions` 对每个物理连接设置：

- `create_if_missing(true)`；
- `journal_mode(WAL)`；
- `synchronous(NORMAL)`；
- `busy_timeout(5s)`；
- `foreign_keys(true)`；
- `cache_size=-64000`；
- `temp_store=MEMORY`；
- `mmap_size=268435456`。

迁移执行方式：

- `ironforge serve` 启动时自动 `rg_db::run_migrations`；
- `ironforge migrate --db-url ...` 可单独执行迁移；
- `ironforge rebuild-fts --db-url ...` 可重建 FTS5 索引。

迁移目录包含 40+ 个 migration 文件，并已经包含多次纠偏迁移，例如 org/team/notification、board/time/wiki revisions、import_tasks 等单复数表名修正。第 3 轮已经记录过 fresh DB 迁移验证尝试未完成；最终文档应建议将 fresh DB migration smoke 作为固定 CI 步骤。

本轮未发现数据库备份/恢复命令。生产部署文档需要单独补充 SQLite `.backup`/快照策略，或在 PostgreSQL 支持落地后补正式备份方案。

### 健康检查与 Metrics

`/health` 返回：

```json
{
  "status": "ok|degraded|unhealthy",
  "version": "...",
  "phase": 22,
  "checks": {
    "database": "ok|error",
    "filesystem": "ok|error",
    "metrics": "ok|not_initialized",
    "git": "ok|error",
    "smtp": "ok|error"
  }
}
```

状态码规则：

- DB、filesystem、git、smtp 都 ok -> HTTP 200；
- 任一关键项失败 -> HTTP 503；
- `metrics` 只进入 checks，不参与 overall。

`/metrics` 输出 Prometheus text format。metrics registry 未初始化时当前 handler 使用 `expect("Metrics registry not initialized")`，理论上如果在测试或特殊启动路径中漏初始化会 panic。生产 `run` 路径会初始化 metrics registry。

观测栈：

- `deploy/docker-compose.observability.yml` 包含 Prometheus、Alertmanager、Grafana、node-exporter；
- `deploy/prometheus/prometheus.yml` 抓取 `/metrics`；
- `deploy/prometheus/alerts.yml` 包含 HTTP error rate、P95 latency、in-flight、DB、Git、CI、availability、memory、disk alerts；
- `deploy/alertmanager/alertmanager.yml` 示例包含 PagerDuty、Slack receiver。

需要注意：Prometheus target 示例是 `host.docker.internal:7878`，但主服务 compose 暴露的是 `8080`。这可能是旧端口或示例未同步；最终运维文档应改成与实际部署一致。

### Docker 与配置部署

`Dockerfile` 运行命令：

```text
ironforge serve
  --repo-root /data/repos
  --http-addr 0.0.0.0:8080
  --ssh-addr 0.0.0.0:2222
  --db-url sqlite:///data/ironforge.db?mode=rwc
  --log-file /data/logs/ironforge.log
```

并要求通过环境变量设置 `IRONFORGE_JWT_SECRET`。

`deploy/docker-compose.yml` 的主服务映射：

- `8080:8080` HTTP；
- `2222:2222` SSH；
- `ironforge-data:/data` 持久化 repos、DB、logs。

但 compose 示例中：

```yaml
IRONFORGE_JWT_SECRET=change-me-in-production
```

而启动代码会拒绝该 secret。因此这个 compose 文件按当前状态不能直接成功启动，必须要求用户替换 secret。最终 followups 应修正示例，或者用 `.env.example` 引导用户设置强 secret。

另一个部署边界：`Dockerfile` 只复制主 `ironforge`，而 compose 中注释的 runner service 使用 `ironforge runner` 子命令而不是独立 `ironforge-runner`，因此 runner 容器示例依赖主二进制内置 runner 模式。第 8 轮已记录 runner 自动注册鉴权不一致，这里也会影响 compose runner 示例。

### CI/CD 平台配置

本轮 `find .github -maxdepth 3 -type f` 未发现 GitHub Actions workflow 文件；也未看到 `.gitlab-ci.yml`。这意味着仓库虽然有丰富本地回归脚本，但没有看到远端 CI 编排文件把这些脚本串起来。

最终工程化文档应区分：

- “已有本地自动化脚本”；
- “缺少正式 hosted CI workflow”。

### 运维缺口与后续修复清单

| 优先级 | 问题 | 影响 | 建议 |
|--------|------|------|------|
| P0 | `deploy/docker-compose.yml` 示例 secret 为 `change-me-in-production`，而服务会拒绝该值 | 快速启动示例失败 | 改为 `.env.example` + 强随机 secret 占位说明 |
| P0 | 缺少 CI workflow 文件 | 本地脚本未纳入持续集成，回归容易漏跑 | 增加 GitHub Actions 或其他 CI 编排，调用 full-interface-regression 的分阶段能力 |
| P1 | Prometheus target 使用 `host.docker.internal:7878`，主服务默认是 8080 | 观测 compose 默认抓不到服务 | 同步端口或文档化外部 target 配置 |
| P1 | 未看到 DB 备份/恢复命令 | 生产数据恢复策略缺位 | 补 SQLite backup/restore 文档或 CLI；PostgreSQL 后补正式备份 |
| P1 | fresh DB migration smoke 未固定在 CI | 迁移表名/幂等性问题可能回归 | 加 `ironforge migrate` fresh DB + `.tables`/关键 API smoke |
| P1 | Docker 镜像只包含主服务二进制 | `ironforge-runner`/`ironforge-mcp` 不能从同镜像直接运行 | 明确单镜像范围，或构建多 binary 镜像 |
| P2 | `/metrics` 未初始化时会 panic | 特殊测试/嵌入路径可能崩溃 | 改为 503 或空 registry 响应 |
| P2 | README/CONTRIBUTING 仍有待创建 `scripts/e2e_test.sh` 的旧段落 | 文档与实际脚本体系不一致 | 清理旧 E2E 段落，统一到现有 scripts |

### 可进入最终文档的内容

可直接进入最终架构文档的测试/构建总览：

> IronForge 的验证体系由 Rust 单元测试、`rg-http` 集成测试、前端 `svelte-check`/Vite build、OpenAPI smoke、前后端 API contract check 和浏览器 console smoke 组成。`scripts/full-interface-regression.mjs` 是当前最完整的一键回归入口，支持按 backend/frontend/runtime 分阶段执行、跳过、超时、重试和报告落盘。

可直接进入最终部署文档的运行模型：

```text
Docker build
  -> node frontend build
  -> cargo release build --bin ironforge
  -> debian runtime with git/curl/sqlite/ssh client

Runtime data
  -> /data/repos
  -> /data/ironforge.db
  -> /data/logs/ironforge.log

HTTP
  -> /health
  -> /metrics
  -> /api/v1
  -> /git
  -> /v2

SSH
  -> 0.0.0.0:2222
```

第 9 轮结论中，compose secret、CI workflow 缺失、Prometheus target 端口、备份恢复策略应进入最终 followups。

---

## 第 10 轮：最终文档收敛

### 目标

把前置分析轮次的记录收敛为三份正式文档：

| 文档 | 文件 | 内容边界 |
|------|------|----------|
| 项目架构总览 | `ironforge-docs/architecture/project-architecture-2026-07.md` | 系统定位、顶层架构、运行入口、crate 边界、数据模型、HTTP/Git/SSH、安全、CI/Package/MCP、测试部署 |
| 前后端结构分布 | `ironforge-docs/architecture/frontend-backend-structure-2026-07.md` | 后端 crates/API/领域模块、前端 routes/API client/stores/components、页面与后端能力映射 |
| 架构差异与待办 | `ironforge-docs/architecture/architecture-followups-2026-07.md` | P0/P1/P2 followups、文档口径修正、建议执行顺序 |

### 收敛原则

- 不再按历史 Phase 逐项复述，而是按当前代码结构归类。
- 保留关键文件路径，便于后续从文档回到源码。
- 把“已有能力”和“实现缺口”分开写，避免最终架构文档把风险误写成已闭环能力。
- 高风险问题集中进入 followups 文档，主架构文档只保留必要警示。
- 前后端结构文档重点服务“新增功能应该放哪里、调用哪里、复用什么”。

### 已生成文档

#### `project-architecture-2026-07.md`

已覆盖：

- 系统定位与顶层架构图；
- 运行入口和二进制入口；
- Rust workspace crate 边界；
- 数据模型分组；
- HTTP 路由前缀和 API 分组；
- Git/SSH 协议链路；
- 安全与认证模型；
- CI/CD、Package Registry、MCP；
- 构建、部署、健康检查、metrics；
- 相对旧架构文档的口径修正。

#### `frontend-backend-structure-2026-07.md`

已覆盖：

- 前后端请求主链路；
- 后端 workspace crates 和 `rg-core` 模块；
- `rg-http` API 文件职责；
- SvelteKit 前端技术栈、目录和 API client；
- 全局页面、仓库页面和非页面客户端映射；
- store、WebSocket、Package Registry 前后端口径；
- 后端能力与前端覆盖矩阵；
- 新功能落点建议。

#### `architecture-followups-2026-07.md`

已覆盖：

- P0 安全/权限/部署问题；
- P1 高优先级增强；
- P2 中期整理；
- 旧文档口径修正；
- 建议执行顺序。

### 本轮修正的口径

| 旧/中间判断 | 最终修正 |
|-------------|----------|
| `/api-docs` PAT 支持待核验 | 第 9 轮发现 `openapi_docs_auth_tests.rs` 覆盖 JWT 和 PAT，最终只保留 cookie 不支持的体验边界 |
| Package Registry 支持 17 种完整协议 | 改为 17 种 type 枚举，专用协议/adapter 覆盖较小，其余 generic fallback |
| Artifact 管理完整 | 第 10 轮后已补 runner raw 上传、服务端文件保存、下载端点和 repo read 权限 |
| MCP 支持 SSE | 改为 stdio 可用，`--sse` 未实现 |
| Docker 镜像包含全部二进制 | 第 10 轮后 runtime 镜像已复制 `ironforge`、`ironforge-runner`、`ironforge-mcp` |

### 最终可交付状态

第 10 轮完成后，本轮架构重盘已形成：

```text
project-architecture-analysis-plan-2026-07.md
project-architecture-analysis-notes-2026-07.md
project-architecture-2026-07.md
frontend-backend-structure-2026-07.md
architecture-followups-2026-07.md
architecture-remediation-plan-2026-07.md
```

第 10 轮后的修复执行已完成首轮 P0/P1 收口：认证会话、仓库级权限、Runner/部署、CI token、Artifact 文件链路、MCP runtime、CSP、Markdown sanitizer 和前端 API client 拆分等均已回填到正式文档。后续继续推进时，应从 `architecture-followups-2026-07.md` 的 P2 长期方向开始，包括 PostgreSQL、MCP SSE、Package 专用协议补全和 gix 后续迁移。

---

## 后续：P0/P1 修复执行计划

### 目标

在第 10 轮最终文档收敛后，把 `architecture-followups-2026-07.md` 中的 P0/P1 缺口转成可拆分的修复计划，便于进入代码实现和 PR 切分。该执行计划已完成首轮修复并作为过程记录保留。

### 新增文档

| 文档 | 文件 | 内容边界 |
|------|------|----------|
| 架构修复执行计划 | `ironforge-docs/architecture-remediation-plan-2026-07.md` | 认证会话、仓库级权限、Runner/部署、CI workflow、P1 安全运维硬化、PR 切分、回归验证矩阵 |

### 修复波次

| 波次 | 范围 | 代表问题 |
|------|------|----------|
| Wave 1 | 认证会话正确性 | cookie-only `/users/me`、Admin cookie、MFA disable、SSO state |
| Wave 2 | 仓库级权限一致性 | Pipeline、Package、OCI、SSH Git 的 `can_read` / `can_write` |
| Wave 3 | Runner、部署和自动回归 | Runner 注册 token、Docker fail closed、compose secret、Prometheus target、CI workflow |
| Wave 4 | P1 安全和运维硬化 | LDAP TLS、trusted proxy、MCP runtime、metrics、backup/restore、artifact 文件链路、job log WS |

当前状态：Wave 1-4 已完成首轮实现和文档回填，P0/P1 强优先级项已清零。

### 与既有文档的关系

- `architecture-followups-2026-07.md` 现在保持已修复清单、P2 长期方向和旧口径修正。
- `architecture-remediation-plan-2026-07.md` 负责保留执行顺序、涉及文件、验收测试和 PR 切分记录。
- 后续新增能力或风险应继续同步更新架构总览、前后端结构映射和 followups。

---
