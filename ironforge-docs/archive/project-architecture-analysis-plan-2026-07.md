# IronForge 项目架构重盘分析步骤

**创建日期**: 2026-07-05  
**目标产物**: 全新的项目架构文档 + 前后端结构分布文档  
**适用范围**: 当前代码库实际状态，不以历史 Phase 描述作为唯一事实来源

---

## 一、分析原则

本轮不是继续修补旧版 `ARCHITECTURE.md`，而是重新从代码、配置、迁移、路由、前端页面和运行入口盘点项目现状。历史文档只作为背景材料，最终结论必须能回指到当前代码路径。

核心原则：

1. **代码优先**：以 `crates/`、`web/`、迁移、测试、配置和构建脚本为事实源。
2. **按层拆解**：先整体后局部，先运行链路后模块细节，避免一开始陷入单个功能。
3. **每轮有产出**：每一轮分析都输出结构表、调用关系、风险点或待核验项。
4. **事实与判断分离**：文档中明确区分“代码事实”“架构解读”“建议调整”。
5. **可追溯**：重要结论必须标注文件路径，必要时标注函数、类型或路由。

---

## 二、最终文档拆分

建议产出两份主文档和一份补充清单：

| 文档 | 建议文件名 | 内容边界 |
|------|------------|----------|
| 项目架构总览 | `ironforge-docs/architecture/project-architecture-2026-07.md` | 系统分层、运行入口、服务边界、数据流、核心模块职责、部署与运行模型 |
| 前后端结构分布 | `ironforge-docs/architecture/frontend-backend-structure-2026-07.md` | Rust crates、HTTP API、DB、SvelteKit routes、前端 API client、状态管理、页面与后端能力映射 |
| 架构差异与待办 | `ironforge-docs/architecture/architecture-followups-2026-07.md` | 旧文档与代码差异、命名不一致、缺测区域、技术债、建议后续任务 |

---

## 三、分轮分析步骤

### 第 0 轮：确定分析基线

**目标**：锁定本次分析基于的代码版本、文档来源和目录范围。

**读取范围**：

- `AGENT.md`
- `AGENTS.md`
- `ARCHITECTURE.md`
- `README.md`
- `ironforge-docs/README.md`
- `Cargo.toml`
- `web/package.json`

**操作方法**：

```bash
git status --short
git rev-parse --short HEAD
rg --files -g 'Cargo.toml' -g 'package.json' -g '*.md'
```

**产出**：

- 分析基线说明：日期、分支、commit、工作区是否有未提交变更。
- 文档可信度分级：哪些文档作为事实源，哪些只作为历史背景。
- 代码范围清单：后端 crates、前端目录、部署目录、测试目录。

**完成标准**：

- 明确后续所有结论基于哪个代码状态。
- 不把过时 Phase 说明直接写入最终架构结论。

---

### 第 1 轮：整体系统分层和运行入口

**目标**：建立项目的顶层架构图和进程模型。

**读取范围**：

- `crates/rg-cli/src/main.rs`
- `crates/rg-http/src/lib.rs`
- `crates/rg-ssh/src/lib.rs`
- `crates/rg-runner/src/main.rs`
- `crates/rg-mcp/src/main.rs`
- `Cargo.toml`

**重点问题**：

- 主二进制 `ironforge` 如何启动 HTTP、SSH、迁移、CI、配置和日志？
- 独立二进制有哪些：`ironforge`、`ironforge-runner`、`ironforge-mcp`？
- 哪些能力在同一进程内，哪些是独立进程或外部依赖？
- `AppState` 包含哪些跨模块共享依赖？

**产出**：

- 顶层系统分层图。
- 二进制入口表。
- 后端 crate 职责总览表。
- 运行时依赖表：SQLite/PostgreSQL、Git CLI、Docker、SMTP、TLS、Repo Root。

**完成标准**：

- 能用一张图解释浏览器、Git CLI、Runner、MCP Client 到后端的入口路径。

---

### 第 2 轮：后端 crate 职责和依赖关系

**目标**：盘清 Rust workspace 中各 crate 的职责、边界和依赖方向。

**读取范围**：

- `crates/*/Cargo.toml`
- `crates/*/src/lib.rs`
- `crates/rg-core/src/*/mod.rs`
- `crates/rg-http/src/api/*.rs`
- `crates/rg-db/src/entities/*.rs`
- `crates/rg-db/src/ops/*.rs`

**操作方法**：

```bash
cargo metadata --format-version 1 --no-deps
find crates -maxdepth 3 -type f | sort
rg '^pub mod|^mod ' crates/*/src
```

**重点问题**：

- `rg-core` 是否只承载业务逻辑，还是混有协议/HTTP 逻辑？
- `rg-http` handlers 到 `rg-core` service 的调用边界是否清晰？
- `rg-db` entities、ops、migrations 是否与 core service 对齐？
- `rg-git` 和 Git CLI gateway 当前承担哪些能力？
- `rg-ci` 与 `rg-core::ci` 是否存在职责重叠？

**产出**：

- crate 依赖矩阵。
- crate 职责说明表。
- 模块边界异常清单。
- 后端目录树精简说明。

**完成标准**：

- 能说明每个 crate 的“拥有者职责”和“不该承担的职责”。

---

### 第 3 轮：领域模型、数据库和迁移链路

**目标**：从数据库实体与迁移反推真实领域模型。

**读取范围**：

- `crates/rg-db/src/entities/`
- `crates/rg-db/src/migrations/`
- `crates/rg-db/src/ops/`
- `crates/rg-core/src/*/service.rs`

**操作方法**：

```bash
find crates/rg-db/src/entities -type f | sort
find crates/rg-db/src/migrations -type f | sort
rg 'table_name|create_table|alter_table|create_index|CREATE VIRTUAL TABLE' crates/rg-db/src
```

**重点问题**：

- 当前实体分为哪些领域：用户、仓库、Issue、PR、CI、Package、SSO/MFA、审计、看板、工时、导入等？
- 迁移是否与实体表名完全一致？
- 哪些表是核心业务表，哪些是功能扩展表？
- FTS、软删除、审计、权限这些横切数据如何落库？

**产出**：

- 数据库实体分组表。
- 核心 ER 关系说明。
- 迁移时间线。
- 表名/实体/ops/service 对照表。
- 数据一致性和迁移风险清单。

**完成标准**：

- 能从数据库层解释 IronForge 当前支持的真实业务能力。

---

### 第 4 轮：HTTP API、Git HTTP 和实时通道

**目标**：整理后端对外 HTTP 能力，包括 REST、Git Smart HTTP、OCI、WebSocket、OpenAPI。

**读取范围**：

- `crates/rg-http/src/lib.rs`
- `crates/rg-http/src/api/*.rs`
- `crates/rg-http/src/git_v2.rs`
- `crates/rg-http/src/oci.rs`
- `crates/rg-http/src/ws.rs`
- `crates/rg-http/src/openapi.rs`
- `crates/rg-http/tests/*.rs`

**操作方法**：

```bash
rg '\\.route\\(|Router::|/api/v1|/git/|/ws|/v2/' crates/rg-http/src
rg '#\\[utoipa::path|operation_id|tag =' crates/rg-http/src
```

**重点问题**：

- REST API 以哪些 resource 分组？
- Git HTTP `/git/:owner/:repo/...` 与普通 REST API 如何分离？
- WebSocket 用于哪些实时能力？
- OCI/package registry 的路径是否与 REST API 分离？
- 鉴权、分页、限流、维护模式、安全头在哪些层处理？

**产出**：

- API 分组表。
- 路由前缀图。
- REST handler 到 core service 的映射表。
- WebSocket 通道说明。
- API 测试覆盖矩阵。

**完成标准**：

- 能让后续读者从文档快速定位“某个前端功能对应哪个后端 API 文件”。

---

### 第 5 轮：Git/SSH/协议实现链路

**目标**：单独盘清 Git 服务端协议能力，避免与普通业务 API 混在一起。

**读取范围**：

- `crates/rg-git/src/`
- `crates/rg-ssh/src/lib.rs`
- `crates/rg-http/src/git_v2.rs`
- `docs/git-protocol.md`

**操作方法**：

```bash
find crates/rg-git/src -type f | sort
rg 'upload-pack|receive-pack|sideband|pkt|Protocol V2|GitCommandGateway|Command::new' crates/rg-git crates/rg-http crates/rg-ssh crates/rg-core
```

**重点问题**：

- upload-pack / receive-pack 的 SSH 与 HTTP 入口如何复用协议层？
- Protocol V1/V2 分工是什么？
- 哪些 Git 操作已经迁移到 gix，哪些仍通过 `GitCommandGateway` 调用系统 Git？
- 权限鉴权在哪一层发生？
- 已知协议坑点是否仍被代码约束保护？

**产出**：

- Git clone/push 时序图。
- Git 协议模块职责表。
- gix 与 Git CLI fallback 清单。
- SSH/HTTP 生命周期和鉴权说明。

**完成标准**：

- 能独立解释 Git CLI 访问 IronForge 时穿过哪些模块。

---

### 第 6 轮：前端结构、路由和状态管理

**目标**：整理 SvelteKit 前端实际页面结构、组件分布、API client 和状态模型。

**读取范围**：

- `web/src/routes/`
- `web/src/lib/api/`
- `web/src/lib/components/`
- `web/src/lib/stores/`
- `web/src/lib/i18n/`
- `web/src/lib/utils/`
- `web/package.json`
- `web/svelte.config.js`
- `web/vite.config.ts`

**操作方法**：

```bash
find web/src/routes -type f | sort
find web/src/lib -maxdepth 3 -type f | sort
rg 'fetch\\(|api\\.|auth|localStorage|websocket|EventSource|goto\\(' web/src
```

**重点问题**：

- 当前页面路由覆盖哪些业务域？
- API client 是否按后端 resource 分组？
- 登录态、实例信息、i18n 如何管理？
- 共享组件有哪些，是否存在页面内重复实现？
- 前端是否覆盖后端已有功能，哪些后端功能没有 UI？

**产出**：

- 前端路由树。
- 页面到 API client 映射表。
- 组件职责表。
- Store 和全局状态说明。
- 后端能力与前端页面覆盖矩阵。

**完成标准**：

- 能从文档判断“新增一个功能页面应该放在哪里、调用哪个 client、复用哪些组件”。

---

### 第 7 轮：横切能力和安全模型

**目标**：整理跨模块能力，尤其是认证、授权、安全、配置、日志、审计、错误处理。

**读取范围**：

- `crates/rg-core/src/auth/`
- `crates/rg-core/src/audit/`
- `crates/rg-core/src/repo/service.rs`
- `crates/rg-http/src/middleware.rs`
- `crates/rg-http/src/security.rs`
- `crates/rg-http/src/rate_limit.rs`
- `crates/rg-http/src/error.rs`
- `crates/rg-core/src/error.rs`
- `crates/rg-cli/src/main.rs`

**重点问题**：

- 用户认证方式有哪些：JWT、PAT、SSH key、SSO、MFA、Runner Token？
- 权限判断集中在哪里，是否存在绕过路径？
- 错误如何从 core/db/git 映射到 HTTP 响应？
- 配置来源优先级是什么？
- 审计日志覆盖哪些动作？
- 限流、维护模式、安全头、Request ID 如何进入请求链路？

**产出**：

- 认证方式矩阵。
- 权限模型说明。
- 请求中间件链路图。
- 配置项分组表。
- 审计与日志说明。

**完成标准**：

- 能说明一个写操作从请求进入到落库之间经历哪些安全检查。

---

### 第 8 轮：CI/CD、Runner、Package、MCP 和扩展能力

**目标**：整理非核心仓库托管能力，明确它们与主系统的连接点。

**读取范围**：

- `crates/rg-ci/src/`
- `crates/rg-runner/src/`
- `crates/rg-core/src/package_registry/`
- `crates/rg-http/src/api/packages.rs`
- `crates/rg-http/src/oci.rs`
- `crates/rg-mcp/src/`
- `crates/rg-core/src/import/`
- `crates/rg-core/src/mirror/`

**重点问题**：

- CI pipeline 如何从 repo 事件触发、调度、执行、写日志和产物？
- 外部 Runner 与主服务如何认证和通信？
- Package Registry 支持哪些协议，公共路径如何划分？
- MCP server 暴露哪些 tools/resources，调用主服务还是直接读库？
- Mirror/import 等后台任务是否已有调度模型？

**产出**：

- 扩展能力总表。
- CI/Runner 时序图。
- Package Registry 协议映射表。
- MCP 能力表。
- 后台任务和异步执行模型说明。

**完成标准**：

- 能把“仓库托管核心”与“平台扩展能力”分开讲清楚。

---

### 第 9 轮：测试、构建、部署和运维结构

**目标**：盘点项目如何被验证、构建、部署和运行。

**读取范围**：

- `crates/rg-http/tests/`
- `.github/` 或其他 CI 配置
- `deploy/`
- `Dockerfile`
- `docker-compose*`
- `ironforge.example.toml`
- `web/README.md`

**操作方法**：

```bash
find . -maxdepth 3 -type f \\( -name 'Dockerfile' -o -name 'docker-compose*' -o -name '*.toml' -o -name '*.yml' -o -name '*.yaml' \\) | sort
find crates/rg-http/tests -type f | sort
```

**重点问题**：

- 现有测试主要覆盖哪些 API 和回归场景？
- release 构建、前端构建、静态资源服务如何衔接？
- 配置文件与 CLI 参数如何组合？
- 部署形态有哪些：本地单机、Docker、外部 Runner、TLS？

**产出**：

- 测试覆盖表。
- 构建产物说明。
- 部署拓扑说明。
- 运维配置清单。

**完成标准**：

- 能从文档复现开发、测试、构建、运行的基本链路。

---

### 第 10 轮：汇总差异、风险和最终文档

**目标**：把前面各轮分析收束成正式架构文档。

**输入**：

- 前置分析轮次产出
- 旧版 `ARCHITECTURE.md`
- `AGENTS.md` 中实现现状
- 当前代码路径引用

**产出**：

- `project-architecture-2026-07.md`
- `frontend-backend-structure-2026-07.md`
- `architecture-followups-2026-07.md`

**最终检查**：

- 所有核心模块都有路径引用。
- 前端页面与后端 API 映射没有明显空洞。
- 旧文档中的过时说法已标记或修正。
- 架构图与实际入口、路由、crate 职责一致。
- 后续待办按严重程度和影响范围排序。

---

## 四、每轮分析记录模板

后续每轮建议按以下模板记录，便于最后合并：

```markdown
## 第 N 轮：标题

### 读取文件

- `path/to/file.rs`
- `path/to/file.svelte`

### 代码事实

| 主题 | 事实 | 证据 |
|------|------|------|
| 示例 | 示例事实 | `path/to/file.rs` |

### 架构解读

说明这些事实代表什么模块边界、数据流或运行模型。

### 待核验项

| 问题 | 原因 | 下一步 |
|------|------|--------|

### 可进入最终文档的内容

整理后的段落、表格或图。
```

---

## 五、建议执行顺序

优先顺序如下：

1. 第 0-1 轮：建立可信基线和全局图。
2. 第 2-4 轮：完成后端主体结构、数据模型和 HTTP 面。
3. 第 5 轮：单独处理 Git/SSH 协议，因为它是 IronForge 与普通 Web 系统最大的差异。
4. 第 6 轮：整理前端结构，并和第 4 轮 API 映射。
5. 第 7-9 轮：补齐横切能力、扩展能力、运维和测试。
6. 第 10 轮：统一口径，生成最终文档。

---

## 六、当前已知注意事项

- `AGENTS.md` 和 `AGENT.md` 中的阶段说明非常有价值，但可能滞后于代码，需要逐项核验。
- `ARCHITECTURE.md` 更像设计文档，不一定完全反映多轮迭代后的实际结构。
- `ironforge-docs/` 中已有分析报告可复用判断框架，但新架构文档必须重新基于代码确认。
- 前端和后端能力可能不完全一致，需要单独输出“后端已实现但前端未覆盖”的矩阵。
- Git 协议、Package Registry、CI Runner、MCP 是系统差异化能力，最终文档中应独立成章。
