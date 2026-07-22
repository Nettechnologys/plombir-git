# IronForge 改进与优化分析报告（整合版）

> **整合来源**：
> - 2026-06-09《全方位改进空间分析报告》—— 8 维度改进规划，识别 P0 生产稳定性缺口
> - 2026-06-17《源码优化空间分析报告》—— 代码级优化，确认前述 P0 已解决并落地 16 项优化
>
> **当前代码状态以 `CLAUDE.md`「实现现状」与 `ironforge-docs/architecture/` 系列为准**。本报告为历史演进记录，供判断技术债与后续方向参考。
>
> **最近复核（2026-07-13）**：已按当前源码回填多数据库、OpenAPI、WebSocket、审计归档、SSH Host Key 和日志轮转状态；历史基线数字仍保留原报告日期口径。

---

## 0. 演进时间线

| 日期 | 报告 | 核心结论 |
|------|------|---------|
| 2026-06-09 | 全方位改进空间分析 | 识别 8 维度缺口；最紧迫 P0 = SQLite 单写瓶颈 / JWT secret 明文 / git CLI fallback 19 处（截至当日；2026-07-04 已全部经网关收敛） / 前端功能缺口 / 可观测性盲区 |
| 2026-06-17 | 源码优化空间分析 | 确认上述 P0 **已全部解决**（WAL+写队列 / JWT env 优先 / GitCommandGateway / 前端补全 / /metrics+health）；基于当前代码库新增 16 项优化（13 已修复） |

> 两份报告是同一改进主线的「规划 → 落地」关系，本条合并不删减实质信息，仅消除重复表述。

---

## 1. 执行摘要

IronForge 已完成 21 个迭代阶段，功能完备度达到轻量级 Git 托管平台的生产基准。在从「功能验证」走向「生产稳定」的路径上，报告识别出两层改进：

1. **规划层缺口（2026-06-09）**：8 维度（架构/数据库/安全/性能/前端/API/CI/运维）的系统性缺口，最紧迫 5 项为 P0/P1 生产稳定性问题。
2. **代码层优化（2026-06-17）**：对当前代码库做静态扫描 + 并行 Agent 深度审查，发现 16 个可改进项（P0~P3），其中 13 项已修复、1 项降级、1 项待处理、1 项被上游阻塞。

**当前代码健康度基线（2026-06-17）**：54,404 行 Rust / 9 crate；公共函数文档覆盖率 75%（769/1017）；测试 180 个（3.3/千行，偏低）；生产代码 unwrap/expect ~34 处；编译警告 0；crate 依赖无循环；DB 索引 132 个。

---

## 2. 改进优先级矩阵（合并）

| 维度 | 问题 | 优先级 | 状态（截至 2026-06-17） | 来源 |
|------|------|--------|------------------------|------|
| 安全性 | JWT secret 明文存储 → env 优先 | P0 | ✅ 已解决 | 改进 |
| 数据库 | SQLite WAL + 写队列 + 连接池调优 | P0 | ✅ 已解决（WAL + 写队列） | 改进 |
| 架构 | git CLI fallback 统一封装 `GitCommandGateway` | P1 | ✅ 已解决 | 改进 |
| 前端 | 包注册表/看板/时间追踪 UI 补全 | P1 | ✅ 部分解决（包注册表/看板 UI 已建） | 改进 |
| 运维 | Prometheus /metrics + /health DB 检查 | P1 | ✅ 已解决 | 改进 |
| 安全性 | Rate Limiting 覆盖所有危险端点 | P1 | ✅ 已覆盖（tower_governor） | 改进 |
| API | OpenAPI 注解补全 + 分页格式统一 | P1 | 🔶 部分（核心 REST 已纳入 OpenAPI；Registry/MCP、统一分页和 AsyncAPI 仍待处理） | 改进 |
| 架构 | rg-core 拆分（audit/notification/search 独立） | P2 | 🔶 部分（模块分组文档 + CoreError 基础） | 改进/优化 |
| 数据库 | 软删除策略统一（user/org/issue 等 deleted_at） | P2 | 🔶 部分（关键表已补） | 改进 |
| 数据库 | 迁移文件命名规范统一 | P2 | ✅ 已对齐 | 改进 |
| CI/CD | 集成测试：PR Merge / SSH push 路径 | P2 | ✅ 已覆盖（PR 三策略与冲突恢复；注册公钥后的 SSH push/clone 全链路） | 改进 |
| 安全性 | LFS URL 签名有效期分级 | P2 | ✅ 已完成（下载 1h / 上传 6h，HMAC 用途/仓库/OID 绑定，过期 410） | 改进 |
| MCP | Tool 扩展（Issue/PR 写操作 + CI/CD 触发） | P2 | 📋 待处理 | 改进 |
| 性能 | WebSocket 换细粒度 channel | P2 | ✅ 已完成（通知按用户、Job 日志按 `job_id` 独立 channel；断开后回收） | 改进 |
| 数据库 | 审计日志归档（TTL + 压缩） | P2 | ✅ 已完成（启动接线、可配置 TTL/批次/间隔、唯一 NDJSON.zst 原子落盘后删除 DB） | 改进 |
| 运维 | Docker 多阶段构建 + compose | P2 | ✅ 已解决（Dockerfile + compose） | 改进 |
| 前端 | i18n key 覆盖率 CI 自动检查 | P3 | 📋 待处理 | 改进 |
| 性能 | PostgreSQL/MySQL 可选后端 | P3 | ✅ 已完成首轮实库验证（2026-07-13；HA/恢复/压测另行推进） | 改进/优化 |
| CI/CD | Runner Watchdog 间隔 60s → 15s | P3 | ✅ 已优化（自适应心跳） | 改进 |
| 性能 | 大文件非流式处理 → OOM 风险 | P0 | ✅ 已修复（OCI/LFS 流式化 + 10GiB 限制） | 优化 |
| 性能 | async 中 CPU 密集操作阻塞 runtime | P0 | ✅ 已修复（merge/compute_diff 包 spawn_blocking） | 优化 |
| 数据库 | N+1 查询 | P0 | ✅ 已修复（批量 IN / join_all / batch_insert） | 优化 |
| 可维护性 | main() 668 行 | P1 | ✅ 已修复（提取 run_serve，减至 457） | 优化 |
| 质量 | 测试覆盖不足（历史上 rg-db/rg-ci 等零测试） | P1 | 🔶 部分（当前源码测试函数：rg-db 14、rg-ci 36、rg-runner 3、rg-ssh 5；rg-cli/rg-mcp 仍为 0） | 优化 |
| 性能 | 重复打开仓库 gix::open 4-8 次 | P1 | ✅ 已修复（_with_repo 变体） | 优化 |
| 性能 | 鉴权路径无缓存（每请求 2-4 次 DB 查） | P1 | ✅ 已修复（PermissionCache 30s TTL） | 优化 |
| 架构 | rg-core 25 模块膨胀 | P2 | 🔶 已对齐（分组文档 + //! doc） | 优化 |
| 错误处理 | anyhow 泛滥 148 处 | P2 | 🔶 基础（新增 CoreError 枚举） | 优化 |
| 构建 | 未使用依赖（oauth2/openidconnect） | P2 | ✅ 已修复 | 优化 |
| 配置 | 硬编码配置值 | P2 | ✅ 已修复（external_url / TimeoutConfig） | 优化 |
| 文档 | 公共函数文档覆盖率 75% | P3 | 📋 待处理（248 个待补） | 优化 |
| 前端 | API client 671 行单文件 | P3 | ✅ 已修复（拆分 20 子模块） | 优化 |
| 协议 | gix 迁移余量（Phase 3） | P3 | 🔒 阻塞（被 gix 上游阻塞） | 优化 |

---

## 3. 分维度分析（融合两份发现与建议）

### 3.1 架构与模块化
- **rg-core 职责膨胀**：24→25 子模块，横切关注点（audit/notification/search/webhook）与业务实体混杂；增量编译慢。
  - 建议：P1 封装 `GitCommandGateway` trait（统一 git CLI 调用、版本检查、错误分类、tracing）；P2 拆出 `rg-notification`/`rg-search`；P2 明确 `rg-mcp` 经 Service trait 依赖；P3 定义 `PipelineSpec`/`ExecutionPlan` 解耦 rg-ci/rg-runner。
- **gix !Send 跨 await**：`gix::Repository` 含 `RefCell(!Send)`，async 跨 await 持有会编译报错（非 UB）。HTTP 路由已注释，当前无实际风险。
- **模块膨胀已对齐**：lib.rs 模块分组（Identity/Collaboration/Delivery&CI/Infrastructure）+ 全模块 `//!` 文档；实际 crate 拆分留待后续。

### 3.2 数据库与持久化
- SQLite 单写锁瓶颈（WAL 允许并发读、写串行）→ P0 已通过 `PRAGMA journal_mode=WAL; synchronous=NORMAL; busy_timeout=5000` + 写连接池 `max_connections=1` + CI 日志 mpsc 缓冲队列解决。
- 迁移命名已统一 `m{YYYYMMDD}_{HHMMSS}_{desc}`；软删除关键表已补 `deleted_at`；审计日志已完成默认 90 天、限量批次、唯一 NDJSON.zst 原子归档并接入服务启动。

### 3.3 安全性
- JWT HS256 secret 明文 TOML → ✅ 改为 `IRONFORGE_JWT_SECRET` env 优先，<32 字节拒启动，支持 `secret_file`。
- Rate Limiting 已覆盖 login/register/totp/oauth-callback（tower_governor，10次/分钟）。
- receive-pack 权限链（匿名→401 / 只读协作者→403 / 分支保护绕过→403）已加集成测试。
- SSH Host Key 已按配置路径自动生成并持久化；LFS Batch 现签发 HMAC 用途/仓库/OID/过期时间绑定 URL（下载 1h / 上传 6h），过期返回 410，上传对公有仓库同样要求写权限。

### 3.4 性能与可扩展性
- Pack 操作已移 `spawn_blocking`；通知 WebSocket 已按用户 channel 隔离，Job 日志也已改为按 `job_id` 独立 channel，连接断开后按接收器计数安全回收；Job 订阅在升级前校验仓库读取权限。
- 多 Runner 注册（`runner_registration` 表）待规划；大文件 clone 背压控制需确认。

### 3.5 前端完整性
- 包注册表浏览 UI、看板视图、MFA/2FA 设置、Runner 管理、时间追踪、Mirror 同步状态、数据导入向导、MCP 配置入口等后端功能的前端补全，部分已建（包注册表/看板），其余按 P1/P2 推进。
- SPA 404 fallback 已修复；i18n CI 自动检查 📋 待处理。

### 3.6 API 设计与兼容性
- 分页响应格式混用（`X-Total-Count` vs `{"data":[],"total":0}`）→ 建议统一 `PageResponse<T>` + `Pagination` extractor。
- 核心 REST OpenAPI 注解与文档端点已落地；Registry/MCP 端点覆盖、分页格式统一、GitHub API 兼容层（`/github/v3/`）和 WebSocket AsyncAPI 规范仍 📋 待处理。

### 3.7 CI/CD 与测试
- PR Merge 三策略 + 冲突恢复、SSH push/clone、OIDC discovery + PKCE callback、Package 9 类原生 REST/协议矩阵均已补真实全链路回归。
- llvm-cov 覆盖率门槛（认证/权限路径 ≥75%）待 enforce；Runner Watchdog 已自适应（默认 15s，心跳 10s）；CI YAML 不支持字段已明确 `warn!()`。

### 3.8 运维与可观测性
- Prometheus `/metrics`、增强 `/health`（database/git/storage/smtp 子项）、Docker 多阶段构建 + compose、备份恢复文档均已 ✅ 解决。
- 日志已使用 `tracing_appender::rolling` 按日轮转并限制保留文件数；按大小轮转和外部 logrotate 联动仍 📋 待处理。

---

## 4. 源码优化修复进展（2026-06-17，16 项）

### 4.1 修复总览

| 阶段 | 编号 | 问题 | 状态 |
|------|------|------|------|
| P0 | 1 | 大文件非流式处理 | ✅ 已修复 |
| P0 | 2 | gix !Send 跨 await | ⚠️ 降级（HTTP 路由已注释，无实际风险） |
| P0 | 3 | async 中 CPU 密集操作 | ✅ 已修复 |
| P0 | 4 | N+1 查询 | ✅ 已修复 |
| P1 | 5 | main() 668 行 | ✅ 已修复 |
| P1 | 6 | 测试覆盖不足 | 🔶 基础已补（rg-db/rg-ci/rg-runner/rg-ssh 已有单元测试，rg-cli/rg-mcp 仍缺） |
| P1 | 7 | 重复打开仓库 | ✅ 已修复 |
| P1 | 8 | 鉴权路径无缓存 | ✅ 已修复 |
| P2 | 9 | rg-core 25 模块膨胀 | 🔶 已对齐 |
| P2 | 10 | anyhow 泛滥 | 🔶 基础已建（CoreError） |
| P2 | 11 | 未使用依赖 | ✅ 已修复 |
| P2 | 12 | 硬编码配置值 | ✅ 已修复 |
| P3 | 13 | 文档覆盖率 75% | 📋 待处理 |
| P3 | 14 | 前端 API client 671 行 | ✅ 已修复 |
| P3 | 15 | PostgreSQL/MySQL 支持 | ✅ 已完成首轮实库 migration/CRUD/FTS/并发认证 smoke |
| P3 | 16 | gix 迁移余量 | 🔒 阻塞（被上游阻塞） |

### 4.2 关键修复详情
- **流式化（#1）**：OCI blob 改用 `File + ReaderStream + StreamBody`；LFS 上传 temp 文件 + `zstd` 边读边压；上传路由加 `RequestBodyLimitLayer(10 GiB)`。
- **spawn_blocking（#3）**：`merge_pr`/`compute_diff` 包入 `spawn_blocking`；MCP `block_on` 无需改（同步 stdio）。
- **N+1（#4）**：`find_by_ids` 批量 IN；code_indexer 批量 INSERT（100/批）；LFS batch `join_all` 并发。
- **权限缓存（#8）**：全局 `PermissionCache`（`HashMap<(repo_id,actor_id,for_write),(bool,Instant)>` + `OnceLock<Mutex>`，TTL 30s）。
- **CoreError（#10）**：`NotFound/Forbidden/Conflict/InvalidInput/Internal` 枚举，`From<anyhow::Error>` 渐进迁移。

### 4.3 已确认良好实践
编译警告清零 · crate 无循环依赖 · rg-mcp/rg-runner 经 HTTP API 独立通信 · DB 索引覆盖完整（132）· 列表 API 普遍分页（21 处）· SSH !Send 处理正确 · Git CLI 统一网关 · HTTP 错误统一 `AppError` · 认证提取集中化 · release profile LTO+strip+codegen-units=1。

---

## 5. 代码健康度指标（2026-06-17 基线）

| 指标 | 数值 | 评价 |
|------|------|------|
| 代码总量 | 54,404 行 Rust | 适中 |
| 公共函数文档覆盖率 | 75%（769/1017） | 良好，248 待补 |
| 测试函数 | 180 个（3.3/千行） | **偏低** |
| 生产代码 unwrap/expect | ~34 处 | 良好（多在测试） |
| 编译警告 | 0 | 优秀 |
| TODO/FIXME | 14 个 | 可接受（多为 gix Phase 3） |
| DB 索引 | 132 个 | 覆盖完整 |
| crate 依赖 | 无循环 | 优秀 |

> 这是 2026-06-17 的历史分布。2026-07-13 复核时 rg-db/rg-ci/rg-runner/rg-ssh 已有测试，rg-cli/rg-mcp 仍为零，且协议级全链路覆盖仍不均衡。

---

## 6. 路线图与 Phase 规划

### 6.1 改进 Phase 22 规划（2026-06-09，约 20 人天）
- **A 生产稳定基础**（P0+P1 安全/性能，~7 人天）：WAL+队列、JWT env、Rate Limit、/health+/metrics、Docker、备份文档。
- **B 前端补全**（P1 前端缺口，~6 人天）：包注册表 UI、看板、SPA fallback、i18n 检查。
- **C 架构债务清理**（P1 技术债，~7.5 人天）：GitCommandGateway、软删除补全、迁移命名、WebSocket 重构。
- **D MCP+API 扩展**（P2，~6 人天）：MCP 写操作、GitHub 兼容层、分页统一。
- **E 测试覆盖率**（P2，~5 人天）：PR Merge 三策略/冲突、SSH push/clone、OIDC PKCE 和 Package 9 类格式矩阵已完成；继续推进 llvm-cov 门槛。

### 6.2 源码优化路线图（2026-06-17，五阶段）
- 第一阶段(P0) #1/#3/#4 ✅ → 第二阶段(P1) #5/#7/#8 ✅ → 第三阶段(P1) #6 🔶 → 第四阶段(P2) #9/#10 🔶 → 第五阶段(P2/P3) #13/#15 📋；#16 gix 持续复查。

---

## 7. 总结与当前建议

- **P0 生产稳定性缺口已闭环**：SQLite WAL、JWT env、GitCommandGateway、Prometheus/health、前端核心补全、Docker 化均已完成。
- **剩余主要是 P2/P3 长期项**：软删除统一、数据库版本矩阵/恢复/HA、OpenAPI 余量与覆盖率门禁、文档覆盖率、gix 迁移余量（被上游阻塞）。
- **质量风险点**：rg-cli/rg-mcp 仍无单元测试，认证/权限路径的 llvm-cov 门槛尚未强制；后续优先补 CLI/MCP 边界行为和覆盖率门禁。
- **权威现状源**：上述条目的当前真实状态以 `CLAUDE.md`「实现现状」与 `ironforge-docs/architecture/architecture-followups-2026-07.md` 为准；本报告为历史演进记录，勿直接当作待办清单。

---

*整合自：2026-06-09《全方位改进空间分析报告》+ 2026-06-17《源码优化空间分析报告》，2026-07-08 合并，2026-07-13 复核当前状态。*
