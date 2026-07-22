# Contributing to IronForge

感谢你对 IronForge 的关注！本文档说明开发规范、crate 职责划分和常见工作流程。

---

## 目录

- [开发环境](#开发环境)
- [项目结构与 crate 职责](#项目结构与-crate-职责)
- [编码规范](#编码规范)
- [提交规范](#提交规范)
- [测试规范](#测试规范)
- [分支管理](#分支管理)
- [Phase 开发计划](#phase-开发计划)

---

## 开发环境

### 必要工具

```bash
# Rust stable (1.95+)
rustup update stable

# 代码格式化
rustfmt --edition 2021

# Lint
cargo clippy

# 系统依赖（macOS）
# git（用于 pack-objects / index-pack / update-ref）
which git   # 必须存在
```

### 推荐工具

```bash
# 快速重建（监听文件变化）
cargo install cargo-watch
cargo watch -x "build --release"

# 查看依赖树
cargo tree

# 审计安全漏洞
cargo audit
```

### 初次设置

```bash
git clone <repo>
cd ironforge
cargo build      # 验证依赖下载和编译通过

# 生成测试用 SSH 主机密钥（一次性）
ssh-keygen -t ed25519 -f /tmp/ironforge_host_key -N ""
```

---

## 项目结构与 crate 职责

### 依赖关系图

```
rg-cli
  ├── rg-core
  │     └── rg-db
  ├── rg-git
  ├── rg-ssh
  │     └── rg-git
  ├── rg-http
  │     ├── rg-git
  │     └── rg-core
  └── rg-db

rg-ci
  └── rg-db

rg-runner
  └── rg-db
```

### 各 crate 边界规则

#### `rg-git` — Git 协议层（纯协议，无业务逻辑）

**允许**：
- pkt-line / sideband 编解码
- upload-pack / receive-pack 协议处理
- 调用系统 `git` 命令（pack-objects、index-pack、update-ref、for-each-ref）
- 文件路径操作

**禁止**：
- 不能依赖 `rg-core`、`rg-db`、`rg-http`、`rg-ssh`
- 不能包含用户认证逻辑
- 不能直接访问数据库

#### `rg-ssh` — SSH 传输层

**允许**：
- russh 服务端实现
- exec_request 路由到 `rg-git`
- SSH 认证（公钥/密码查 DB，对接 `rg-core::auth` + `rg-db::ops`）

**禁止**：
- 不能包含 Git 协议解析逻辑（委托给 `rg-git`）
- 不能直接操作数据库

#### `rg-http` — HTTP 传输层

**允许**：
- Axum 路由定义
- Git Smart HTTP 端点实现
- REST API 端点（Users / Repos / Issues / PRs / Wiki / LFS / Webhooks / CI/CD）
- 中间件（认证、CORS、限流）

**禁止**：
- 不能包含 Git 协议解析逻辑（委托给 `rg-git`）
- 业务逻辑应委托给 `rg-core`

#### `rg-core` — 核心业务逻辑

**允许**：
- 用户/仓库/Issue/PR/Wiki/Hook 业务逻辑
- 认证授权（argon2 密码哈希、JWT）
- 权限校验

**禁止**：
- 不能包含 HTTP/SSH 协议细节
- 不能包含 Git wire 协议实现

#### `rg-db` — 数据库层

**允许**：
- SeaORM 实体定义
- 数据库迁移文件
- CRUD 操作函数

**禁止**：
- 不能包含业务逻辑
- 不能包含 HTTP/SSH 层代码

**迁移规范（必读，曾踩坑）**：
- ⚠️ `#[derive(Iden)] enum Foo { Table }` 生成的是**单数**表名 `foo`，而实体用复数 `#[sea_orm(table_name = "foos")]`。新增表时务必显式指定表名（`#[sea_orm(iden = "foos")]` 或 raw SQL）并与实体 `table_name` 对齐，否则运行时报 `no such table` 且后续 ALTER 迁移会让服务启动崩溃。
- 非幂等语句（`ADD COLUMN`/`CREATE` 等）用 `manager.has_table()/has_column()` 守卫，保证半执行后可安全重跑。
- 新增迁移后用全新库验证：`ironforge migrate` + `sqlite3 .tables` 核对表名。
- 给 `AppState` 加字段时，同步更新 `crates/rg-http/tests/common/mod.rs::build_test_app_state`。

#### `rg-cli` — 入口

**允许**：
- CLI 参数解析（clap）
- 各服务的启动和协调

**禁止**：
- 不能包含业务逻辑（全部委托给其他 crate）

#### `rg-runner` — Runner Agent（独立二进制）

**允许**：
- Runner 注册和心跳
- 从服务器轮询 Job
- Job 执行（本地 shell 或 Docker）
- 日志上传和 Artifact 上传

**禁止**：
- 不能直接操作 HTTP 路由（只作为 HTTP 客户端调用 rg-http API）
- 不能包含业务逻辑

---

## 编码规范

### 通用规范

```rust
// ✅ 错误处理：用 anyhow::Result 配合 ? 操作符
pub async fn do_something(path: &Path) -> anyhow::Result<()> {
    let output = std::process::Command::new("git")
        .arg("-C").arg(path)
        .args(["rev-parse", "HEAD"])
        .output()
        .context("failed to run git rev-parse")?;
    Ok(())
}

// ✅ 日志：用 tracing，结构化字段
tracing::info!(path = %repo_path.display(), user = %username, "Starting upload-pack");
tracing::error!(error = %e, "git index-pack failed");

// ❌ 不要用 println! / eprintln! 输出日志
println!("starting server");  // ❌
```

### async 规范

```rust
// ✅ 函数签名：泛型约束写明 Unpin
pub async fn write_pkt_line<W: AsyncWrite + Unpin>(writer: &mut W, ...) -> Result<()>

// ✅ BufReader：只在需要 read_pkt_line 的地方包装，用完立即 drop
{
    let mut reader = BufReader::new(&mut *stream);
    let result = process_push(repo_path, &mut reader).await?;
}  // BufReader drop 在这里，之后 stream 可以继续用于写

// ✅ 调用系统命令：用 tokio::process::Command 做异步
let mut cmd = tokio::process::Command::new("git")
    .arg("-C").arg(repo_path)
    .args(["index-pack", "--fix-thin", "--stdin"])
    .stdin(Stdio::piped())
    .spawn()?;
```

### 错误处理规范

```rust
// ✅ 库 crate：用 thiserror 定义错误类型
#[derive(thiserror::Error, Debug)]
pub enum AuthError {
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("user not found: {0}")]
    UserNotFound(String),
}

// ✅ 应用层/lib crate：用 anyhow::Result
pub async fn authenticate(username: &str, password: &str) -> anyhow::Result<User>

// ❌ 不要用 unwrap() / expect() 在生产路径中
let sha = output.stdout.first().unwrap();  // ❌
```

### 注释规范

关键算法和协议细节**必须**有注释说明：

```rust
// ✅ 解释"为什么"，而不只是"做什么"
// Git receive-pack 的 report-status 响应必须整体作为 band-1 sideband 数据发送。
// 不能先发 sideband flush 再发 plain pkt-lines——客户端在收到 sideband flush 后
// 就会停止读取，后续的 plain pkt-lines 将永远不会被读取。
// 参考：通过 GIT_TRACE_PACKET=1 对真实 git-receive-pack 抓包验证。
async fn send_response<W: AsyncWrite + Unpin>(...) -> Result<()> {
```

---

## 提交规范

遵循 [Conventional Commits](https://www.conventionalcommits.org/)：

```
<type>(<scope>): <description>

[body]

[footer]
```

### Type

| Type | 说明 |
|------|------|
| `feat` | 新功能 |
| `fix` | Bug 修复 |
| `docs` | 文档更新 |
| `refactor` | 重构（不改变行为） |
| `test` | 测试相关 |
| `chore` | 构建/依赖/工具相关 |
| `perf` | 性能优化 |

### Scope

使用 crate 名：`rg-git`、`rg-ssh`、`rg-http`、`rg-core`、`rg-db`、`rg-ci`、`rg-cli`

### 示例

```
feat(rg-ssh): implement SSH git push with sideband-64k report-status

Fix the SSH receive-pack response encoding: report-status pkt-lines
must be wrapped in band-1 sideband data, not sent as plain pkt-lines
after a sideband flush.

Closes #12
```

```
fix(rg-git): use read_pkt_line instead of read_line in process_push

Using read_line() caused UTF-8 parse failures when encountering binary
packfile data, since it tried to read the pkt-line length header as text.
```

---

## 测试规范

### 单元测试

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_write_pkt_line() {
        let mut buf = Vec::new();
        write_pkt_line(&mut buf, &PktLine::text("hello")).await.unwrap();
        assert_eq!(&buf, b"000ahello\n");
    }
}
```

### 覆盖率

项目使用 `cargo-llvm-cov` 进行代码覆盖率分析：

```bash
# 安装（首次）
cargo install cargo-llvm-cov

# macOS: 需要设置 LLVM 工具路径（Xcode Command Line Tools）
export LLVM_COV=/Library/Developer/CommandLineTools/usr/bin/llvm-cov
export LLVM_PROFDATA=/Library/Developer/CommandLineTools/usr/bin/llvm-profdata

# 生成覆盖率报告
cargo llvm-cov --lib                    # 文本报告
cargo llvm-cov --html --open            # HTML 报告（自动打开浏览器）
cargo llvm-cov --lcov --output-path target/coverage.lcov  # LCOV（Codecov/Coveralls）

# 运行测试 + 覆盖率
cargo llvm-cov
```

配置见 `cargo-llvm-cov.toml`。

### 集成测试与运行态回归

集成测试入口统一维护在 `scripts/` 下。不要再新增一次性端到端 shell
脚本，避免测试口径漂移。

```bash
# 全量回归：后端测试、前端静态检查/构建、运行态 smoke
node scripts/full-interface-regression.mjs

# 仅后端 OpenAPI 冒烟
BACKEND_URL=http://127.0.0.1:8080 node scripts/openapi-interface-smoke.mjs

# 仅前端页面 console/network 冒烟
BASE=http://127.0.0.1:5173 node scripts/console-smoke.mjs

# 前端 API client 与 OpenAPI 参数对齐
BACKEND_URL=http://127.0.0.1:8080 node scripts/api-client-contract-check.mjs
```

Git 协议专项问题可以按 `AGENTS.md` 中的 SSH/HTTP clone/push 命令模板手动复现。

---

## 分支管理

```
main          ← 稳定分支，只接受经过测试的 PR
dev           ← 开发主干
phase/2-auth  ← Phase 2 用户认证功能分支（示例）
fix/ssh-eof   ← Bug 修复分支（示例）
```

PR 合并到 `main` 前要求：
1. `cargo build --release` 通过
2. `cargo clippy` 无 error
3. `node scripts/full-interface-regression.mjs` 或对应子集回归通过

---

## Phase 开发计划

### ✅ Phase 0：基建（已完成）
- Cargo workspace
- 日志系统（tracing）
- CLI（clap）
- 基础错误处理

### ✅ Phase 1：Git 协议层（已完成，2026-04-24）
- pkt-line 协议
- git-upload-pack（clone/fetch）—— SSH + HTTP
- git-receive-pack（push）—— SSH + HTTP
- sideband-64k 多路复用
- SSH 服务端（russh）
- HTTP 服务端（Axum）

### ✅ Phase 2：用户系统（已完成，2026-04-24）
- `rg-db`：SeaORM 实体 + SQLite 迁移（users / repositories / ssh_keys / access_tokens）
- `rg-core/auth`：用户注册/登录（argon2 + JWT）
- SSH Key 管理 + 公钥认证
- 仓库权限（Public/Private）
- REST API 基础（`/api/v1/...`）

### ✅ Phase 3：Issue + Pull Request（已完成，2026-04-24）
- `rg-db`：issues / issue_comments / pull_requests / milestones 实体 + 迁移
- `rg-core/issue`：Issue CRUD + 标签 + 里程碑 + 评论
- `rg-core/pull_request`：PR 创建 + diff（git CLI）+ 合并（merge/squash/rebase 三策略）
- REST API：`/api/v1/repos/:owner/:name/issues/*` + `/api/v1/repos/:owner/:name/pulls/*`

### ✅ Phase 4：Wiki + LFS + Webhook（已完成，2026-04-24）
- `rg-db`：wiki_pages / lfs_objects / webhooks / webhook_deliveries 实体 + 迁移
- `rg-core/wiki`：Wiki 页面 CRUD
- `rg-core/lfs`：LFS batch API + 对象上传/下载（磁盘存储）
- `rg-core/webhook`：Webhook 注册/触发/投递 + HMAC-SHA256 签名
- REST API：wiki / lfs / webhooks 端点

### ✅ Phase 5：CI/CD + 权限鉴权（已完成，2026-04-24）
- `rg-db`：pipelines / pipeline_stages / pipeline_jobs 实体 + 迁移
- `rg-ci`：.ironforge-ci.yml 解析 + Pipeline 执行器（Stage/Job 串行执行）
- `rg-http/api/ci`：Pipeline REST API（list / trigger / retry / cancel / job detail）
- Push 自动触发 CI（receive-pack 后台触发）
- Push 自动触发 Webhook（push 事件 payload）
- HTTP Git 协议权限鉴权（Bearer Token → can_read/can_write）

### ✅ Phase 6：Web UI + 高级功能（已完成，2026-04-24）

- Web UI（SvelteKit 5，独立前端）
- 前端国际化（i18n）：中文 + 英文，199 个翻译 key，locale store + localStorage 持久化
- 代码审查 / 分支保护规则

### ✅ Phase 7~10：高级功能扩展（已完成，2026-04-24~05-11）

- **Phase 7**: TLS/HTTPS 支持（axum-server + rustls）
- **Phase 8**: TOML 配置文件 + 日志轮转（tracing-appender）
- **Phase 9**: API 统一分页 + GPG 签名验证
- **Phase 10**: 组织/团队系统 + 通知系统 + 邮件通知

### ✅ Phase 11~12：工程化改进（已完成，2026-04-27）

- **Phase 11**: 前端国际化完善（i18n 策略 + 翻译 key 补全）
- **Phase 12**: 代码覆盖率集成（cargo-llvm-cov）

### ✅ Phase 13：DB 分页 + V2 + Admin（已完成，2026-04-27~28）

- PaginatedResponse 统一分页（5 个 list API）
- Git Protocol V2 HTTP 集成完善
- Admin API 增强用户管理

### ✅ Phase 14~16：P0/P1 Gap 补齐（已完成，2026-05-08~09）

- **Phase 14~15**: Star/Watch、仓库删除/转移、Releases/Tags、Labels CRUD、Milestones、PAT、Fork、Commit Status、FTS5 搜索
- **Phase 16**: Webhooks 13 事件、Watch 通知集成、Labels-Issue 关联 API

### ✅ Phase 17：CI/CD Runner 收尾（已完成，2026-05-10）

- Runner Token Bearer 认证中间件（`authenticate_runner`）
- 外部 Runner 模式（`--external-runners` flag）
- Runner Agent 独立二进制（`crates/rg-runner/` → `ironforge-runner`）
- Artifact 管理（DB 迁移 + entity + ops + API 4 端点）
- Job 日志 WebSocket 实时推送（`/ws/job/:job_id`）
- Admin Runner 管理前端

### ✅ Phase 18：gix 迁移（已完成，2026-05-10）

- rg-ci CI 配置读取迁移（read_ci_config + has_ci_config → gix）
- rg-core checkout 迁移（git checkout ×2 → gix edit_reference）
- rg-core fast-forward 迁移（git merge --ff-only → gix repo.reference）
- 进度 ~60%（18 → 13 处 git CLI 保留）

### ✅ Phase 19：P2 功能（已完成，2026-05-11）

- R-14: Fork PR 跨仓库支持（head_repo_id + 跨仓库 diff/merge）
- R-15: Release Asset HTTP 端点（upload/download/list/get/delete）
- R-16: Search API 细分（SearchFilters qualifier 解析）

### ✅ Phase 20：工程化收尾（已完成，2026-05-11）

- 构建优化（release profile）
- 统一错误处理（AppError enum + IntoResponse）
- SQLite 性能调优（WAL + 7 项 PRAGMA）
- 配置校验（validate_config）
- 健康检查增强（DB ping + FS check）
- Request-ID 中间件 + Rate Limiter
- SQL 注入防护（参数化查询）
- 集成测试（10 个 API 测试）
- OpenAPI 全量覆盖（142 个 utoipa::path 注解 + Swagger UI）

完整计划见 [ARCHITECTURE.md](ARCHITECTURE.md)。

> **Phase 21（包注册表 / LDAP·SSO·2FA 企业认证 / 审计日志 / 数据迁移 / 镜像 / 看板 / 时间追踪 / 代码搜索 / SSH V2）也已完成**（2026-06-07 大规模功能扩展）。完整功能清单见 [CLAUDE.md](CLAUDE.md) 的「实现现状」。
