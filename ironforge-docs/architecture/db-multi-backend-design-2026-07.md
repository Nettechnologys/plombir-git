# IronForge 多数据库后端支持 — 设计文档（PRD + 架构 + 任务分解）

> 主理人：齐活林（Qi） · 交付总监
> 日期：2026-07-08
> 范围：为 IronForge 增加 PostgreSQL / MySQL 可选后端支持，对标 Gitea 双后端能力。

> **2026-07-13 实施验证更新**：连接分流、后端感知迁移/FTS 和 CI smoke 已落地，并在真实 PostgreSQL、MySQL 服务上通过 migration、CRUD、counter、Wiki/仓库 FTS、并发登录锁定及完整服务启动 `/health` 验证。验证过程中修复了时间默认值、严格外键顺序/类型、MySQL TEXT 默认值、`NATURAL LANGUAGE MODE` 语法及 PostgreSQL UTC timestamp 类型问题；CLI/数据库层连接诊断不会再输出 URL 密码。

## 1. 目标与范围（已与用户确认）

| 决策点 | 结论 |
|--------|------|
| 后端范围 | PostgreSQL **与** MySQL 同时支持 |
| 全文检索 | 跨后端**完整 FTS**（PG 原生 FTS / MySQL FULLTEXT），非降级 |
| 数据迁移 | 不提供 SQLite→PG/MySQL 迁移工具，仅新部署可用 |
| 二进制体积 | 以 cargo **feature-flag** 控制（`db-sqlite` 默认；`db-postgres`/`db-mysql` 按需启用），保持默认构建轻量 |

**产品目标**：在不破坏 SQLite 嵌入式零依赖体验的前提下，使 IronForge 能以 PostgreSQL 或 MySQL 作为可选后端部署，支撑更高并发与团队/企业场景。

**用户故事（运维视角）**
1. 作为运维，我可以用 PostgreSQL 实例作为 IronForge 后端以支撑更高并发。
2. 作为运维，我可以用 MySQL 实例作为 IronForge 后端以适配既有基础设施。
3. 作为运维，我可以通过 `database_url` 的 scheme（sqlite:// / postgres:// / mysql://）切换后端而无需改代码。
4. 作为用户，我在 PG/MySQL 后端下仍可使用仓库/Issue/Wiki/代码的全局全文检索。

## 2. 实施前约束（2026-07-08 历史基线）

1. `Cargo.toml` 中 `sea-orm` / `sea-orm-migration` 仅开 `sqlx-sqlite`，PG/MySQL 驱动未编入。
2. `rg-db/src/lib.rs::connect()` 仅构造 SQLite connector + PRAGMA，无 scheme 分流。
3. 约 20 处写死 `DatabaseBackend::Sqlite`（搜索 6 + code_indexer 4 + 多个 ops）。
4. 全文检索基于 SQLite **FTS5 虚拟表**（`repos_fts`/`issues_fts`/`wiki_pages_fts`/`code_fts`），迁移脚本使用 FTS5 专有 DDL 与 `MATCH`/`snippet()`/`rank` 语法。
5. CLI 备份执行 `VACUUM INTO`（SQLite 专有）；部分测试用 `sqlite::memory:`。

## 3. 架构方案

### 3.1 连接层（P0）
- `rg-db/src/lib.rs` 的 `connect*` 按 `db_url` scheme 分流：
  - `sqlite://` → 现有 `SqliteConnectOptions`（PRAGMA 不变）
  - `postgres://` / `postgresql://` → `PostgresConnectOptions`（cfg 门控 `#[cfg(feature="db-postgres")]`）
  - `mysql://` → `MySqlConnectOptions`（cfg 门控 `#[cfg(feature="db-mysql")]`）
- 未启用对应 feature 却传入该 scheme 时，返回清晰错误。
- SeaORM 的 `Database::connect(url)` 已能按 scheme 自动选驱动；门控仅控制驱动是否编入。

### 3.2 动态后端判定（P0）
- 全量替换写死 `DatabaseBackend::Sqlite` 为 `db.get_database_backend()`（ops/搜索层）。
- SeaORM `Statement` 占位符 `?` 会被自动翻译为 PG 的 `$N` / MySQL 的 `?`，故现有参数化 SQL 跨后端安全。

### 3.3 全文检索抽象（P1，最大工作量）
- 新增 `crates/rg-core/src/search/dialect.rs`，统一产出各后端的 FTS DDL 与查询片段：

| 后端 | FTS 表定义 | 匹配谓词 | 排序 | 高亮片段 |
|------|-----------|----------|------|----------|
| SQLite | `CREATE VIRTUAL TABLE x USING fts5(...)` | `x MATCH ?` | `ORDER BY rank` | `snippet(x,col,'<b>','</b>','...',20)` |
| Postgres | `tsv tsvector GENERATED ALWAYS AS (to_tsvector('simple', ...)) STORED` + GIN 索引 | `x.tsv @@ plainto_tsquery('simple', ?)` | `ORDER BY ts_rank_cd(x.tsv, plainto_tsquery('simple', ?)) DESC` | 用 `ts_headline` |
| MySQL | `FULLTEXT(col1, col2)`（InnoDB） | `MATCH(col1,col2) AGAINST (? IN NATURAL LANGUAGE MODE)` | 默认相关度 | 无内建高亮（返回 content 前 200 字符） |

- PG/MySQL 的 FTS 列由数据库自动维护（PG 生成列 / MySQL FULLTEXT 索引），无需应用层触发器；SQLite 维持现有应用层同步。
- `search/service.rs` 与 `code_indexer.rs` 改为调用 `dialect` 生成 SQL。
- `rebuild_fts_indexes`（lib.rs）改为后端感知：SQLite=现有 DELETE+INSERT；PG/MySQL=自动维护，仅 `ANALYZE`/`OPTIMIZE`。

### 3.4 迁移脚本（P1）
- `m20260508_000005_create_fts5_indexes.rs` 与 `m20260512_000001_create_code_fts.rs` 的 `up()` 按 `manager.get_database_backend()` 分支：
  - SQLite → 现有 FTS5 + triggers
  - Postgres → 生成列 + GIN 索引
  - MySQL → FULLTEXT 表
- `down()` 同样分支清理。

### 3.5 CLI / 配置（P0）
- `rg-cli` 接受 `postgres://` / `mysql://` URL（默认仍为 sqlite）。
- `backup` / `restore` 命令：仅 SQLite 支持 `VACUUM INTO`；其他后端返回明确错误（dump 工具留待后续）。

## 4. 文件变更清单

| 文件 | 变更 |
|------|------|
| `Cargo.toml` | sea-orm/sea-orm-migration 加 `default-features=false`；追加 `db-sqlite/db-postgres/db-mysql` workspace features |
| `crates/rg-db/Cargo.toml` | 增加 `[features]` 透传 |
| `crates/rg-db/src/lib.rs` | connect 分流 + rebuild_fts 后端感知 |
| `crates/rg-db/src/migrations/m20260508_000005_*.rs` | up/down 后端分支 |
| `crates/rg-db/src/migrations/m20260512_000001_*.rs` | up/down 后端分支 |
| `crates/rg-core/src/search/dialect.rs` | **新增** FTS 方言模块 |
| `crates/rg-core/src/search/service.rs` | 调用 dialect 生成 SQL |
| `crates/rg-core/src/search/code_indexer.rs` | 调用 dialect 生成 SQL |
| `crates/rg-db/src/ops/{package,issue_label,label,org,package_version}_ops.rs` | 动态后端 |
| `crates/rg-cli/src/main.rs` | backup/restore 门控 + URL 接受 |

## 5. 任务分解（有序，含依赖）

1. **T1** [P0] Cargo feature flags（workspace + rg-db） — 无依赖
2. **T2** [P0] `rg-db/src/lib.rs` connect scheme 分流 + PG/MySQL cfg 门控 — 依赖 T1
3. **T3** [P0] 全量替换 `DatabaseBackend::Sqlite` 为 `db.get_database_backend()`（ops + search） — 无依赖
4. **T4** [P0] CLI backup/restore 门控 + URL 接受 — 依赖 T1
5. **T5** [P1] 新增 `search/dialect.rs` FTS 方言 — 无依赖
6. **T6** [P1] 重构 `service.rs` / `code_indexer.rs` 使用 dialect — 依赖 T5
7. **T7** [P1] FTS 迁移脚本后端分支 — 依赖 T5
8. **T8** [P1] `rebuild_fts_indexes` 后端感知 — 依赖 T5
9. **T9** [QA] `cargo check --workspace`（默认 sqlite）必须通过；尝试 `--features db-postgres,db-mysql` 编译

## 6. 风险与待明确

- **实库 smoke 已完成**：2026-07-13 PostgreSQL/MySQL 均通过迁移、CRUD、计数器、FTS 与并发认证验证；这不替代 HA、故障恢复和长期压力测试。
- **驱动体积**：启用 `db-postgres`/`db-mysql` 会显著增大二进制（sqlx-postgres 体量较大），故默认构建仅含 sqlite。
- **FTS 语义差异**：SQLite `rank` 与 PG `ts_rank_cd` / MySQL 相关度排序口径不同，排序结果近似而非完全一致；高亮在 MySQL 下退化为 content 前 N 字符。
- **触发器 vs 生成列**：SQLite 维持应用层同步；PG/MySQL 由 DB 自动维护，应用层写入对 FTS 列无害。

## 7. 验收标准

- `cargo check --workspace`（默认）零错误，且 SQLite 既有行为不变。
- `cargo check -p rg-db --features db-postgres,db-mysql` 可编译（驱动编入）。
- 连接层能据 `database_url` scheme 选后端；非法组合给出明确错误。
- FTS 查询层在三后端下生成语义正确的 SQL；SQLite 由本地测试覆盖，PostgreSQL/MySQL 已通过真实服务 smoke。
