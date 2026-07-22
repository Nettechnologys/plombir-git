# IronForge 🔨

> **铁匠铺** — 一个用 Rust 从零实现的轻量级 Git 托管平台

[![Rust](https://img.shields.io/badge/rust-1.95%2B-orange)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

IronForge 对标 [Gitea](https://gitea.com/) / [Forgejo](https://forgejo.org/)，目标是用纯 Rust 实现一个内存占用极低（<50MB）、单二进制部署的全功能 Git 托管平台，支持仓库管理、Issue、Pull Request、Wiki 和 CI/CD。

---

## 功能状态

| 功能 | 状态 | 说明 |
|------|------|------|
| `git clone` over HTTPS | ✅ 完成 | Git Smart Protocol V1 |
| `git push` over HTTPS  | ✅ 完成 | report-status + sideband-64k |
| `git clone` over SSH   | ✅ 完成 | russh 0.51，公钥/密码认证 |
| `git push` over SSH    | ✅ 完成 | sideband 正确封装 |
| 用户注册 / 登录         | ✅ 完成 | argon2 密码哈希 + JWT HS256 |
| 仓库管理               | ✅ 完成 | SeaORM + SQLite，REST API |
| SSH 公钥认证（查 DB）   | ✅ 完成 | russh auth_publickey 查 ssh_keys 表 |
| Issue CRUD + 评论       | ✅ 完成 | 标签 / 里程碑 / 评论 |
| Pull Request + Diff     | ✅ 完成 | 三种合并策略（merge/squash/rebase） |
| Wiki CRUD               | ✅ 完成 | 页面创建/编辑/删除/列表 |
| Git LFS                 | ✅ 完成 | batch API + 对象上传/下载 + zstd 压缩 |
| Webhook                 | ✅ 完成 | 注册/触发/投递/HMAC-SHA256 签名 |
| CI/CD Pipeline          | ✅ 完成 | .ironforge-ci.yml 解析 + 后台执行 |
| Git 协议鉴权            | ✅ 完成 | HTTP Bearer Token + can_read/can_write |
| 代码审查（PR Review）   | ✅ 完成 | approve / request_changes / comment / dismiss + inline comments |
| 分支保护               | ✅ 完成 | require PR + require approval + required status checks |
| 协作者管理             | ✅ 完成 | read / write / admin 权限 |
| 文件浏览 API           | ✅ 完成 | tree / blob / log / branches / tags |
| Web UI (SvelteKit)     | ✅ 完成 | 登录/注册/仓库/Issue/PR/Wiki/CI/代码审查/组织/通知 |
| Docker Runner          | ✅ 完成 | CI Job 容器化执行（image 字段 → docker run） |
| 组织/团队系统          | ✅ 完成 | organization + team + 成员管理 + 权限 |
| 通知系统               | ✅ 完成 | 创建/列表/已读/批量已读 + 前端页面 |
| API Rate Limiting      | ✅ 完成 | Token Bucket 中间件（IP 限流 + 可配置窗口） |
| WebSocket 实时通知     | ✅ 完成 | broadcast channel + JWT 认证 + 自动重连 |
| 邮件通知               | ✅ 完成 | SMTP（lettre）+ HTML 模板 + 可配置 |
| 组织仓库               | ✅ 完成 | org_id 关联 + 组织拥有仓库创建 |
| 权限鉴权完善           | ✅ 完成 | org member / team permission → can_read/can_write |
| TLS/HTTPS 支持         | ✅ 完成 | axum-server + rustls，CLI --tls-cert/--tls-key |
| TOML 配置文件          | ✅ 完成 | CLI args > config file > defaults，ironforge.example.toml |
| 日志轮转               | ✅ 完成 | tracing-appender RollingFileAppender (DAILY + non-blocking) |
| API 统一分页           | ✅ 完成 | PaginationParams + PaginatedResponse\<T\>，5 个 list API |
| Git Smart Protocol V2  | ✅ 完成 | HTTP V2 支持（Git-Protocol: version=2 header + ls-refs/fetch） |
| GPG 签名验证           | ✅ 完成 | GET /repos/:owner/:name/commits/:sha/signature |

---

## 快速开始

### 环境要求

- Rust 1.95+（推荐 stable）
- git（系统命令，用于 pack-objects / index-pack / update-ref / diff）
- macOS 或 Linux

### 编译

```bash
git clone <this-repo>
cd ironforge
cargo build --release
```

二进制产物位于 `target/release/ironforge`。

### 生成 SSH 主机密钥

首次运行需要一个 SSH 主机密钥：

```bash
ssh-keygen -t ed25519 -f /tmp/ironforge_host_key -N ""
```

### 启动服务器

```bash
# 创建仓库根目录
mkdir -p /tmp/ironforge/repos

# 启动（HTTP :8080 + SSH :2222 + SQLite）
./target/release/ironforge serve \
  --repo-root /tmp/ironforge/repos \
  --http-addr 0.0.0.0:8080 \
  --ssh-addr  0.0.0.0:2222 \
  --host-key  /tmp/ironforge_host_key \
  --db-url    sqlite:///tmp/ironforge/ironforge.db?mode=rwc \
  --jwt-secret my-secret-key
```

参数说明：

| 参数 | 说明 | 默认值 |
|------|------|--------|
| `--repo-root` | 裸仓库存储根目录 | 必填 |
| `--http-addr` | HTTP 监听地址 | `0.0.0.0:8080` |
| `--ssh-addr` | SSH 监听地址 | `0.0.0.0:2222` |
| `--host-key` | SSH 主机密钥路径 | 必填 |
| `--db-url` | SQLite 数据库 URL | `sqlite:///tmp/ironforge/ironforge.db?mode=rwc` |
| `--jwt-secret` | JWT 签名密钥 | 必填 |
| `--config` | TOML 配置文件路径 | 无 |
| `--tls-cert` | TLS 证书 PEM 路径 | 无（启用 HTTPS） |
| `--tls-key` | TLS 私钥 PEM 路径 | 无（启用 HTTPS） |
| `--log-file` | 日志文件路径 | 无（输出到 stderr） |
| `--log-max-files` | 最大日志文件数 | 10 |
| `--docker` | 启用 Docker CI runner | false |
| `--rate-limit-max` | 限流最大请求数 | 100 |
| `--rate-limit-window` | 限流窗口（秒） | 60 |
| `--smtp-host` | SMTP 服务器地址 | 无（禁用邮件） |
| `--smtp-port` | SMTP 端口 | 587 |
| `--smtp-user` | SMTP 用户名 | 无 |
| `--smtp-pass` | SMTP 密码 | 无 |
| `--smtp-from` | 发件人地址 | 无 |

启动时自动运行数据库迁移，无需手动建表。

日志级别通过环境变量控制：

```bash
RUST_LOG=debug ./target/release/ironforge serve ...
```

### 创建测试仓库

```bash
./target/release/ironforge create-repo testuser testrepo \
  --repo-root /tmp/ironforge/repos
# → 创建 /tmp/ironforge/repos/testuser/testrepo.git
```

---

## REST API

所有 API 在 `/api/v1/` 下，需要认证的接口在 Header 中传 `Authorization: Bearer <token>`。

### 用户

```bash
# 注册
curl -X POST http://localhost:8080/api/v1/users/register \
  -H "Content-Type: application/json" \
  -d '{"username":"testuser","email":"test@example.com","password":"secret123"}'

# 登录（返回 JWT token）
curl -X POST http://localhost:8080/api/v1/users/login \
  -H "Content-Type: application/json" \
  -d '{"username":"testuser","password":"secret123"}'

# 查看当前用户
curl http://localhost:8080/api/v1/users/me \
  -H "Authorization: Bearer <token>"
```

### 仓库

```bash
# 创建仓库
curl -X POST http://localhost:8080/api/v1/repos \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"name":"myrepo","description":"test repo"}'

# 列出用户仓库
curl http://localhost:8080/api/v1/repos/testuser

# 查看仓库详情
curl http://localhost:8080/api/v1/repos/testuser/myrepo
```

### Issue

```bash
# 创建 Issue
curl -X POST http://localhost:8080/api/v1/repos/testuser/myrepo/issues \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"title":"Bug report","body":"Something is wrong","labels":"bug","milestone_id":1}'

# 列出 Issue（?state=open/closed/all）
curl "http://localhost:8080/api/v1/repos/testuser/myrepo/issues?state=open" \
  -H "Authorization: Bearer <token>"

# 查看 Issue 详情
curl http://localhost:8080/api/v1/repos/testuser/myrepo/issues/1 \
  -H "Authorization: Bearer <token>"

# 更新 Issue
curl -X PATCH http://localhost:8080/api/v1/repos/testuser/myrepo/issues/1 \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"title":"Updated title","state":"closed"}'

# 添加评论
curl -X POST http://localhost:8080/api/v1/repos/testuser/myrepo/issues/1/comments \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"body":"This is a comment"}'

# 列出评论
curl http://localhost:8080/api/v1/repos/testuser/myrepo/issues/1/comments \
  -H "Authorization: Bearer <token>"
```

### Pull Request

```bash
# 创建 PR
curl -X POST http://localhost:8080/api/v1/repos/testuser/myrepo/pulls \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"title":"Add feature","body":"Description","head_branch":"feature","base_branch":"main"}'

# 列出 PR（?state=open/closed/merged/all）
curl "http://localhost:8080/api/v1/repos/testuser/myrepo/pulls?state=open" \
  -H "Authorization: Bearer <token>"

# 查看 PR 详情
curl http://localhost:8080/api/v1/repos/testuser/myrepo/pulls/1 \
  -H "Authorization: Bearer <token>"

# 获取 Diff
curl http://localhost:8080/api/v1/repos/testuser/myrepo/pulls/1/diff \
  -H "Authorization: Bearer <token>"

# 合并 PR（strategy: merge / squash / rebase）
curl -X POST http://localhost:8080/api/v1/repos/testuser/myrepo/pulls/1/merge \
  -H "Authorization: Bearer <token>" \
  -H "Content-Type: application/json" \
  -d '{"strategy":"merge"}'
```

---

### API 文档鉴权（建议默认开启）

`/api-docs/*` 默认受保护，需要有效 JWT 或 PAT 才能访问。

```bash
# 登录拿 token
TOKEN="$(curl -s http://localhost:8080/api/v1/users/login \
  -H 'Content-Type: application/json' \
  -d '{"login":"testuser","password":"secret123"}' \
  | jq -r '.token')"

# OpenAPI JSON
curl -H "Authorization: Bearer ${TOKEN}" http://localhost:8080/api-docs/openapi.json

# Swagger UI
curl -H "Authorization: Bearer ${TOKEN}" http://localhost:8080/api-docs/
```

配合 OpenAPI 冒烟脚本可自动补 token 并校验鉴权行为：

```bash
OPENAPI_REQUIRE_AUTH=1 BACKEND_URL=http://127.0.0.1:8080 node scripts/openapi-interface-smoke.mjs
```

## Git 操作

### SSH

```bash
GIT_SSH_COMMAND="ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null" \
  git clone ssh://git@localhost:2222/testuser/testrepo /tmp/myrepo

cd /tmp/myrepo
echo "hello" > test.txt
git add test.txt && git commit -m "test"

GIT_SSH_COMMAND="ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null" \
  git push origin main
```

### HTTP

```bash
git clone http://localhost:8080/git/testuser/testrepo /tmp/myrepo-http
cd /tmp/myrepo-http
# ... 修改文件后
git push origin main
```

---

## 项目结构

```
ironforge/
├── Cargo.toml              # Workspace 根，统一依赖版本
├── ARCHITECTURE.md         # 历史架构方案文档
├── AGENT.md                # AI 助手统一入口（所有 AI 工具必读）
├── CLAUDE.md               # 完整 AI 协作上下文（Codex / Claude Code / WorkBuddy）
├── CONTRIBUTING.md         # 开发指南
├── ironforge-docs/
│   ├── README.md                             # 文档索引（单一事实来源）
│   ├── architecture/                         # 架构总览/前后端结构/后续/followups/分模块/DB多后端
│   ├── analysis/                             # 改进与优化整合报告
│   ├── comparison/                           # Gitea 对比 + 差距清单
│   ├── ci/                                   # CI Runner 架构
│   ├── testing/                              # 功能测试 + 审计
│   └── archive/                              # 过程文档与过时报告（追溯）
├── docs/
│   ├── p0-prd.md           # P0 功能 PRD
│   ├── p0-system-design.md # P0 系统设计 + 任务分解
│   └── git-protocol.md     # Git 协议实现细节与踩坑记录
├── crates/
│   ├── rg-cli/     # 主二进制入口（bin = "ironforge"）
│   ├── rg-core/    # 核心业务逻辑（用户/仓库/Issue/PR）
│   ├── rg-git/     # Git 协议层（pkt-line、upload-pack、receive-pack、sideband）
│   ├── rg-ssh/     # SSH 服务端（russh 0.51）
│   ├── rg-http/    # HTTP 服务端 + REST API（Axum 0.8）
│   ├── rg-db/      # 数据库层（SeaORM 1.1 + SQLite）
│   ├── rg-ci/      # CI/CD 引擎（YAML 解析 + Pipeline 执行器）
│   └── rg-runner/  # Runner Agent 独立二进制（bin = "ironforge-runner"）
└── web/            # SvelteKit 前端（独立 SPA）
```

### 各 crate 职责

| Crate | 职责 | 状态 |
|-------|------|------|
| `rg-cli` | CLI 入口，`serve` / `create-repo` / `migrate` / `runner` 命令 | ✅ 完成 |
| `rg-git` | Git Smart Protocol V1/V2：pkt-line 编解码、upload-pack、receive-pack、sideband-64k | ✅ 完成 |
| `rg-ssh` | russh SSH 服务端，exec_request 路由到 rg-git，公钥/密码认证查 DB | ✅ 完成 |
| `rg-http` | Axum HTTP 服务端，Git Smart HTTP 端点 + REST API + WebSocket | ✅ 完成 |
| `rg-core` | 业务逻辑：用户认证（argon2+JWT）、仓库、Issue、Pull Request、Wiki、LFS、Webhook、Review、Branch Protection、Collaborator、Org、Notification | ✅ Phase 1-6 |
| `rg-ci` | CI/CD 引擎：YAML 解析 + Pipeline 执行器（Stage/Job 串行执行）+ Docker Runner | ✅ Phase 5 |
| `rg-db` | SeaORM 实体 + 迁移（users/repos/ssh_keys/issues/comments/pulls/milestones/wiki/lfs/webhooks/pipelines） | ✅ Phase 1-5 |
| `rg-runner` | Runner Agent 独立二进制：注册、心跳、轮询 Job、执行、上传日志/Artifact | ✅ Phase 17 |

---

## 数据模型

```
┌──────────────┐     ┌──────────────────┐     ┌──────────────────┐
│    users      │     │   repositories   │     │    ssh_keys      │
├──────────────┤     ├──────────────────┤     ├──────────────────┤
│ id (PK)      │←────│ owner_id (FK)    │     │ id (PK)          │
│ username     │     │ id (PK)          │     │ user_id (FK)     │
│ email        │     │ name             │     │ key_data         │
│ password_hash│     │ description      │     │ fingerprint      │
│ created_at   │     │ is_private       │     │ title            │
└──────────────┘     │ created_at       │     └──────────────────┘
                     └────────┬─────────┘
                              │
              ┌───────────────┼───────────────┐
              │               │               │
    ┌─────────┴──────┐ ┌─────┴──────────┐ ┌──┴───────────────┐
    │    issues      │ │ pull_requests  │ │   milestones     │
    ├────────────────┤ ├────────────────┤ ├──────────────────┤
    │ id (PK)        │ │ id (PK)        │ │ id (PK)          │
    │ repo_id (FK)   │ │ repo_id (FK)   │ │ repo_id (FK)     │
    │ number         │ │ number         │ │ title            │
    │ title          │ │ title          │ │ description      │
    │ body           │ │ body           │ │ due_date         │
    │ state          │ │ state          │ │ state            │
    │ labels         │ │ head_branch    │ └──────────────────┘
    │ author_id (FK) │ │ base_branch    │
    │ milestone_id   │ │ head_sha       │
    │ created_at     │ │ author_id (FK) │
    │ updated_at     │ │ merge_strategy │
    └───────┬────────┘ │ merge_sha      │
            │          │ milestone_id   │
    ┌───────┴──────────┐│ created_at     │
    │ issue_comments   ││ updated_at     │
    ├──────────────────┤└────────────────┘
    │ id (PK)          │
    │ issue_id (FK)    │   ┌──────────────┐   ┌──────────────┐   ┌──────────────────┐
    │ author_id (FK)   │   │ wiki_pages   │   │ lfs_objects  │   │    webhooks      │
    │ body             │   ├──────────────┤   ├──────────────┤   ├──────────────────┤
    │ created_at       │   │ id (PK)      │   │ id (PK)      │   │ id (PK)          │
    │ updated_at       │   │ repo_id (FK) │   │ repo_id (FK) │   │ repo_id (FK)     │
    └──────────────────┘   │ title        │   │ oid (SHA256) │   │ url              │
                           │ content      │   │ size         │   │ content_type     │
                           │ author_id    │   │ uploaded     │   │ secret           │
                           │ sha          │   │ created_at   │   │ active           │
                           │ created_at   │   └──────────────┘   │ events           │
                           │ updated_at   │                      │ created_at       │
                           └──────────────┘                      └────────┬─────────┘
                                                                         │
                                                                ┌────────┴─────────┐
                                                                │ webhook_deliveries│
                                                                ├──────────────────┤
                                                                │ id (PK)          │
                                                                │ webhook_id (FK)  │
                                                                │ event            │
                                                                │ delivery_id      │
                                                                │ response_status  │
                                                                │ request_payload  │
                                                                │ response_body    │
                                                                │ duration_ms      │
                                                                │ created_at       │
                                                                └──────────────────┘
```

---

## 开发

### 开发构建

```bash
cargo build          # debug 构建
cargo build --release  # release 构建
```

### 回归测试入口

日常回归优先使用现有自动化脚本，而不是维护一次性的 shell 片段：

```bash
# 全量回归：后端测试、前端静态检查/构建、运行态 smoke
node scripts/full-interface-regression.mjs

# 后端 OpenAPI 冒烟
BACKEND_URL=http://127.0.0.1:8080 node scripts/openapi-interface-smoke.mjs

# 前端页面 console/network 冒烟
BASE=http://127.0.0.1:5173 node scripts/console-smoke.mjs

# 前端 API client 与 OpenAPI 参数对齐
BACKEND_URL=http://127.0.0.1:8080 node scripts/api-client-contract-check.mjs
```

Git 协议问题需要手动定位时，可以单独执行 SSH/HTTP clone/push 调试；命令模板见
`AGENTS.md` 的 Git 协议测试约定。

### 日志调试

```bash
# 查看服务器日志
tail -f /tmp/ironforge.log

# 开启 git 协议追踪（客户端侧）
GIT_TRACE_PACKET=1 GIT_TRACE=1 git push origin main 2>&1
```

---

## 技术选型

| 层级 | 选型 | 版本 |
|------|------|------|
| 异步运行时 | tokio | 1.x |
| HTTP 框架 | axum + axum-server | 0.8 / 0.7 |
| SSH 服务端 | russh | 0.51 |
| Git 操作 | gix + git CLI gateway | 0.84 |
| ORM | SeaORM | 1.1 |
| 认证 | argon2 + JWT | 0.5 |
| TLS | rustls + tokio-rustls | 0.23 / 0.26 |
| 序列化 | serde + serde_json + toml | 1.x |
| 错误处理 | anyhow + thiserror | 1.x / 2.x |
| 日志 | tracing + tracing-subscriber + tracing-appender | 0.1 |
| CLI | clap | 4.x |
| 前端 | SvelteKit 5 + adapter-static | SPA mode |
| 前端 i18n | Svelte 5 reactive store + localStorage | 中文 + 英文 |
| 代码覆盖率 | cargo-llvm-cov | HTML/LCOV/JSON 输出 |

> **关于 i18n**：前端完整国际化（199 个翻译 key），后端统一英文无需 i18n。
> **关于 gix**：当前 workspace 使用 gix 0.84。Git 对象操作采用 gix + `GitCommandGateway` 混合模式；pack、rebase、archive、部分 unified diff/GPG 等能力仍依赖 Git CLI 或等待 gix 上游能力成熟。

---

## Roadmap

### ✅ Phase 1 — Git 协议（已完成）

- HTTP + SSH git clone/push
- pkt-line / sideband-64k / Smart Protocol V1
- russh SSH 服务端

### ✅ Phase 2 — 用户系统（已完成）

- SeaORM 实体 + 迁移（users / repositories / ssh_keys / access_tokens）
- argon2 密码哈希 + JWT HS256
- SSH 公钥/密码认证查 DB
- REST API：/api/v1/users/* + /api/v1/repos/*

### ✅ Phase 3 — Issue + Pull Request（已完成）

- Issue CRUD + 标签 + 里程碑 + 评论
- Pull Request 创建 + diff 计算（git CLI）+ 三种合并策略
- REST API：/api/v1/repos/:owner/:name/issues/* + /api/v1/repos/:owner/:name/pulls/*

### ✅ Phase 4 — Wiki + LFS + Webhook（已完成）

- Wiki 页面 CRUD（DB 存储，支持标题/内容/作者/提交信息）
- Git LFS batch API（上传/下载，磁盘分片存储 `<oid_prefix>/<oid>`）
- Webhook 注册/触发/投递（HMAC-SHA256 签名，异步后台投递，投递记录）
- REST API：/api/v1/repos/:owner/:name/wiki/*、/lfs/*、/hooks/*

### ✅ Phase 5 — CI/CD + 权限鉴权（已完成）

- CI/CD Pipeline 引擎（`.ironforge-ci.yml` 解析 + Stage/Job 执行器）
- Pipeline REST API（list / trigger / retry / cancel / job detail）
- Push 自动触发 CI（receive-pack 后台触发）
- Push 自动触发 Webhook（push 事件 payload）
- HTTP Git 协议权限鉴权（Bearer Token → can_read/can_write）
- DB：pipelines / pipeline_stages / pipeline_jobs 实体 + 迁移

### ✅ Phase 6 — Web UI + 高级功能（已完成）

- CI/CD Pipeline 引擎（`.ironforge-ci.yml`）
- Web UI（SvelteKit，独立前端）
- 代码审查 / 分支保护规则

### ✅ Phase 10 — TLS + 配置文件 + 日志轮转 + API 分页 + GPG 签名（已完成）

- TLS/HTTPS 支持（axum-server + rustls）
- TOML 配置文件（优先级 CLI args > config file > defaults）
- 日志轮转（tracing-appender RollingFileAppender）
- API 统一分页（PaginationParams + PaginatedResponse\<T\>）
- GPG 签名验证（git log --format=%G? + gpgsig header）

### ✅ Phase 11 — 前端国际化（已完成，2026-04-27）

- Svelte 5 locale store + localStorage 持久化
- 自动检测浏览器语言（zh → 中文，en → 英文）
- 199 个翻译 key（中/英双语）
- 后端统一英文，无 i18n 需求

### ✅ Phase 12 — 代码覆盖率集成（已完成，2026-04-27）

- cargo-llvm-cov 覆盖率工具
- HTML 报告（target/llvm-cov/html）
- LCOV 格式（Codecov/Coveralls 集成）
- JSON 格式（CI 集成）

---

## 架构文档

当前架构事实请优先阅读：

- [项目架构总览](ironforge-docs/architecture/project-architecture-2026-07.md)
- [前后端结构分布](ironforge-docs/architecture/frontend-backend-structure-2026-07.md)
- [架构差异与后续待办](ironforge-docs/architecture/architecture-followups-2026-07.md)
- [文档索引](ironforge-docs/README.md)（完整导航）
- [贡献者快速上手](ironforge-docs/contributor-quickstart.md)（5 分钟跑起来）

[ARCHITECTURE.md](ARCHITECTURE.md) 保留为历史设计文档，可用于了解早期技术选型和设计意图；当前模块边界、部署状态和风险口径以 2026-07 架构文档为准。

---

## License

MIT

## 自动化联调（前后端）

在本机运行前后端联调时，可以用一键脚本检查关键通道是否打通：

```bash
# 默认：后端 127.0.0.1:8080，前端 127.0.0.1:5173
node scripts/frontend-backend-smoke.mjs

# 如果你的端口不同：
BACKEND_URL=http://127.0.0.1:8080 FRONTEND_URL=http://127.0.0.1:3000 node scripts/frontend-backend-smoke.mjs

# 前后端解耦部署时，可直接指定 API 源：
API_BASE=https://api.example.com/api/v1 BACKEND_URL=https://api.example.com FRONTEND_URL=http://127.0.0.1:5173 node scripts/frontend-backend-smoke.mjs
```

如果你用 Vite 本地调试前端，建议开启 `VITE_API_BASE`（不在同源时必须）：

```bash
cd web
VITE_API_BASE=http://127.0.0.1:8080/api/v1 npm run dev
```

跨域前后端部署时，后端也需要允许浏览器 origin，并让 CSP `connect-src`
覆盖 API 与 WebSocket：

```bash
IRONFORGE_CORS_ORIGINS=http://127.0.0.1:5173
# 如有额外 API/WS origin，可补：
# IRONFORGE_CSP_CONNECT_SRC=https://api.example.com,wss://api.example.com
```

### 全量接口自动化回归（建议日常执行）

新增自动化脚本（默认路径）：

- `scripts/openapi-interface-smoke.mjs`：自动读取 `/api-docs/openapi.json` 并对全部 OpenAPI 接口做可用性请求，默认要求文档鉴权（未鉴权必须为 401；通过后继续复测）。
- `scripts/console-smoke.mjs`：自动发现 `web/src/routes/**/+page.svelte` 并用浏览器检查全部前端页面的 console/network 运行态错误。
- `scripts/full-interface-regression.mjs`：一次性执行后端测试、前端检查、前端构建，并在检测到服务可达时执行运行态联调；默认会探测前端 `4173` 和 `5173`。
- `scripts/api-client-contract-check.mjs`：对比前端 API client 与 OpenAPI 的路由声明、方法和参数签名是否一致。

```bash
# 一键全量回归（先启动后端 8080；前端可用 4173 preview 或 5173 dev）
node scripts/full-interface-regression.mjs

# 受限环境只跑前端与接口可达性（跳过后端集成测试）
SKIP_BACKEND_TESTS=1 node scripts/full-interface-regression.mjs

# 受限环境只跑后端可达性+前端静态（跳过 web check/build）
SKIP_FRONTEND_STATIC=1 node scripts/full-interface-regression.mjs

# 受限环境只跑静态和参数对齐，不跑运行态冒烟（openapi/frontend-backend/console）
SKIP_RUNTIME_SMOKES=1 node scripts/full-interface-regression.mjs

# 只执行某一类回归（backend / frontend / runtime / all，默认 all）
FULL_REGRESSION_ONLY=backend node scripts/full-interface-regression.mjs
FULL_REGRESSION_ONLY=frontend node scripts/full-interface-regression.mjs
FULL_REGRESSION_ONLY=runtime node scripts/full-interface-regression.mjs

# 设置单命令超时（毫秒），超时将直接失败并退出（默认不设置即不限制）
REGRESSION_TIMEOUT_MS=120000 FULL_REGRESSION_ONLY=backend node scripts/full-interface-regression.mjs

# 阶段级超时（优先级高于 REGRESSION_TIMEOUT_MS）
# BACKEND_TEST_TIMEOUT_MS / FRONTEND_STATIC_TIMEOUT_MS / RUNTIME_TIMEOUT_MS
# 分别控制：后端测试 / 前端静态 check+build / 运行态冒烟
BACKEND_TEST_TIMEOUT_MS=120000 FRONTEND_STATIC_TIMEOUT_MS=120000 RUNTIME_TIMEOUT_MS=60000 node scripts/full-interface-regression.mjs

# 脚本会输出每次执行的阶段超时配置，便于确认实际生效参数

# 失败重试（按阶段可配置）
# 默认不重试；全局重试和分阶段重试（后缀 _RETRIES 为优先）
REGRESSION_RETRIES=1 REGRESSION_RETRY_DELAY_MS=3000 \
BACKEND_TEST_RETRIES=2 FRONTEND_STATIC_RETRIES=1 RUNTIME_RETRIES=1 \
node scripts/full-interface-regression.mjs

# 指数退避（可选）
# 基础重试间隔=REGRESSION_RETRY_DELAY_MS
# 退避系数=REGRESSION_RETRY_BACKOFF_MS（>=1，1 表示固定间隔）
# 间隔上限=REGRESSION_RETRY_MAX_DELAY_MS
REGRESSION_RETRIES=3 REGRESSION_RETRY_DELAY_MS=1000 REGRESSION_RETRY_BACKOFF_MS=2 REGRESSION_RETRY_MAX_DELAY_MS=8000 node scripts/full-interface-regression.mjs

# 步骤级重试覆盖（优先级高于阶段/全局重试）
# 场景：只给前端 build 做 2 次重试，后端测试保持不重试
BACKEND_TEST_RETRIES=0 FRONTEND_BUILD_RETRIES=2 \
node scripts/full-interface-regression.mjs

# 步骤级超时覆盖（优先级高于阶段/全局超时）
# 场景：给前端 build 放宽到 4 分钟，openapi 冒烟缩短到 15 秒
FRONTEND_BUILD_TIMEOUT_MS=240000 OPENAPI_SMOKE_TIMEOUT_MS=15000 node scripts/full-interface-regression.mjs

# 步骤级退避覆盖（优先级高于全局退避）
# 场景：只给前端 build 使用更长退避，console-smoke 使用更短退避
FRONTEND_BUILD_RETRY_BACKOFF_MS=2.5 FRONTEND_BUILD_RETRY_MAX_DELAY_MS=120000 CONSOLE_SMOKE_RETRY_BACKOFF_MS=1.2 CONSOLE_SMOKE_RETRY_MAX_DELAY_MS=5000 node scripts/full-interface-regression.mjs

# 生成回归执行报告（JSON）
# 包含每步状态/耗时/重试明细，适合 CI 历史追踪与告警系统落盘
REGRESSION_REPORT_FILE=/tmp/full-regression-report.json node scripts/full-interface-regression.mjs

# 生成回归执行报告（Markdown）
REGRESSION_REPORT_FORMAT=md REGRESSION_REPORT_FILE=/tmp/full-regression-report.md node scripts/full-interface-regression.mjs

# 使用 CLI 参数设置格式（可与 env 并用）
REGRESSION_REPORT_FILE=/tmp/full-regression-report.md node scripts/full-interface-regression.mjs --report-format md

# 仅后端 OpenAPI 全量冒烟（后端可达即可）
BACKEND_URL=http://127.0.0.1:8080 node scripts/openapi-interface-smoke.mjs

# 仅前端页面运行态冒烟（默认自动发现全部 SvelteKit 页面）
BASE=http://127.0.0.1:5173 node scripts/console-smoke.mjs

# 仅做前后端接口参数对齐（client vs OpenAPI）
BACKEND_URL=http://127.0.0.1:8080 node scripts/api-client-contract-check.mjs

# 离线对齐：先把 OpenAPI 规范导出到文件后复用本地检查（适用于 CI/沙箱）
OPENAPI_SPEC_FILE=/tmp/openapi.json BACKEND_URL=http://127.0.0.1:8080 node scripts/api-client-contract-check.mjs

# 对齐检查只上报告警（CI 中不阻断）
CLIENT_CONTRACT_STRICT=0 BACKEND_URL=http://127.0.0.1:8080 node scripts/api-client-contract-check.mjs

# 查看自动发现到的前端页面列表
node scripts/console-smoke.mjs --list-routes
```
