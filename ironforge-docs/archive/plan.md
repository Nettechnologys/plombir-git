# IronForge — Git CLI 统一 & gix 迁移技术债 修复计划

> 本文件是交给开发智能体执行的任务说明。范围：**Phase 1 + Phase 2 执行**，Phase 3 仅记录待办。
> 约束：`cargo build` 0 警告、`cargo test` 全绿；**PR diff 迁移要求与 git CLI 字节级一致**。

> **执行状态（2026-07-04）**
> - **Phase 1 ✅ 完成**：所有 raw `Command::new("git")` 已统一到 `GitCommandGateway`。原盘点遗漏的 `repo/service.rs` 13 处（`auto_init` / `create_or_update_file` / `delete_file`）已迁移；网关新增 `run_with_env` 支持 commit 身份 env；防回归守卫 `test_no_raw_git_command_in_crates` 移除 `repo/service.rs` 豁免后通过；`/health` 已用 `global_gateway().is_ok()`。
> - **Phase 2A ✅ 完成（按回退条款）**：numstat 已 gix 化（`gix_diff_numstat`）；unified patch 字节级一致性无法在合理工作量内达成，按回退条款保留网关-CLI + `TODO(gix)` 标注，登记 Phase 3。
> - **Phase 2B ✅ 完成**：commit gpgsig 头读取已用 gix `extra_headers()`（`repo_content.rs`）；加密级验签留 Phase 3。
> - **Phase 3 ✅ 已登记**：`CLAUDE.md` 已记录 rebase / pack / thin-pack / GPG 验签 / bare clone 待办与复查条件。

## Context（背景）

IronForge 把 git 操作分两层逐步收敛：
1. **Git CLI 统一**：所有 `git` 子进程调用应经 `GitCommandGateway`（`crates/rg-git/src/cli_gateway.rs`），统一获得超时、tracing、结构化错误与版本校验。当前仍有 **15 处 raw `Command::new("git")`** 散落在外，绕过了这些保障。
2. **gix 原生迁移**：用 gix Rust API 取代 git 子进程，减少 spawn、提升性能与可移植性。进度 ~70%，剩余集中在 diff / rebase / pack / GPG / clone。部分今天可做（diff、commit header），部分必须等 gix 上游成熟（rebase、pack 生成/索引、加密级 GPG 验签、本地 bare clone）。

---

## 现状盘点（代码已核实，行号以当前 main 为准，可能漂移）

| 位置 | 调用 | 处理 |
|---|---|---|
| `crates/rg-core/src/pull_request/service.rs:316,544` | `git fetch`（fork ref，2 处） | Phase 1 → 网关 |
| `crates/rg-core/src/pull_request/service.rs:350,391,425,465` | `git diff --numstat` + patch（4 处） | Phase 2 → gix blob-diff（严格对齐） |
| `crates/rg-core/src/pull_request/service.rs:610,619,698,708` | `git rebase` / `--abort`（4 处） | Phase 1 → 网关；gix 原生 = Phase 3（阻塞） |
| `crates/rg-git/src/protocol/upload_pack.rs:358` | `git pack-objects`（async） | Phase 1 → `spawn_async`；gix 原生 = Phase 3 |
| `crates/rg-git/src/protocol/receive_pack.rs:280` | `git index-pack --fix-thin`（async） | Phase 1 → `spawn_async`；gix 原生 = Phase 3 |
| `crates/rg-git/src/protocol/v2.rs:814` | `git pack-objects` | Phase 1 → 网关/`spawn_async`；gix 原生 = Phase 3 |
| `crates/rg-http/src/api/archive.rs:49` | `git archive` | Phase 1 → 网关（gix 原生非本次范围） |
| `crates/rg-http/src/lib.rs:620` | `git --version`（健康检查，spawn_blocking） | Phase 1 → 复用网关版本校验 |
| `crates/rg-http/src/api/repo_content.rs:706` | `git cat-file commit`（读 gpgsig 头） | Phase 2 → gix `extra_headers()` |

`import/service.rs`、`mirror/service.rs`、`repo_content.rs`(大部分) **已走网关**。`repo/service.rs` 原盘点误标为"已走网关"——实际 `auto_init` / `create_or_update_file` / `delete_file` 共 13 处 raw `Command::new("git")`，本轮（2026-07-04）已全部迁移至网关。

**网关 API（`crates/rg-git/src/cli_gateway.rs`，已存在，复用）**：
- `cli_gateway::global_gateway() -> &'static Result<GitCommandGateway>`（懒初始化 + 版本校验）
- `.run(args: &[&str], repo_path: Option<&Path>) -> Result<GitOutput>`（同步，带超时）
- `.run_or_bail(args, repo_path) -> Result<()>`
- `.spawn_async(args, repo_path) -> Result<tokio::process::Child>`（流式，已 `kill_on_drop(true)`）
- `GitOutput`：`.success()` / `.stdout_str()` / `.stderr_str()` / `.ensure_success()`

---

## Phase 1 — 统一所有 git 调用到 GitCommandGateway（执行）

目标：消除全部 raw `Command::new("git")`，建立"网关之外不得直接 spawn git"的不变量。

**做法**：
- **同步调用**（`pull_request` 的 fetch/rebase、`archive`、`health`）：
  ```rust
  let git = rg_git::cli_gateway::global_gateway().as_ref().map_err(|e| anyhow::anyhow!("{e}"))?;
  let out = git.run(&["fetch", &head_repo_path_str, &refspec], Some(&base_repo_path))?;
  // 保留原语义：fetch 失败 warn 非致命；rebase 失败 abort + bail
  ```
  注意 `-C <repo_path>` 由网关的 `repo_path` 参数自动添加，不要再手写 `-C`。
- **异步 pack 流式**（`upload_pack`/`receive_pack`/`v2`）：改用 `gateway.spawn_async(&[...], Some(repo))` 拿 `tokio::process::Child`，沿用现有 stdin 写入 / stdout 读取 / sideband 封装逻辑。
  ⚠️ `index-pack --fix-thin --stdin`（见 CLAUDE.md 踩坑 #4）与 pack-objects 的管道时序**不能变**；只替换进程创建方式。
- **健康检查**（`rg-http/src/lib.rs` 的 `/health`）：把 `git` 项改为读取 `global_gateway().is_ok()`（网关构造时已校验 `git --version`），省一次 spawn。
- **rebase（4 处）**：移到网关，但**仍是 CLI**（gix 原生留到 Phase 3）；保留 `rebase --abort` 容错分支。

**防回归守卫**：在 `crates/rg-git/` 加测试，扫描整个 `crates/*/src` 源码，断言除 `cli_gateway.rs` 外无 `Command::new("git")` / `process::Command::new("git")`，失败时报出具体文件与行。

**验收**：`cargo build` 0 警告；`cargo test` 全绿；HTTP+SSH 的 clone/push e2e（pack 路径）通过。

---

## Phase 2 — 今天可行的 gix 原生迁移（执行）

### 2A. PR diff → gix `blob-diff`（要求与 git CLI 字节级一致）
- 改 `compute_same_repo_diff` / `compute_cross_repo_diff`（`crates/rg-core/src/pull_request/service.rs`）。
- **numstat（additions/deletions/status/files_changed）**：用 gix tree-to-tree diff（`gix::object::tree::diff` + 行计数），可靠达成数值对齐。
- **unified patch 文本**：用 gix `blob-diff`（workspace 已启用 `blob-diff` feature，见 `Cargo.toml`）生成 3 行上下文 unified diff。
- **严格一致的硬门槛**：新增对拍测试 —— 固定 fixture 仓库上，断言 gix 输出与 `git diff` / `git diff --numstat` **逐字节相同**（覆盖 hunk 头、mode/rename、二进制、空 diff 等边界）。
  - patch 文本能字节一致 → 完成迁移。
  - 某些边界无法在合理工作量内对齐 → **patch 文本回退保留网关-CLI**（仍把 numstat 迁到 gix，部分收益），代码注释 + Phase 3 记录该子项为 gix 阻塞。**绝不降低输出质量来凑一致**。

### 2B. commit gpgsig 头读取 → gix `extra_headers()`
- `crates/rg-http/src/api/repo_content.rs` 的 `verify_commit_signature` 当前用 `git cat-file commit` 读 `gpgsig` 头（注释"gix 不易暴露"已过时）。
- 改用 gix：`commit.decode()?` → `CommitRef::extra_headers()` 查 `gpgsig`，去掉该 gateway 调用。
- ⚠️ **仅迁移"是否存在签名 / 读取签名块"**；**加密级验签**仍依赖外部 gpg → Phase 3。保持 `verified` 字段语义不回退。

**验收**：PR diff 对拍测试通过（或按回退条款记录）；签名端点 `GET /repos/:owner/:name/commits/:sha/signature` 行为不变。

---

## Phase 3 — 等待 gix 上游成熟（本次不执行，仅记录 + 复查条件）

在对应代码处保留/补充 `TODO(gix)` 标注，并在 `CLAUDE.md`「剩余差距/技术债」登记。每项给出解除阻塞触发条件：

| 待办 | 阻塞原因 | 复查 / 解除条件 |
|---|---|---|
| Rebase 合并（PR rebase ×2 路径） | `gix-rebase` 仍处 "idea" 阶段，无 API | gix 发布稳定 rebase API |
| Pack 生成（upload-pack / v2 fetch） | gix 无高层 pack 协商/生成 API | gix 提供 server 端 pack 生成 |
| Thin-pack 索引（receive-pack `index-pack --fix-thin`） | gix 缺针对现有 ODB 的 thin 补全解析 | gix `gix-pack` 支持 `--fix-thin` 等价 |
| 加密级 GPG 验签 | gix 无验签；需 gpgme/sequoia | gix 内建验签，或单独引入 `sequoia-openpgp` |
| 本地 bare clone（fork） | `prepare_clone_bare` 不支持本地路径 | gix 支持 file-transport bare clone |
| git archive 原生化（可选） | **非 gix 阻塞**（可用 gix tree-walk + tar/flate2）；本次未选 | 视需要单独排期 |

复查节奏：每次 `gix` 版本升级（当前 `0.84`，见 `Cargo.toml`）时过一遍本表。

---

## 关键文件
- `crates/rg-git/src/cli_gateway.rs` — 网关（复用，可能补 stdin 便利方法）
- `crates/rg-core/src/pull_request/service.rs` — fetch/diff/rebase（Phase 1+2 主战场）
- `crates/rg-git/src/protocol/{upload_pack,receive_pack,v2}.rs` — pack 流式（Phase 1）
- `crates/rg-http/src/api/archive.rs`、`crates/rg-http/src/lib.rs` — archive / health（Phase 1）
- `crates/rg-http/src/api/repo_content.rs` — commit header（Phase 2B）
- `CLAUDE.md` — Phase 3 待办与复查条件登记

## 验证清单
1. `cargo build --release` — 0 警告；`cargo test --release` — 全绿。
2. **防回归守卫测试**通过：源码中除 `cli_gateway.rs` 外无 raw `Command::new("git")`。
3. **PR diff 对拍测试**：gix 输出与 `git diff` / `--numstat` 逐字节一致（或按 2A 回退条款记录）。
4. e2e：起 `serve`，HTTP `git clone` + `git push`（验证 pack 流式未回归）、SSH clone/push；建跨仓库/同仓库 PR 验证 diff 与 merge（merge/squash 走 gix，rebase 走网关-CLI）；命中签名端点。
5. 现有集成测试（`crates/rg-http/tests/api_tests.rs`、`org_tests.rs`）保持通过。

## 交付
Phase 1+2 改动 + 防回归守卫 + 对拍测试 + Phase 3 文档登记，作为一组提交。
