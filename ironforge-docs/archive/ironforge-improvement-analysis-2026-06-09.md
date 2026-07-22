# IronForge 全方位改进空间分析报告

> **评审时间**：2026-06-09  
> **评审对象**：IronForge v0.x（Phase 1~21 全部完成）  
> **技术栈**：Rust / Axum 0.8 / SeaORM 1.1(SQLite) / gix 0.84 / russh 0.51 / SvelteKit 5

---

## 执行摘要

IronForge 已完成 21 个迭代阶段，功能完备度已达到轻量级 Git 托管平台的生产基准水位。然而，在从「功能验证」走向「生产稳定」的路径上，存在若干关键缺口：

**最紧迫的 5 个改进点**：

1. **[P0] SQLite 单写瓶颈**：CI/CD 日志高频写入会在并发场景下触发 `SQLITE_BUSY`，需引入写队列 + WAL 调优。
2. **[P0] JWT Secret 明文存储**：配置文件 TOML 存储 HS256 secret，配置文件泄露即全局 Token 失效，需引入 env 优先加载。
3. **[P1] git CLI fallback 19 处散布**：PR Merge/Diff、Mirror、Fork 等关键路径依赖系统 `git` 命令，无版本固定、无错误分类，需统一封装为 `GitCommandGateway`。
4. **[P1] 前端功能缺口**：包注册表（9 种格式）、看板、时间追踪等后端功能无前端页面，严重影响功能完整性感知。
5. **[P1] 可观测性盲区**：无 Prometheus `/metrics` 端点、`/health` 不含 DB/Git 子项检查、无结构化 tracing span 关联。

---

## 改进优先级矩阵

| 维度 | 问题 | 优先级 | 工作量(人天) | 风险 |
|------|------|--------|-------------|------|
| 安全性 | JWT secret 明文存储 → env 优先 | P0 | 2 | 高 |
| 数据库 | SQLite WAL + 写队列 + 连接池调优 | P0 | 3 | 中 |
| 架构 | git CLI fallback 统一封装 `GitCommandGateway` | P1 | 5 | 中 |
| 前端 | 包注册表/看板/时间追踪 UI 补全 | P1 | 10 | 低 |
| 运维 | Prometheus /metrics + /health DB检查 | P1 | 3 | 低 |
| 安全性 | Rate Limiting 覆盖所有危险端点 | P1 | 2 | 中 |
| API | OpenAPI 注解补全 + 分页格式统一 | P1 | 4 | 低 |
| 架构 | rg-core 拆分（audit/notification/search 独立） | P2 | 6 | 中 |
| 数据库 | 软删除策略统一（user/org/issue 等补充 deleted_at） | P2 | 4 | 中 |
| 数据库 | 迁移文件命名规范统一 | P2 | 1 | 低 |
| CI/CD | 集成测试：PR Merge / SSH push 路径 | P2 | 6 | 低 |
| MCP | Tool 扩展（Issue/PR 写操作 + CI/CD 触发） | P2 | 4 | 低 |
| 性能 | WebSocket 换 per-repo channel | P2 | 3 | 低 |
| 数据库 | 审计日志归档（TTL + 压缩） | P2 | 3 | 低 |
| 运维 | Docker 多阶段构建 Dockerfile + compose | P2 | 2 | 低 |
| 前端 | i18n key 覆盖率 CI 自动检查 | P3 | 2 | 低 |
| 性能 | PostgreSQL 可选后端 | P3 | 15 | 高 |
| CI/CD | Runner Watchdog 间隔 60s → 15s | P3 | 1 | 低 |

---

## 维度 1：架构与模块化

### 当前状态
9 crate 的 workspace 整体分层合理，但 `rg-core` 已演变为超级模块（24 个子模块），部分横切关注点与业务逻辑混杂；`git CLI fallback` 散点分布是当前最显著的架构债务。

### 主要问题

1. **rg-core 职责膨胀**：`audit`、`notification`、`search`、`webhook` 等横切关注点与 `repo`、`user`、`issue` 等业务实体模块共存，24 个模块导致增量编译慢，任何修改都需跨模块搜索依赖。

2. **git CLI fallback 无统一封装**：19 处 `std::process::Command::new("git")` 散布在 `pull_request/service.rs`、`mirror/service.rs`、`import/service.rs`、`repo/service.rs` 中，缺乏统一 PATH 查找与版本校验、错误分类、超时控制、tracing span 注入。

3. **rg-mcp 与 rg-core 循环依赖风险**：rg-mcp 直接依赖 rg-core service impl，crate 边界形同虚设，未来难以独立部署 MCP 服务。

4. **rg-runner 与 rg-ci 边界模糊**：CI pipeline 定义/解析与 Runner 执行环境之间缺少清晰的 `PipelineSpec → ExecutionPlan` 转换层。

### 改进建议

| 优先级 | 建议 |
|--------|------|
| **P1** | **封装 `GitCommandGateway` trait**：在 `rg-git/src/cli_gateway.rs` 统一所有 `git` CLI 调用，提供 `spawn_git(args, repo_path, timeout) -> Result<Output, GitCliError>`，含版本检查缓存、结构化错误枚举、tracing span 注入 |
| **P2** | **从 rg-core 拆出 `rg-notification`（WebSocket+SMTP）和 `rg-search`（FTS5）** |
| **P2** | **rg-http 模块化路由**：每个业务模块暴露 `pub fn router() -> Router<AppState>`，顶层 `.merge()` 合并 |
| **P2** | **明确 rg-mcp 依赖边界**：通过 Service trait 而非直接 impl 操作数据 |
| **P3** | **rg-ci 定义 `PipelineSpec`/`ExecutionPlan` 明确接口**，rg-runner 只消费 `ExecutionPlan` |

---

## 维度 2：数据库与持久化

### 当前状态
SeaORM + SQLite 在小规模场景下工作良好，但高并发 CI/CD 日志写入、混乱的迁移命名、不一致的软删除策略和缺失的审计日志归档，使数据层在生产化路径上存在明显缺口。

### 主要问题

1. **SQLite 单写锁瓶颈**：WAL 模式允许并发读但写操作串行，CI/CD 日志 + 包注册表并发推包时极易出现 `SQLITE_BUSY`。

2. **迁移文件命名不一致**：`m000001_create_users` vs `m20260508_000001_add_audit_log` 两种风格混用，混合命名导致执行顺序不确定。

3. **软删除策略不统一**：只有 `repo` 表有 `deleted_at`，`user`、`org`、`issue`、`package` 等表直接硬删除，导致审计日志 `actor_id` 外键可能指向已删除用户。

4. **审计日志归档策略缺失**：无 TTL、无分区、无归档机制，高活跃仓库半年内可达百万行。

5. **连接池参数未调优**：SQLite 不支持真正连接并发，`max_connections=10` 反而增加锁竞争；建议写池 `max_connections=1` + 只读池 `max_connections=4~8`。

### 改进建议

| 优先级 | 建议 |
|--------|------|
| **P0** | **SQLite WAL + 调优**：初始化时执行 `PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000;`；写连接池 `max_connections=1` |
| **P0** | **CI/CD 日志写入缓冲队列**：日志行先写入 `tokio::sync::mpsc`，单一 consumer task 批量 INSERT（每 100ms 或 100 条刷一次） |
| **P2** | **迁移命名统一**：`m{YYYYMMDD}_{HHMMSS}_{description}`，写入 CONTRIBUTING.md |
| **P2** | **补充关键表软删除**：`user`、`org`、`issue`、`package_version` 添加 `deleted_at`，通过自定义 `SoftDeleteEntity` trait 统一 `WHERE deleted_at IS NULL` |
| **P2** | **审计日志按月归档**：定时任务将 90 天前记录导出为 NDJSON 压缩文件存入 `data/audit-archive/` |

---

## 维度 3：安全性

### 当前状态
认证体系（Argon2 + JWT + TOTP + OAuth2/OIDC + LDAP）完整，但 JWT secret 明文存储、rate limiting 覆盖盲区、LFS URL 签名有效期管理等问题在生产环境下存在实质安全风险。

### 主要问题

1. **JWT HS256 Secret 明文存储**：`config.toml` 存储 secret，配置文件误传 Git / Docker 构建泄露均可导致全局 Token 失效。

2. **Rate Limiting 覆盖盲区**：`POST /login`、`POST /register`、`GET /archive/{sha}.tar.gz`、TOTP 验证端点等高危端点是否已覆盖？

3. **receive-pack 权限控制需验证**：匿名推送 401、分支保护 bypass 403、LFS batch API 权限一致性等场景需集成测试覆盖。

4. **SSH Host Key 管理**：每次重启若重新生成，SSH 客户端会报 `WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!`。

5. **LFS URL 签名有效期**：下载有效期过长（>24h）增加泄露风险；上传有效期过短（<5min）导致大文件上传中途失效。

### 改进建议

| 优先级 | 建议 |
|--------|------|
| **P0** | **JWT Secret 安全化**：优先从 `IRONFORGE_JWT_SECRET` env 读取，降级才读 TOML；secret < 32 字节拒绝启动；支持 `secret_file` 配置项 |
| **P1** | **Rate Limiting 全面覆盖**：使用 `tower_governor` 对 `login/register/totp/oauth-callback` 按 IP 限流（10次/分钟） |
| **P1** | **receive-pack 权限链强化**：集成测试覆盖匿名推送→401、只读协作者推送→403、分支保护绕过→403 |
| **P2** | **SSH Host Key 持久化**：从 `data/ssh/host_key_ed25519` 加载，不存在则生成并保存，文件权限 `0600` |
| **P2** | **LFS URL 签名有效期分级**：下载 1h，上传 6h，超期返回 `410 Gone` |

---

## 维度 4：性能与可扩展性

### 当前状态
单节点架构在小团队场景下性能充足，但 git CLI 调用（pack-objects/index-pack）、WebSocket 全局广播、SQLite 单写等设计约束在中等规模下会成为瓶颈。

### 主要问题

1. **Pack-objects/index-pack 阻塞 async runtime**：CPU 密集型操作若未包裹 `spawn_blocking`，会卡顿整个 tokio runtime。

2. **WebSocket broadcast 扩展性**：全局 `broadcast::Sender<Event>` 频道，100 用户在线时一条 CI 日志触发 100 次无效 clone。

3. **CI/CD 单节点 Runner 限制**：无多 Runner 注册支持，无法水平扩展 job 并发。

4. **大文件 clone 背压控制**：`git pack-objects` 输出管道到 HTTP response 是否有 backpressure 控制？

### 改进建议

| 优先级 | 建议 |
|--------|------|
| **P0** | **确认 SQLite WAL 模式**：检查 `rg-db/src/lib.rs` 连接初始化，若未开启立即补充 |
| **P1** | **Pack 操作移至 `spawn_blocking`**：所有 `git pack-objects/index-pack` 调用包裹在 `tokio::task::spawn_blocking` |
| **P2** | **WebSocket 换 per-repo 频道**：`DashMap<RepoId, broadcast::Sender<RepoEvent>>` + `DashMap<JobId, broadcast::Sender<LogLine>>` |
| **P2** | **Runner 多实例支持**：添加 `runner_registration` 表，按 capacity/labels 分配 job |
| **P3** | **评估 PostgreSQL 迁移路径**：抽象 `DatabaseCapabilities` trait |

---

## 维度 5：前端完整性

### 当前状态
SvelteKit 5 SPA 覆盖了核心 Git 托管功能，但包注册表（9 种格式）、看板、时间追踪等后端功能完全无前端入口，且 SPA 404 fallback 需确认配置。

### 后端有功能但前端无页面的完整清单

| 后端模块 | 无前端页面功能 |
|---------|-------------|
| `packages.rs` | 9 种包注册表浏览/搜索/版本管理 UI |
| `boards.rs` | 看板（Kanban）视图 |
| `time_tracking.rs` | 时间追踪与工时报表 |
| `mirrors.rs` | Mirror 仓库同步状态 UI |
| `runners.rs` | Runner 管理页面 |
| `mfa.rs` | MFA/2FA 设置页面 |
| `imports.rs` | 数据导入向导 |
| MCP Server | MCP 配置入口 |

### 改进建议

| 优先级 | 建议 |
|--------|------|
| **P1** | **补全包注册表浏览 UI**：`web/src/routes/[owner]/[repo]/packages/` 路由，优先 Cargo/npm/Docker |
| **P1** | **补全看板视图**：`web/src/routes/[owner]/[repo]/issues/board` Kanban 列视图，支持拖拽 |
| **P1** | **修复 SPA 404 fallback**：`ServeDir` 添加 `not_found_service` 回退到 `index.html` |
| **P2** | **i18n CI 自动检查**：`web/scripts/check-i18n.ts` 比对 zh/en key 集合，`pnpm check:i18n` 有差异则 CI 失败 |
| **P2** | **补全时间追踪 UI**：`web/src/routes/[owner]/[repo]/time-tracking` 工时列表 + 报表 |

---

## 维度 6：API 设计与兼容性

### 当前状态
142 个端点基本覆盖平台功能，但路径命名规范性、分页格式统一性、GitHub API 兼容度存在可提升空间。

### 主要问题

1. **分页响应格式混用**：`X-Total-Count` header + 数组 body vs `{"data":[],"total":0}` 包装体，混用导致客户端集成困惑。

2. **OpenAPI 注解覆盖率**：Registry 端点、MCP 端点是否在 `#[utoipa::path]` 中注解？

3. **GitHub API 兼容性**：关键端点响应格式差异会导致 `release-drafter`、`semantic-release` 等工具失效。

4. **WebSocket API 无文档**：WS 协议（连接 URL、消息格式、重连策略）需要 AsyncAPI 规范。

### 改进建议

| 优先级 | 建议 |
|--------|------|
| **P1** | **统一分页响应**：`PageResponse<T> { data, total, page, page_size }` + Axum `Pagination` extractor，强制所有列表端点使用 |
| **P1** | **API 路径审计**：提取所有路由，用脚本检查命名规范，整理 `docs/api-naming-conventions.md` |
| **P2** | **补全 OpenAPI 注解**：重点 Registry 系列端点，添加 `cargo test openapi_schema_valid` 自动化测试 |
| **P2** | **GitHub API 兼容层**：`/github/v3/` 路由前缀，优先兼容最常用 10 个端点 |
| **P3** | **AsyncAPI 规范**：`docs/asyncapi.yaml` 描述 WS 消息格式 |

---

## 维度 7：CI/CD 与测试

### 当前状态
功能层面 CI/CD 完整，但集成测试覆盖关键路径存在明显缺口，Runner Watchdog 60s 周期在快速失败场景下响应迟钝。

### 主要问题

1. **集成测试关键缺口**：PR Merge 3 种策略（含冲突路径）、SSH push 全链路、Registry 推包/拉包 9 种格式、OAuth2/OIDC PKCE 流程。

2. **cargo-llvm-cov 覆盖率**：安全关键路径（认证/权限检查）无覆盖率门槛。

3. **Runner Watchdog 60s 周期**：快速 job（<5s）崩溃后最多等 60s 才重调度。

4. **CI YAML 语法支持范围不明确**：`${{ env.VAR }}`、`needs.job.outputs.xxx`、`strategy.matrix` 等 GitHub Actions 特有语法，不支持时应明确报错而非静默忽略。

### 改进建议

| 优先级 | 建议 |
|--------|------|
| **P1** | **PR Merge 策略集成测试**：`tests/integration/pr_merge.rs`，测试 ff/squash/rebase 成功路径和冲突路径 |
| **P2** | **llvm-cov 覆盖率门槛**：`rg-core` 认证/权限路径 ≥75% 行覆盖率，CI enforce |
| **P2** | **Runner Watchdog 自适应**：默认 15s，>30min job 降级 60s；Runner 主动心跳 10s，3 次缺失才重调度 |
| **P2** | **明确 CI YAML 支持子集**：`docs/ci-yaml-reference.md` + 不支持字段输出 `warn!()` |
| **P3** | **`docker-compose.test.yml`** 一键测试环境 |

---

## 维度 8：运维与可观测性

### 当前状态
核心功能完整，但 Prometheus metrics、细化健康检查、Docker 镜像、备份恢复等生产运维基础设施几乎空白。

### 主要问题

1. **无 Prometheus /metrics 端点**：无法监控 QPS、延迟、Git 操作数量、CI job 队列深度等关键指标。

2. **`/health` 检查不完整**：若只返回 `{"status":"ok"}` 不检查 SQLite 可写性、git 命令可执行性、SMTP 连接，Kubernetes healthcheck 无法检测真实服务降级。

3. **Docker 镜像缺失**：无 Dockerfile，部署需要完整 Rust toolchain，门槛极高。

4. **备份/恢复文档缺失**：SQLite 数据库、`data/` 目录（Git 仓库、LFS 对象、Artifact）的备份策略未文档化。

### 改进建议

| 优先级 | 建议 |
|--------|------|
| **P1** | **Prometheus /metrics**：集成 `metrics` + `metrics-exporter-prometheus`，记录 HTTP 请求数/延迟、Git 操作计数、CI 队列深度、WebSocket 连接数、DB 查询延迟 |
| **P1** | **增强 /health**：`{"status":"healthy/degraded/unhealthy", "checks":{"database":"ok","git":"ok","storage":"ok","smtp":"ok/skipped"}}`，DB 检查 `SELECT 1`，git 检查 `git --version` |
| **P2** | **Docker 多阶段构建**：`rust:1.82-slim` builder → `debian:bookworm-slim` runtime（约 50~80MB），`docker-compose.yml` + CI 推送到 GHCR |
| **P2** | **备份/恢复文档**：`docs/backup-restore.md`，含 SQLite 热备份、`data/` rsync、恢复步骤、定时备份脚本 |
| **P2** | **日志轮转**：`tracing_appender::rolling::daily("logs/", "ironforge.log")` + `logrotate` 配置示例 |

---

## 推荐的 Phase 22 规划

### 🥇 Phase 22-A：生产稳定基础（P0 + P1 安全/性能）
**目标**：让 IronForge 可以放心地部署到生产环境  
**工作内容**：
1. SQLite WAL + 连接池调优 + CI 日志写入队列（2天）
2. JWT Secret 环境变量优先加载 + 启动校验（0.5天）
3. Rate Limiting 全面覆盖危险端点（1天）
4. `/health` 增强 + Prometheus `/metrics` 基础指标（2天）
5. Docker 多阶段构建 + `docker-compose.yml`（1天）
6. 备份/恢复文档（0.5天）

**总工作量**：约 7 人天 | **产出**：生产就绪的部署包 + 可监控的服务

---

### 🥈 Phase 22-B：前端功能补全（P1 前端缺口）
**目标**：让已有后端功能对用户可见  
**工作内容**：
1. 包注册表浏览 UI（Cargo/npm/Docker 优先，3个格式）（3天）
2. Issue 看板视图（2天）
3. SPA 404 fallback 修复（0.5天）
4. i18n CI 自动检查脚本（0.5天）

**总工作量**：约 6 人天 | **产出**：核心包注册表和看板功能对最终用户可用

---

### 🥉 Phase 22-C：架构债务清理（P1 技术债）
**目标**：降低长期维护成本  
**工作内容**：
1. `GitCommandGateway` trait 封装（3天）
2. 软删除策略补全（user/org/issue 表）（2天）
3. 迁移文件命名统一（1天）
4. WebSocket per-repo 频道重构（1.5天）

**总工作量**：约 7.5 人天 | **产出**：更健壮的 git CLI 集成 + 更一致的数据层

---

### 4️⃣ Phase 22-D：MCP + API 扩展（P2 生态）
**工作内容**：
1. MCP Tool 扩展：Issue/PR 写操作、CI trigger（3天）
2. GitHub API 兼容层（10个核心端点）（2天）
3. 分页响应格式统一（1天）

**总工作量**：约 6 人天

---

### 5️⃣ Phase 22-E：测试覆盖率提升（P2 质量）
**工作内容**：
1. PR Merge 3 种策略集成测试（2天）
2. SSH push 全链路集成测试（2天）
3. llvm-cov 覆盖率门槛 CI enforce（0.5天）
4. CI YAML 不支持语法明确化（0.5天）

**总工作量**：约 5 人天

---

## 总结

**建议执行顺序**：Phase 22-A（生产稳定）→ Phase 22-B（前端补全）→ Phase 22-C（架构债务），约 **20 人天**可将 IronForge 从「功能完整的原型」升级为「可信赖的生产平台」。

---

*报告由架构师高见远 + 代码扫描联合输出，2026-06-09*
