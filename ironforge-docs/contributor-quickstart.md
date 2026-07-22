# IronForge 贡献者快速上手

> 目标：5 分钟把项目跑起来，并提交第一个 PR。
> 详细开发规范见 [CONTRIBUTING.md](../CONTRIBUTING.md)；AI 助手上下文见 [AGENT.md](../AGENT.md) / [CLAUDE.md](../CLAUDE.md)。
> 文档总索引以 [ironforge-docs/README.md](README.md) 为单一事实来源。

---

## 项目一句话

**IronForge（铁匠铺）** 是一个用 Rust 从零实现的轻量级 Git 托管平台，对标 Gitea / Forgejo——内存 <50MB、单二进制部署、全功能（仓库 / Issue / PR / Wiki / CI / 包注册表 / 企业认证 / 审计 / 代码搜索）。

## 技术栈速览

| 层 | 技术 |
|----|------|
| 语言 | Rust 1.95 (stable) |
| HTTP | Axum 0.8 + axum-server (rustls, TLS) |
| SSH | russh 0.51 |
| Git | gix 0.84 + `GitCommandGateway`（部分能力仍经 git CLI） |
| 数据库 | SeaORM 1.1 (SQLite) |
| 认证 | Argon2 + JWT (HS256) |
| CI/CD | serde_yaml + sh / docker |
| 前端 | SvelteKit 5 SPA，中英双语 i18n（199 key） |

## 环境要求

- Rust stable 1.95+（`rustup update stable`）
- Node.js（前端构建与回归脚本）
- 系统 `git`（pack-objects / index-pack / update-ref 等仍被调用，必须存在）
- SQLite 由 SeaORM 自动管理，无需额外安装

## 5 分钟跑起来

```bash
git clone https://github.com/lengyuqu/ironforge
cd ironforge
cargo build --release

# 生成 SSH 主机密钥（一次性）
ssh-keygen -t ed25519 -f /tmp/ironforge_host_key -N ""

# 启动
./target/release/ironforge serve \
  --repo-root /tmp/ironforge/repos \
  --http-addr 0.0.0.0:8080 \
  --ssh-addr  0.0.0.0:2222 \
  --host-key  /tmp/ironforge_host_key

# 浏览器打开 http://localhost:8080
```

## 本地开发循环

1. **分支**：从 `main` 切 `feat/xxx` 或 `fix/xxx`
2. **改代码** → `cargo build` 零警告
3. **测试**：
   - 后端单元 / 集成：`cargo test`
   - 全量回归（后端 + 前端 + 运行态）：`node scripts/full-interface-regression.mjs`
   - 覆盖率：`cargo llvm-cov`
4. **Lint**：`cargo clippy` 无 error
5. **提交**：Conventional Commits（`feat(rg-git): ...` / `fix(rg-core): ...`）
6. **PR**：合并到 `main` 前需 build + clippy + 回归全绿

## crate 职责（速记）

```
rg-cli      入口 / 启动协调
rg-core     业务逻辑（auth / repo / issue / pr / wiki / lfs / package_registry / search / audit ...）
rg-git      Git 协议层（pkt-line / upload-pack / receive-pack，纯协议）
rg-ssh      SSH 服务端（russh）
rg-http     HTTP + REST API + Git Smart HTTP + WebSocket
rg-db       SeaORM 实体 / 迁移 / ops
rg-ci       CI/CD 引擎（YAML 解析 + Pipeline 执行）
rg-runner   Runner Agent 独立二进制（ironforge-runner）
rg-mcp      MCP 服务器（ironforge-mcp，暴露 Tools + Resources 给 AI Agent）
```

## 常见坑（先看这个）

- **gix `!Send`**：`gix::Repository` 含 `RefCell`，async fn 中不得跨 `.await` 持有，用同步块收集数据后再做 async I/O。
- **Git CLI 统一**：所有 git 命令必须经 `GitCommandGateway`，禁止新增 `Command::new("git")`（防回归守卫 `test_no_raw_git_command_in_crates`）。
- **SeaORM 表名**：`#[derive(Iden)]` 生成单数表名，实体用复数 `table_name`，新增表务必显式对齐，否则运行时 `no such table`。
- **Axum 共享 State**：所有嵌套路由必须共享同一 `State<AppState>`。

## 文档地图

| 我想了解… | 读这个 |
|----------|--------|
| 项目说明 / 快速开始 | [README.md](../README.md) |
| 开发规范 / crate 边界 / 提交测试 | [CONTRIBUTING.md](../CONTRIBUTING.md) |
| AI 助手上下文（轻量入口） | [AGENT.md](../AGENT.md) |
| AI 深度上下文（踩坑 / 现状） | [CLAUDE.md](../CLAUDE.md) |
| 架构总览 / 前后端结构 | [architecture/](architecture/) |
| 改进与优化整合报告 | [analysis/improvement-analysis.md](analysis/improvement-analysis.md) |
| vs Gitea 功能对比 | [comparison/gitea-vs-ironforge-2026.md](comparison/gitea-vs-ironforge-2026.md) |
| 文档总索引 | [README.md](README.md) |
