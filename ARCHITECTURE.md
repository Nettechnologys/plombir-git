# IronForge（铁匠铺）— 用 Rust 从零造一个 Git 托管平台

> 项目代号：**IronForge**（已定名，formerly RustGit）
> 目标：对标 Gitea/Forgejo 的全功能 Git 托管平台，用 Rust 实现极致轻量与高性能

---

## 一、项目愿景

- **极致轻量**：内存占用 < 50MB（对比 Gitea ~200MB）
- **单二进制部署**：一个文件跑起来，Docker 镜像 < 20MB
- **全功能**：仓库管理、用户/组织、Issue、Pull Request、Wiki、CI/CD
- **跨平台**：macOS + Linux（Docker），后续可扩展 ARM 嵌入式

---

## 二、整体架构

```
┌─────────────────────────────────────────────────────────┐
│                      客户端层                             │
│   git CLI (SSH/HTTPS)  ·  Web 浏览器  ·  REST API       │
└──────────┬──────────────────────┬───────────────────────┘
           │ SSH (russh)          │ HTTPS (Axum)
           ▼                      ▼
┌─────────────────────────────────────────────────────────┐
│                   协议适配层 (Protocol)                    │
│  ┌──────────────┐  ┌──────────────┐  ┌───────────────┐  │
│  │ SSH Handler  │  │ HTTP Handler │  │ REST API      │  │
│  │ upload-pack  │  │ info/refs    │  │ /api/v1/...   │  │
│  │ receive-pack │  │ upload-pack  │  │               │  │
│  └──────┬───────┘  └──────┬───────┘  └──────┬────────┘  │
└─────────┼─────────────────┼─────────────────┼───────────┘
          │                 │                 │
          ▼                 ▼                 ▼
┌─────────────────────────────────────────────────────────┐
│                    核心业务层 (Core)                       │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌────────────┐  │
│  │ Repository│ │  User   │ │  Issue   │ │PullRequest │  │
│  │  Manager  │ │  Auth   │ │ Tracker  │ │   Engine   │  │
│  └──────────┘ └──────────┘ └──────────┘ └────────────┘  │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐                 │
│  │  Wiki    │ │  LFS     │ │  Hook    │                 │
│  │  Engine  │ │ Storage  │ │  System  │                 │
│  └──────────┘ └──────────┘ └──────────┘                 │
└─────────────────┬───────────────────────┬───────────────┘
                  │                       │
        ┌─────────┴────────┐    ┌────────┴────────┐
        ▼                  ▼    ▼                 ▼
┌──────────────┐  ┌──────────────┐  ┌──────────────────────┐
│  Git 数据层   │  │  持久化层    │  │    CI/CD 引擎         │
│ (gix/gitoxide)│  │  (SeaORM)   │  │  (Pipeline Runner)   │
│  对象存储      │  │   SQLite    │  │  Job调度/日志/产物   │
│  引用管理      │  │             │  │                      │
│  Pack 文件     │  │             │  │                      │
└──────────────┘  └──────────────┘  └──────────────────────┘
```

---

## 三、技术选型

### 3.1 核心框架

| 层级 | 选型 | 版本 | 理由 |
|------|------|------|------|
| **异步运行时** | tokio | 1.x | Rust 异步生态事实标准 |
| **HTTP 框架** | axum | 0.8+ | tokio 官方出品，生态好，性能优秀 |
| **SSH 服务端** | russh | 0.51+ | 纯 Rust SSH2 实现，支持服务端 |
| **Git 操作** | gix (gitoxide) | 0.83+ | 纯 Rust Git 实现，零 C 依赖（当前迁移进度 ~70%，12 处 git CLI fallback 待替换） |
| **ORM** | SeaORM | 1.x | 异步原生，迁移工具成熟，API 友好 |
| **数据库** | SQLite（默认） | — | 轻量起步，内置 WAL 模式 |
| **模板引擎** | Askama | 2.x | 编译时检查，性能极高 |
| **序列化** | serde + serde_json | 1.x | Rust 事实标准 |
| **配置** | config | 0.14+ | 支持 TOML/YAML/ENV 多格式 |
| **日志** | tracing + tracing-subscriber | 0.1 | 结构化日志， tokio 官方推荐 |
| **认证** | argon2 + jsonwebtoken | — | 密码哈希 + JWT Token |
| **前端** | SvelteKit (独立 SPA) | 5.x | 轻量、编译体积小、开发体验好 |

### 3.2 技术选型决策分析

#### ORM 对比

| 维度 | SeaORM | SQLx | Diesel |
|------|--------|------|--------|
| 异步支持 | ✅ 原生 | ✅ 原生 | ⚠️ 需 async-graphql |
| 动态查询 | ✅ 强 | ✅ 强 | ❌ 编译时强类型 |
| 迁移工具 | ✅ 内置 | ❌ 需 sqlx-cli | ✅ 内置 |
| 学习曲线 | 中等 | 低 | 高 |
| 适合场景 | **业务逻辑复杂** | 简单查询 | 类型安全优先 |

**选择 SeaORM**：业务层有 User/Repo/Issue/PR/Wiki 等复杂关联，动态查询需求多，SeaORM 的 Builder 模式更灵活。

#### Git 库对比

| 维度 | gix (gitoxide) | git2 (libgit2) |
|------|----------------|----------------|
| 纯 Rust | ✅ ~70%（核心对象操作已完成，部分协议操作使用 git CLI fallback） | ❌ C 依赖 |
| 编译速度 | 快 | 慢（需链接 C） |
| 服务端协议 | ❌ 尚未支持 | ❌ 不支持 |
| 对象操作 | ✅ 成熟 | ✅ 非常成熟 |
| 活跃度 | 极高（15k+ commits） | 中等 |

**选择 gix**：虽然服务端协议需自行实现，但纯 Rust 的优势在交叉编译和 Docker 镜像体积上回报巨大。服务端协议层（SMART protocol）是本项目必须自行实现的核心部分。

#### 前端框架对比

| 维度 | SvelteKit | Vue + Vite | React + Next.js |
|------|-----------|------------|-----------------|
| 编译体积 | 极小 (~10KB) | 小 (~30KB) | 大 (~40KB+) |
| 学习曲线 | 低 | 低 | 中 |
| SSR 支持 | ❌（SPA 模式） | ✅ | ✅ |
| Rust 集成 | 无特殊依赖 | 无特殊依赖 | 无特殊依赖 |

**选择 SvelteKit**：当前阶段使用 SPA 模式（编译为静态文件，adapter-static），编译产物最小，适合内嵌到二进制中或作为独立前端部署。

---

## 四、模块设计

### 4.1 Cargo Workspace 结构

```
ironforge/
├── Cargo.toml                    # workspace 根
├── crates/
│   ├── rg-core/                  # 核心业务逻辑
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── repo/             # 仓库管理
│   │   │   ├── user/             # 用户/组织
│   │   │   ├── auth/             # 认证授权
│   │   │   ├── issue/            # Issue 跟踪
│   │   │   ├── pull_request/     # PR 引擎
│   │   │   ├── wiki/             # Wiki 引擎
│   │   │   ├── hook/             # Git Hook 系统
│   │   │   └── lfs/              # LFS 存储
│   │   └── Cargo.toml
│   │
│   ├── rg-git/                   # Git 协议层
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── protocol/         # SMART 协议实现
│   │   │   │   ├── pkt_line.rs   # pkt-line 编解码
│   │   │   │   ├── upload_pack.rs
│   │   │   │   ├── receive_pack.rs
│   │   │   │   ├── capability.rs
│   │   │   │   └── packfile.rs   # pack 编解码
│   │   │   ├── transport/        # 传输适配
│   │   │   │   ├── ssh.rs
│   │   │   │   └── http.rs
│   │   │   └── object/           # Git 对象操作（gix 封装）
│   │   │       ├── blob.rs
│   │   │       ├── tree.rs
│   │   │       ├── commit.rs
│   │   │       ├── reference.rs
│   │   │       └── diff.rs
│   │   └── Cargo.toml
│   │
│   ├── rg-ssh/                   # SSH 服务端
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── server.rs         # russh 服务端
│   │   │   ├── auth.rs           # 公钥/密码认证
│   │   │   └── session.rs        # 会话管理
│   │   └── Cargo.toml
│   │
│   ├── rg-http/                  # HTTP 服务端 + REST API
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── api/              # REST API handlers（扁平结构）
│   │   │   │   ├── repos.rs
│   │   │   │   ├── users.rs
│   │   │   │   ├── issues.rs
│   │   │   │   ├── pulls.rs
│   │   │   │   ├── wiki.rs
│   │   │   │   ├── releases.rs
│   │   │   │   ├── admin.rs
│   │   │   │   ├── auth.rs
│   │   │   │   ├── mfa.rs
│   │   │   │   ├── sso.rs
│   │   │   │   └── ...
│   │   │   ├── git_v2.rs         # Git Protocol V2 handler
│   │   │   ├── security.rs       # CORS/CSP/security headers
│   │   │   ├── ws.rs             # WebSocket handler
│   │   │   └── oci.rs            # OCI container registry
│   │   └── Cargo.toml
│   │
│   ├── rg-db/                    # 数据库层
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── entities/         # SeaORM 实体
│   │   │   │   ├── user.rs
│   │   │   │   ├── repo.rs
│   │   │   │   ├── issue.rs
│   │   │   │   ├── pull_request.rs
│   │   │   │   ├── wiki.rs
│   │   │   │   ├── access_token.rs
│   │   │   │   └── ci_pipeline.rs
│   │   │   └── migrations/       # 数据库迁移
│   │   └── Cargo.toml
│   │
│   ├── rg-ci/                    # CI/CD 引擎
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── scheduler.rs      # 任务调度
│   │   │   ├── runner.rs         # Job 执行器
│   │   │   ├── pipeline.rs       # Pipeline 解析
│   │   │   ├── artifact.rs       # 产物管理
│   │   │   └── log.rs            # 构建日志
│   │   └── Cargo.toml
│   │
│   └── rg-cli/                   # CLI 入口（主二进制）
│       ├── src/
│       │   └── main.rs
│       └── Cargo.toml
│
├── web/                          # 前端 (SvelteKit)
│   ├── src/
│   │   ├── routes/
│   │   ├── components/
│   │   └── lib/
│   ├── package.json
│   └── svelte.config.js
│
├── ironforge.toml                # 默认配置
├── ironforge.example.toml        # 配置示例
│
├── docker/
│   ├── Dockerfile
│   └── docker-compose.yml
│
├── migrations/                   # SeaORM 迁移文件
│
└── docs/
    ├── architecture.md
    ├── git-protocol.md
    └── api-reference.md
```

### 4.2 数据库模型设计（核心表）

```
┌─────────────┐     ┌──────────────────┐     ┌──────────────────┐
│   users     │     │   repositories   │     │     issues       │
├─────────────┤     ├──────────────────┤     ├──────────────────┤
│ id          │◄──┐ │ id               │◄──┐ │ id               │
│ username    │   │ │ name             │   │ │ title            │
│ email       │   │ │ slug             │   │ │ body             │
│ password    │   │ │ description      │   │ │ state (open/     │
│ avatar_url  │   │ │ owner_id ────────┘   │ │   closed)        │
│ is_admin    │   │ │ is_private       │   │ │ repo_id ─────────┘
│ created_at  │   │ │ default_branch   │   │ │ author_id        │
│ updated_at  │   │ │ created_at       │   │ │ milestone_id     │
└──────┬──────┘   │ │ updated_at       │   │ │ created_at       │
       │          └──────────────────┘   │ │ updated_at       │
       │                                  └──────────────────┘
       │
       │          ┌──────────────────┐     ┌──────────────────┐
       │          │  pull_requests   │     │    wiki_pages    │
       │          ├──────────────────┤     ├──────────────────┤
       │          │ id               │     │ id               │
       │          │ title            │     │ title            │
       │          │ body             │     │ content (MD)     │
       │          │ state            │     │ repo_id          │
       │          │ repo_id          │     │ version          │
       │          │ author_id        │     │ created_at       │
       │          │ base_branch      │     │ updated_at       │
       │          │ head_branch      │     └──────────────────┘
       │          │ merged_at        │
       │          └──────────────────┘
       │
       │          ┌──────────────────┐     ┌──────────────────┐
       │          │  access_tokens   │     │  ci_pipelines    │
       │          ├──────────────────┤     ├──────────────────┤
       │          │ id               │     │ id               │
       │          │ user_id          │     │ repo_id          │
       │          │ token_hash       │     │ name             │
       │          │ name             │     │ config (YAML)    │
       │          │ scopes           │     │ status           │
       │          │ expires_at       │     │ trigger (push/   │
       │          └──────────────────┘     │  tag/manual)     │
       │                                   │ created_at       │
       │          ┌──────────────────┐     └──────────────────┘
       │          │  ssh_keys        │
       │          ├──────────────────┤     ┌──────────────────┐
       └────────►│ id               │     │  ci_jobs         │
                  │ user_id          │     ├──────────────────┤
                  │ public_key       │     │ id               │
                  │ fingerprint      │     │ pipeline_id      │
                  │ title            │     │ name             │
                  │ created_at       │     │ status           │
                  └──────────────────┘     │ log              │
                                           │ started_at       │
                                           │ finished_at      │
                                           └──────────────────┘
```

---

## 五、核心子系统设计

### 5.1 Git 协议层

这是整个项目**技术难度最高**的模块。需要从零实现 Git Smart Protocol（V1 + V2）。

#### SSH 通道（russh）

```
客户端 git clone/push
    │
    ▼ SSH 连接
┌──────────────────────────┐
│ russh Server             │
│  - 公钥认证 → rg-db 查询  │
│  - 密码认证 → argon2 验证  │
│  - exec_request 路由:     │
│    git-upload-pack    → fetch/clone
│    git-receive-pack   → push
└──────────┬───────────────┘
           │
           ▼
┌──────────────────────────┐
│ rg-git protocol 模块      │
│  - pkt-line 解析器        │
│  - capability 协商        │
│  - want/have 引用协商     │
│  - packfile 编解码        │
│  - side-band 多路复用     │
└──────────────────────────┘
```

#### HTTP 通道（Axum）

```
客户端 git clone/push (HTTPS)
    │
    ▼
┌──────────────────────────┐
│ Axum Router              │
│  GET  /{user}/{repo}.git/info/refs?service=git-upload-pack
│  POST /{user}/{repo}.git/git-upload-pack
│  POST /{user}/{repo}.git/git-receive-pack
│  GET  /{user}/{repo}.git/HEAD
└──────────────────────────┘
```

### 5.2 Pull Request 引擎

PR 的核心是 **Git diff 计算 + 状态机 + Webhook**：

```
PR 状态机:
    OPEN ──→ REVIEWING ──→ APPROVED ──→ MERGED
      │           │                          │
      ▼           ▼                          ▼
    CLOSED     CHANGES_REQUESTED           CLOSED

合并策略（支持多种）:
  - Merge Commit (默认)
  - Squash and Merge
  - Rebase and Merge
```

PR diff 计算依赖 gix 的 tree-diff 能力：
```rust
// 伪代码：计算 PR diff
fn compute_pr_diff(repo: &Repository, base: &str, head: &str) -> Result<Diff> {
    let base_tree = repo.resolve_commit(base)?.tree()?;
    let head_tree = repo.resolve_commit(head)?.tree()?;
    let diff = base_tree.diff(&head_tree)?;
    Ok(diff)
}
```

### 5.3 CI/CD 引擎

采用轻量级设计，**不依赖 Docker**，直接在宿主机执行：

```
.rustgit-ci.yml 定义:
  stages:
    - test
    - build
    - deploy
  jobs:
    test:
      stage: test
      script:
        - cargo test --all
    build:
      stage: build
      script:
        - cargo build --release
      artifacts:
        - target/release/myapp

触发方式:
  - push 到分支
  - 创建/更新 PR
  - 创建 Tag
  - 手动触发
```

Runner 执行模型：
```
Pipeline 被触发
    │
    ▼
Scheduler (rg-ci scheduler)
    │
    ├── Job 1 (test) ──── Runner 执行 ──── 状态: passed/failed
    │
    ├── Job 2 (build) ─── Runner 执行 ──── 产物: target/release/...
    │
    └── Job 3 (deploy) ── Runner 执行 ──── 状态: skipped (依赖 Job 2)
```

### 5.4 Wiki 引擎

Wiki 本质上是一个 **独立的 Git 仓库**，内容为 Markdown 文件：

```
每个仓库自动关联一个 .wiki.git 裸仓库:
  user/repo.wiki.git/
    ├── Home.md           (首页)
    ├── Getting-Started.md
    └── API-Reference.md

Web UI:
  - Markdown 渲染 (comrak)
  - 页面编辑历史 (Git log)
  - 页面版本对比 (Git diff)
```

---

## 六、分阶段开发计划

> ✅ **状态：全部完成（Phase 1-21）。** 以下为原始开发计划的回顾，所有 Phase 0-5 的功能已实现。实际实现细节见 CLAUDE.md。

### Phase 0：项目基建（1-2 周）

- [x] Cargo workspace 初始化
- [x] crate 结构搭建
- [x] 配置管理（config + TOML）
- [x] 日志系统（tracing）
- [x] SQLite 数据库初始化 + SeaORM 迁移
- [x] 基础 CLI 参数解析（clap）
- [x] Docker 构建脚本

**交付物**：能跑起来的空壳程序，显示欢迎页面

### Phase 1：核心 Git 协议（3-4 周）⭐ 最难

- [x] pkt-line 协议解析器
- [x] Git Smart Protocol V1 引用协商
- [x] git-upload-pack（clone/fetch）实现
- [x] git-receive-pack（push）实现
- [x] packfile 编解码（含 ofs-delta）
- [x] SSH 服务端（russh）+ 公钥认证
- [x] HTTP Git 智能协议
- [x] 仓库 CRUD（创建/删除/列表）

**交付物**：`git clone/push` 能正常工作

### Phase 2：用户系统 + Web UI 基础（2-3 周）

- [x] 用户注册/登录
- [x] SSH Key 管理
- [x] Access Token 管理
- [x] 仓库权限（Public/Private）
- [x] 前端框架搭建（SvelteKit）
- [x] 仓库列表页 + 代码浏览页
- [x] 文件历史/Blame

**交付物**：能注册登录、浏览代码的 Web 界面

### Phase 3：Issue + Pull Request（3-4 周）

- [x] Issue CRUD + 状态管理
- [x] Issue 标签 + 里程碑
- [x] Issue 评论
- [x] PR 创建（基于分支）
- [x] PR diff 计算 + 在线 review
- [x] PR 合并（三种策略）
- [x] PR 状态机 + Webhook
- [x] 通知系统（基础版）

**交付物**：完整的协作功能

### Phase 4：Wiki + LFS + 高级功能（2-3 周）

- [x] Wiki 引擎（Git 仓库后端）
- [x] Markdown 渲染 + 目录生成
- [x] LFS 协议实现
- [x] Webhook 系统
- [x] API Token 认证
- [x] 组织/团队概念
- [x] 活动流（Timeline）

**交付物**：功能对齐 Gitea

### Phase 5：CI/CD 引擎（3-4 周）

- [x] Pipeline YAML 解析
- [x] Job 调度器
- [x] Runner 执行引擎
- [x] 构建日志流式输出
- [x] 产物管理
- [x] CI 状态徽章
- [x] 触发规则配置
- [x] CI 设置页面

**交付物**：内置 CI/CD，对标 Gitea Actions

### 总周期估算：14-20 周（约 3.5-5 个月）

---

## 七、风险与缓解

| 风险 | 影响 | 缓解策略 |
|------|------|----------|
| Git 协议实现复杂度高 | Phase 1 可能延期 2-3 周 | 参考 openEuler rgssh 成果，先 V1 再 V2 |
| gix 服务端 API 不成熟 | 需要自己封装底层操作 | Packfile 编解码直接自己写，不依赖 gix |
| Rust 学习曲线 | 开发速度比 Go/Java 慢 30-50% | 每个模块先写最小可用版本，迭代完善 |
| 前端工作量大 | 可能占去 40% 时间 | 前期用 API 驱动开发，UI 最后统一做 |
| CI/CD 安全性 | 执行用户脚本的风险 | 沙箱隔离 + 资源限制 + 可选关闭 |

---

## 八、命名建议

| 候选名 | 含义 | 备注 |
|--------|------|------|
| **RustGit** | 直白 | 可能和 rust-lang/git 冲突 |
| **Forged** | Forge（铸造）+ Rust 后缀 ed | 读起来有 "锻造" 意味 |
| **IronForge** | 铁 + 锻造 | 强调 Rust（铁）+ 代码锻造 |
| **Redmine** | 已被占用 | ❌ |
| **GitForge** | 直白 | 太通用 |
| **Tin** | 锡 | 轻量 + 金属，但太短 |
| **Ferrox** | 拉丁语"铁" | 读音酷，但生僻 |

个人推荐 **IronForge** — Rust 本意就是铁，Forge 意为锻造，合起来就是"铁匠铺"，完美契合 Git 代码锻造的意象。

---

*此方案为 Phase 0 原始设计文档。截至 2026-06，Phase 0-20 已全部完成，实际实现细节见 CLAUDE.md 和项目源码。*
