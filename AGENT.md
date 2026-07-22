# IronForge — AI Agent 协作指南

> 本文件是 IronForge 项目的 **AI 助手统一入口**。
> 大多数 AI 编程助手（Codex、Trae、CodeBuddy、WorkBuddy 等）会**优先自动读取本文件**，也会同时读取 `CLAUDE.md`。
> Claude Code 默认自动读取 `CLAUDE.md`，但同样会读取本文件。
> **建议**：所有 AI 助手先通读本文件获取概览，再根据任务深入 `CLAUDE.md` 或其他文档。

---

## 快速定位（30 秒了解项目）

**IronForge**（铁匠铺）是一个用 Rust 从零实现的轻量级 Git 托管平台，对标 Gitea/Forgejo。

- **目标**: 内存 <50MB、单二进制部署、全功能（仓库/Issue/PR/Wiki/CI）
- **阶段**: Phase 1~20 全部完成（核心功能 + Protocol V2 + 前端 i18n + P0/P1/P2 Gap + CI/CD Runner + 工程化）+ Phase 21（Package Registry / LDAP/SSO/2FA / Audit Log / Mirror / Board / Tracking / 代码搜索 / SSH V2）
- **技术栈**: Rust (Axum/SeaORM) + SvelteKit 5，SQLite，gix (gitoxide) + Git CLI gateway

### 关键文件速查

| 文件 | 用途 | 何时读取 |
|------|------|---------|
| `CLAUDE.md` | 最完整的 AI 协作上下文（踩坑记录、依赖版本、常见错误、实现现状清单） | **每次开始工作前必读** |
| `ironforge-docs/README.md` | 文档索引（单一事实来源） | 找任何分析报告时先读 |
| `ironforge-docs/architecture/project-architecture-2026-07.md` | 当前代码事实架构总览 | 设计新功能、核验模块边界时 |
| `ironforge-docs/architecture/frontend-backend-structure-2026-07.md` | 当前前后端结构和页面/API 映射 | 修改前端页面、API client 或 HTTP handler 时 |
| `ironforge-docs/architecture/architecture-followups-2026-07.md` | 当前已修复项、P2 长期方向和旧口径修正 | 判断风险、技术债和后续方向时 |
| `ARCHITECTURE.md` | 历史架构方案、技术选型决策、数据库模型 | 了解设计背景；当前事实以 2026-07 架构文档为准 |
| `CONTRIBUTING.md` | 开发规范、crate 边界规则、编码规范、Phase 计划 | 写新代码时 |
| `README.md` | 快速开始、REST API 示例、E2E 测试脚本 | 首次接触项目时 |
| `.ai/README.md` | AI Agent 接入指南（MCP + REST API + prompt 模板） | 需要让 AI 工具调用 IronForge 时 |

---

## 项目结构

```
ironforge/
├── Cargo.toml              # Workspace 根（统一依赖版本）
├── ARCHITECTURE.md         # 历史架构方案
├── CLAUDE.md               # 最完整的 AI 协作上下文 ⭐
├── CONTRIBUTING.md         # 开发规范
├── AGENT.md                # 本文件（AI 统一入口）
├── .ai/                   # AI Agent 接入规范（README + MCP配置 + prompt模板）
├── README.md               # 项目说明
├── docs/
│   ├── p0-prd.md                   # P0 功能 PRD
│   ├── p0-system-design.md         # P0 系统设计
│   ├── p0-completion-plan.md       # P0 完善方案 — 剩余缺口
│   ├── git-protocol.md             # Git 协议实现细节与踩坑记录
│   ├── ai-agent-integration.md     # AI Agent 集成方案
│   └── project-audit-2026-06.md    # 项目审计报告
├── ironforge-docs/
│   ├── README.md                               # 文档索引（单一事实来源）
│   ├── architecture/                           # 架构总览/前后端结构/后续/followups/分模块/DB多后端
│   ├── analysis/                               # 改进与优化整合报告
│   ├── comparison/                             # Gitea 对比 + 差距清单
│   ├── ci/                                     # CI Runner 架构
│   ├── testing/                                # 功能测试 + 审计
│   └── archive/                                # 过程文档与过时报告（追溯）
├── crates/
│   ├── rg-cli/             # 主二进制入口（bin = "ironforge"）
│   ├── rg-core/            # 核心业务逻辑
│   ├── rg-git/             # Git 协议层（pkt-line/V1/V2）
│   ├── rg-ssh/             # SSH 服务端（russh）
│   ├── rg-http/            # HTTP 服务端 + REST API（Axum）
│   ├── rg-db/              # 数据库层（SeaORM + SQLite）
│   ├── rg-ci/              # CI/CD 引擎
│   ├── rg-runner/          # Runner Agent（bin = "ironforge-runner"）
│   └── rg-mcp/             # MCP 服务器（bin = "ironforge-mcp"，stdio-only）
└── web/                    # SvelteKit 前端（不在 crates/ 下）
```

---

## 技术栈速查

| 层级 | 选型 | 版本 |
|------|------|------|
| HTTP 框架 | axum + axum-server | 0.8 / 0.7 |
| SSH 服务端 | russh | 0.51 |
| Git 操作 | gix (gitoxide) + git CLI fallback | 0.84 |
| ORM | SeaORM | 1.1 |
| 数据库 | SQLite | — |
| 前端 | SvelteKit 5 SPA | — |
| 认证 | argon2 + JWT HS256 | — |
| TLS | rustls + axum-server | — |

---

## 常见命令

```bash
# 编译（release 构建用于集成测试）
cargo build --release

# 启动服务器
./target/release/ironforge serve \
  --repo-root /tmp/ironforge/repos \
  --http-addr 0.0.0.0:8080 \
  --ssh-addr  0.0.0.0:2222 \
  --host-key  /tmp/ironforge_host_key

# 创建测试仓库
./target/release/ironforge create-repo <owner> <repo> --repo-root /tmp/ironforge/repos
```

---

## 按任务类型延伸阅读

### 修改 Git 协议相关代码
→ `docs/git-protocol.md` — pkt-line 格式、sideband 多路复用、upload-pack/receive-pack 实现细节

### 开发新功能 / 规划下一步
→ `CLAUDE.md` 中「实现现状」表格 — 确认功能是否已实现
→ `ironforge-docs/architecture/project-architecture-2026-07.md` — 核验当前架构事实
→ `ironforge-docs/architecture/architecture-followups-2026-07.md` — 查看已修复项和长期方向
→ `ARCHITECTURE.md` — 了解历史设计意图
→ `CONTRIBUTING.md` — 遵循编码规范

### gix 迁移 / 替换 git CLI 调用
→ `CLAUDE.md` 中「实现现状」表格 — 当前迁移进度和剩余 CLI 调用

### CI/CD Runner 开发
→ `ironforge-docs/ci/ci-runner-architecture.md` — Runner 调度架构

### 前端开发（SvelteKit）
→ `CLAUDE.md` 中「前端技术要点」— i18n 策略、Svelte 5 runes 用法

---

## 各 AI 工具的读取习惯

| AI 工具 | 自动读取的文件 | 深度参考 |
|---------|-------------|---------|
| **Claude Code** | `CLAUDE.md`（默认）+ `AGENT.md` | 本文件提供概览，`CLAUDE.md` 提供最完整细节 |
| **Codex / Trae / CodeBuddy** | `AGENT.md`（优先）+ `CLAUDE.md` | 本文件提供概览，`CLAUDE.md` 提供踩坑记录和依赖版本 |
| **WorkBuddy** | `.workbuddy/memory/MEMORY.md` | `AGENT.md` + `CLAUDE.md` + 2026-07 架构文档 |

> 💡 **设计意图**: `AGENT.md` 是轻量级统一入口（适合所有 AI 工具快速上手），`CLAUDE.md` 是深度上下文（包含完整的踩坑记录、依赖版本、常见错误排查）。两者互补，建议搭配使用。

---

## 分析报告索引

`ironforge-docs/` 已按主题分子目录整理，完整索引见 [`ironforge-docs/README.md`](ironforge-docs/README.md)：

| 子目录 | 内容 |
|------|------|
| `architecture/` | 架构总览、前后端结构、后续待办、分模块分析、DB 多后端 |
| `analysis/` | 改进与优化整合报告（2026-06-09 规划 + 2026-06-17 落地） |
| `comparison/` | Gitea 功能对比 + 差距清单 |
| `ci/` | CI Runner 架构 |
| `testing/` | 功能测试指南 + 前后端审计 |
| `archive/` | 过程文档与过时报告（仅供追溯） |

> 过时分析报告已移至 `archive/`，详阅 `archive/ARCHIVE.md`。

---

*本文件与 `CLAUDE.md` 保持同步更新。如发现有遗漏或不一致，请同步修正两文件。*
