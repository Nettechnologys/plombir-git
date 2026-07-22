# IronForge 对齐 Gitea 1.26.4 进度台账

> 建立日期：2026-07-13
> 最近更新：2026-07-14
> 对比基线：Gitea 1.26.4、IronForge `39e33a9` 之后的当前工作区
> 任务定义：[gitea-detail-alignment-plan-2026-07-13.md](./gitea-detail-alignment-plan-2026-07-13.md)
> 本文职责：只维护任务状态、证据、当前工作和变更记录；范围、依赖、估算与验收门仍以任务定义文档为准。

## 1. 状态定义

| 状态 | 含义 | 进入条件 | 退出条件 |
|---|---|---|---|
| `DONE` | 已完成 | 代码、测试、文档和兼容性验收全部通过 | 发现回归时退回 `DOING` 或 `BLOCKED` |
| `DOING` | 修改中 | 已开始产生代码、测试或对齐矩阵成果 | 达到 DoD 后转 `DONE`；遇到外部阻塞转 `BLOCKED` |
| `READY` | 可开始 | 范围、依赖和验收标准明确 | 开始实施后转 `DOING` |
| `TODO` | 待开始 | 已进入计划但尚未满足开工条件 | 依赖和验收标准明确后转 `READY` |
| `BLOCKED` | 已阻塞 | 存在明确外部依赖、技术限制或待决策事项 | 阻塞解除后转 `READY` 或 `DOING` |

状态约束：

- 同一时间原则上只保留一个主任务为 `DOING`；
- `DONE` 必须同时附代码证据、测试证据和验收说明，不能只凭接口、表或页面存在；
- 纯文档任务至少需要文档链接、审阅结论和后续任务入口；
- 状态变化必须追加到本文末尾的变更记录；
- 规划文档与本文状态冲突时，以本文的“任务状态总表”为当前状态事实源。

## 2. 当前快照

| 指标 | 数量 | 说明 |
|---|---:|---|
| 总任务 | 54 | 来自细节功能计划 M0–M5 |
| `DONE` | 9 | `ALIGN-001`、`GIT-001`、`GIT-002`、`GIT-003`、`GIT-004`、`CI-200`、`STORAGE-001`、`ISSUE-101`、`ISSUE-103` |
| `DOING` | 0 | 当前没有修改中的任务 |
| `READY` | 1 | `ISSUE-105` |
| `TODO` | 44 | 等待依赖、验收标准或排期 |
| `BLOCKED` | 0 | 当前没有确认的外部阻塞 |

当前没有 `DOING` 任务。

下一任务：`ISSUE-105` — 多 Assignee 数据模型、API、筛选和 UI（`READY`）。

## 3. 已完成的前置基础能力

以下能力已在本轮细节对齐计划前完成，用于说明当前起点；它们不计入上述 54 个增量任务的 `DONE` 数量。

| 基础项 | 状态 | 已完成范围 | 代码/测试证据 |
|---|---|---|---|
| 多数据库首轮运行兼容 | `DONE` | SQLite、PostgreSQL、MySQL migration、CRUD、计数器、FTS、认证并发与服务 `/health` smoke | `crates/rg-core/tests/multi_backend_smoke.rs`、`.github/workflows/regression.yml` |
| Gitea Actions 有限兼容基础 | `DONE` | YAML 转换、Secrets、Variables、Matrix、Cache、本地 Reusable Workflow、Environment 审批、OIDC；不支持语义 fail closed | `crates/rg-ci/src/gitea_actions.rs`、`crates/rg-ci/src/condition.rs`、`crates/rg-http/tests/ci_secrets_tag_protection_tests.rs` |
| Pull Mirror | `DONE` | Pull mirror 创建、更新、删除、定时/手动同步和设置页 | `crates/rg-core/src/mirror/service.rs`、`crates/rg-http/src/api/mirrors.rs`、`web/src/routes/[owner]/[repo]/settings/mirror/+page.svelte` |
| HTTP 仓库归档下载 | `DONE` | 按 ref 下载 zip/tar.gz；不等同于 `git archive --remote` / upload-archive | `crates/rg-http/src/api/archive.rs` |
| LDAP/OIDC/MFA 登录闭环 | `DONE` | LDAP 登录、OIDC Discovery/PKCE、MFA challenge、登录审计和管理员解锁 | `crates/rg-core/src/auth/`、`crates/rg-http/src/api/sso.rs`、`crates/rg-http/src/api/mfa.rs`、`crates/rg-http/tests/admin_sso_audit_tests.rs` |

## 4. 本轮执行结果

### ALIGN-001 — 代码可验证的 Gitea 对齐矩阵

状态：`DONE`

完成情况：

- [x] 固定 Gitea 1.26.4 和 IronForge 当前工作区基线；
- [x] 建立独立进度台账和统一状态定义；
- [x] 确认 Protocol V2 `fetch=shallow` 存在“已广告但未执行”问题；
- [x] 从旧 `gitea-gap-list.csv` 迁移仍然有效的功能条目；
- [x] 为 67 项能力补齐 Gitea 行为、IronForge 行为、代码证据和测试证据；
- [x] 用 100 分加权矩阵统一旧报告的 85% 与新计划口径，当前为 70.5%；
- [x] 将 Arch、Chef、CRAN 补入 `PKG-306`；
- [x] 按原子任务合计修正里程碑串行估算和模糊依赖；
- [x] 重建 `gitea-gap-list.csv`，旧状态由 Git 历史追溯；
- [x] 完成审阅并执行 `GIT-001`。

当前证据：

- 评分口径：`ironforge-docs/comparison/gitea-alignment-matrix-2026-07-13.md`；
- 机器可读矩阵：`ironforge-docs/comparison/gitea-gap-list.csv`；
- 67 项权重合计 100，得分 70.45，展示值 70.5%。

### GIT-001 — Protocol V2 capability 真实性

状态：`DONE`

完成情况：

- [x] `rg-git`、`rg-http` 的 HTTP/SSH 广告共用 `ADVERTISED_CAPABILITIES`；
- [x] 将 `fetch=shallow` 改为只广告已支持的 `fetch`；
- [x] shallow/deepen/filter 显式请求 fail closed，不再静默忽略；
- [x] 增加 rg-git 与 rg-http capability 单元测试；
- [x] `cargo test -p rg-git protocol::v2 --lib`：3 passed；
- [x] `cargo test -p rg-http git_v2 --lib`：1 passed；
- [x] `cargo build --release`：通过。

后续状态：当时登记的 shallow/deepen 与 filter 限制已分别由 `GIT-004`、`GIT-002` 完成，并从失败/回退断言升级为正向验收。

### GIT-003 — Git 真实客户端兼容矩阵

状态：`DONE`

完成情况：

- [x] 新增 `scripts/git-protocol-e2e.sh`，启动临时 IronForge 并自动创建私有仓库与 SSH 凭证；
- [x] 覆盖 HTTP/SSH、V1/V2 的 clone/fetch，覆盖 HTTP/SSH V1 receive-pack push；
- [x] 用 packet trace 断言 V1/V2 实际协商版本，而非只验证命令退出码；
- [x] 首次固化 shallow/deepen 的失败语义和 partial-clone 的客户端回退语义；前者随后由 `GIT-004` 升级为正向验收；
- [x] 首次运行发现并修复 V2 fetch acknowledgments/ready section 顺序错误；
- [x] 新增 section 顺序单元测试，`cargo test -p rg-git protocol::v2 --lib`：6 passed；
- [x] 10 个真实客户端场景本地通过；
- [x] 新增 `git-protocol` CI job 和测试说明文档。

当前证据：

- 自动化：`scripts/git-protocol-e2e.sh`；
- CI：`.github/workflows/regression.yml` 的 `git-protocol` job；
- 说明：`ironforge-docs/testing/git-protocol-client-matrix-2026-07.md`；
- 评分：Git 协议 9.75/12，总分 72.45/100，展示值 72.5%。

已知限制：GIT 协议剩余计划项为 `PERF-001` 大仓库性能基线；shallow/deepen 与 partial-clone filter 已升级为正向断言。

### GIT-004 — Protocol V2 shallow/deepen

状态：`DONE`

完成情况：

- [x] `fetch=shallow` 仅在完整实现后重新进入 HTTP/SSH 统一 capability 列表；
- [x] 解析 `shallow`、`deepen`、`deepen-relative`、`deepen-since` 和 `deepen-not`；
- [x] 计算新旧 shallow boundary，并按 Protocol V2 `shallow-info` section 发送 `shallow`/`unshallow`；
- [x] pack 生成按新 boundary 截断，deepen 时不把浅客户端的 `have` 错当成完整历史；
- [x] HTTP V2 验证 depth=1、deepen=2、unshallow、shallow-exclude 和 shallow-since；
- [x] SSH V2 验证 depth=2；
- [x] 真实客户端矩阵由 10 场景扩展为 12 场景并全部通过；
- [x] `cargo test -p rg-git protocol::v2 --lib`：7 passed。

当前证据：

- 实现：`crates/rg-git/src/protocol/v2.rs`；
- 自动化：`scripts/git-protocol-e2e.sh`；
- 说明：`ironforge-docs/testing/git-protocol-client-matrix-2026-07.md`；
- 评分：Git 协议 10.5/12，总分 73.20/100，展示值 73.2%。

已知限制：当前边界计算需要遍历请求起点的提交图，超大历史的耗时与内存阈值由 `PERF-001` 建立；partial-clone filter 不在本任务范围。

### GIT-002 — Protocol V2 partial clone/filter

状态：`DONE`

完成情况：

- [x] capability 更新为 `fetch=shallow filter`，HTTP/SSH 继续共用同一广告源；
- [x] filter-spec 经长度和控制字符校验后传入 `git pack-objects --filter`；
- [x] HTTP V2 `blob:none --no-checkout` 初始 pack 存在缺失 blob，promisor 配置正确；
- [x] HTTP checkout trace 请求目标 blob 并恢复工作树文件；
- [x] SSH V2 `tree:0 --no-checkout` 初始 pack 存在缺失 tree/blob；
- [x] SSH checkout 经持久化身份配置触发 promisor fetch，根 tree 与工作树文件可用；
- [x] 12 场景真实客户端矩阵全部通过；
- [x] `rg-git` 7 个、`rg-http` 1 个目标测试通过。

当前证据：

- 实现：`crates/rg-git/src/protocol/v2.rs`；
- 自动化：`scripts/git-protocol-e2e.sh`；
- CI：`.github/workflows/regression.yml` 的 `git-protocol` job；
- 评分：Git 协议 11.25/12，总分 73.95/100，展示值 74.0%。

已知限制：尚未建立大仓库下各 filter-spec 的传输量、CPU 和延迟阈值，由 `PERF-001` 跟踪。

### CI-200 — 第三方 Actions 执行与安全边界 ADR

状态：`DONE`

完成情况：

- [x] 审计当前 Actions 转换器、内置 Runner、外部 Runner、网络、Secret 和 Token 行为；
- [x] 对照 Gitea 1.26 的 `act_runner`、容器、host mode、fork 审批和 Token 权限模型；
- [x] 冻结第三方 Action 只在具备 `actions-v1` 能力的专用外部 Runner 执行；
- [x] 冻结每 Job 临时容器、默认断网、禁止 Docker socket/privileged、资源限制和 capability 匹配要求；
- [x] 冻结 Action 来源 allowlist、mutable ref 解析为 commit SHA、缓存隔离与审计字段；
- [x] 冻结只注入被引用 Secret、fork PR 审批/无 Secret/只读 Token，以及 `pull_request_target` fail closed；
- [x] 明确首期 composite/Node20、后续 Docker Action 的兼容顺序和 `CI-202`～`CI-208` 实施关卡；
- [x] 明确在隔离执行器和真实兼容回归完成前，第三方 `uses:` 继续显式拒绝。

当前证据：

- 决策文档：`ironforge-docs/ci/adr-0001-third-party-actions-execution.md`；
- 当前实现审计：`crates/rg-ci/src/gitea_actions.rs`、`crates/rg-ci/src/runner.rs`、`crates/rg-runner/src/main.rs`、`crates/rg-http/src/api/runners.rs`；
- Gitea 参考：Actions Overview、Design、FAQ、Comparison 和 Token permissions 官方文档；
- 评分：本任务只完成设计门，不把尚未实现的第三方 Action runtime 计分，总分保持 73.95/100，展示值 74.0%。

后续入口：`CI-202`/`CI-203` 结构化 step 与运行时语义，`CI-204` 沙箱和网络，`CI-206` Action runtime，`CI-208` 固定版本兼容矩阵。

### STORAGE-001 — 统一 BlobStorage trait

状态：`DONE`

完成情况：

- [x] 新增 object-safe `BlobStorage`、稳定 `BlobKey`、metadata、exists、delete 和前缀 inventory；
- [x] 本地 backend 使用同目录临时文件、`flush + sync_all + rename` 原子发布；
- [x] 拒绝绝对路径、`.`/`..`、隐藏段、控制字符、反斜杠和 symlink 越界；用户段 percent encode；
- [x] Package 与 CI Artifact 的新 DB 记录保存 backend-neutral key，并兼容历史绝对路径；
- [x] LFS 保留流式 zstd，大对象经 `put_file` 发布；新布局与 legacy 布局均可下载；
- [x] OCI chunk upload 在 digest 校验后发布，Blob/Manifest 使用统一 backend；自定义旧 OCI root 可读；
- [x] Release Asset 新写入迁移到 BlobStorage，并保留旧目录读取/删除；
- [x] 冻结 Attachment/Archive Cache 预留 namespace、DB/Blob 补偿流程、Retention、备份和迁移边界；
- [x] 明确 S3/MinIO、签名 URL、在线迁移和一致性 repair 属于 `STORAGE-002`。

当前证据：

- 核心实现：`crates/rg-core/src/blob_storage.rs`；
- 消费者：`crates/rg-core/src/lfs/service.rs`、`package_registry/storage.rs`、`package_registry/oci/storage.rs`、`release/service.rs`、`crates/rg-http/src/api/artifacts.rs`；
- 契约文档：`ironforge-docs/storage/blob-storage-contract-2026-07.md`；
- 测试：BlobStorage 3 项、PackageStorage 2 项、OCI storage 1 项、Artifact 1 项、LFS 3 项、Package format/权限 3 项、Release 6 项全部通过；
- 构建：`cargo check --workspace --all-targets` 通过；
- 评分：本地存储与对象存储基础共增加 0.5 分，总分 74.45/100，展示值 74.5%。

已知限制：远程 backend、签名直传/直下、legacy 批量迁移、DB 双向一致性扫描/repair 和 OCI upload 过期清理由 `STORAGE-002` 跟踪；CI Cache 是可再生数据，继续使用独立 retention 路径。

### ISSUE-101 — Markdown Issue/PR Template

状态：`DONE`

已完成：

- [x] 按 Gitea 1.26.4 对齐 8 个 Issue Markdown 目录、`.gitea`/`.github` chooser config 和 6 个 PR Markdown 候选路径；
- [x] 仅从默认分支读取直属 Markdown 文件，限制 1 MiB/UTF-8，空仓库安全返回；
- [x] 解析 `name`、`title`、`about`/`description`、`labels`、`assignees`、`ref` front matter；
- [x] 新增模板列表、配置读取/校验、PR 模板 REST API 和 OpenAPI 路径；
- [x] 公开/私有仓库统一读权限，匿名私有仓库 `401`、无权限用户 `403`；
- [x] Issue Web 模板选择、空白入口、联系链接、标题/正文/标签预填；PR 创建正文预填且不覆盖用户输入；
- [x] 中英文 i18n、兼容文档和 `CLAUDE.md` 当前事实已同步；
- [x] 核心 4 项测试、HTTP 2 项真实仓库集成测试、workspace check、前端 check/build 通过；
- [x] Playwright 真实浏览器验证 1440×900 Issue chooser/预填、PR 预填和 390×844 Issue chooser；页面身份、非空白、无 framework overlay、交互状态和截图均通过；
- [x] 未发现 page error 或模板相关失败请求；仓库头既有 `/starred`、`/watch` 401 以及登录前 `/users/me` 401 已单独解释，不属于本任务回归。

评分：Markdown Template 完成获得组合矩阵行的 0.5 分；YAML Issue Form 仍由 `ISSUE-102` 跟踪。总分 74.95/100，展示值 75.0%。

### ISSUE-103 — Issue/PR/评论附件

状态：`DONE`

已完成：

- [x] 新增 `attachments` 迁移和实体，显式覆盖 Issue、PR、Issue Comment、Review Comment 四类归属；
- [x] 使用 `attachments/{repo_id}/{uuid}/{filename}` Blob key，并实现上传/删除 DB-Blob 补偿；
- [x] 100 MiB 单文件限制、Gitea 默认扩展名白名单、1 GiB 仓库配额、文件名与 Content-Type 防注入；
- [x] Issue/评论 Gitea 兼容 multipart API，以及独立 PR/Review Comment API；
- [x] 目标作者或仓库写入者可上传/删除，跨仓库和错误目标组合返回 404；
- [x] Issue、PR、Issue Comment、Review Comment 统一 Web 附件面板和中英文文案；
- [x] 16 个集合/单附件路由进入 OpenAPI；
- [x] 上传改为 multipart chunk → 临时文件 → `BlobStorage::put_file`，本地下载使用流式响应，删除补偿使用临时文件备份/恢复，避免 100 MiB 文件整块驻留内存；
- [x] HTTP 回归扩展为 2 项，覆盖 PR、Review Comment、私有仓库 401/403、错误目标 404 与无大文件写入的配额边界；
- [x] 附件 DDL 在 SQLite/PostgreSQL/MySQL query builder 上生成通过，fresh SQLite 由 HTTP 测试实际迁移；实库由 CI service-container smoke 持续验证；
- [x] 修复生产 SPA CSP 重复 nonce 导致 Chromium 空白页，并补精确单 nonce 单元测试；
- [x] 修复附件下载被 SvelteKit 路由二次拦截，以及 star/watch 五个端点仅 Bearer、不认 HttpOnly Cookie 的控制台 401；
- [x] Playwright 在 1440×1000、390×844 完成 Issue/Issue Comment/PR/Review Comment 渲染，Issue/PR 上传、下载内容校验、删除；无控制台错误、5xx 或横向溢出；
- [x] `cargo check --workspace --all-targets`、release 构建、前端 check/build、核心/HTTP/鉴权/迁移目标测试通过。

评分：附件完成获得组合矩阵行的 0.4 分；Reaction 仍由 `ISSUE-104` 跟踪。总分 75.35/100，展示值 75.4%。

## 5. 任务状态总表

### M0：基线与 Git 协议正确性

| ID | 状态 | 任务 | 当前说明 |
|---|---|---|---|
| ALIGN-001 | `DONE` | 建立代码可验证的 Gitea 对齐矩阵 | 67 项、100 分制矩阵完成；GIT-002 后为 74.0% |
| GIT-001 | `DONE` | 停止错误 capability 广告并 fail closed | HTTP/SSH 共用 capability 列表，目标测试通过 |
| GIT-002 | `DONE` | Partial clone/filter | HTTP blob:none、SSH tree:0 与 lazy fetch 验收通过 |
| GIT-003 | `DONE` | Git 真实客户端兼容矩阵 | 10 场景脚本、CI、文档与 V2 section 修复完成 |
| GIT-004 | `DONE` | 完整 shallow/deepen | 12 场景真实客户端验收通过并重新广告 |
| PERF-001 | `TODO` | 大仓库性能基线 | 依赖 GIT-003 |

### M1：仓库与 Issue 高频体验

| ID | 状态 | 任务 | 当前说明 |
|---|---|---|---|
| REPO-101 | `TODO` | Push Mirror | 已有 Pull Mirror；需新增多目标 Push Mirror |
| REPO-102 | `TODO` | upload-archive / `git archive --remote` | 现有 HTTP archive API 不等同于该协议 |
| REPO-103 | `TODO` | Template Repository | 未开始 |
| REPO-104 | `TODO` | Blame、Go to file、目录删除 | 未开始 |
| REPO-105 | `TODO` | 自动生成 Release Notes | 未开始 |
| REPO-106 | `TODO` | 仓库内 OpenAPI 安全渲染 | 未开始 |
| ISSUE-101 | `DONE` | Markdown Issue/PR Template | 默认分支发现、API/权限、Issue/PR 预填和桌面/移动浏览器 E2E 通过 |
| ISSUE-102 | `TODO` | YAML Issue Form | 依赖 ISSUE-101 |
| ISSUE-103 | `DONE` | Issue/PR/评论附件 | 四类目标、流式 Blob、权限/配额、Web/OpenAPI、HTTP 与桌面/移动浏览器闭环通过 |
| ISSUE-104 | `TODO` | Reaction | 未开始 |
| ISSUE-105 | `READY` | 多 Assignee | 下一任务；当前仍为单个 `assignee_id` |
| ISSUE-106 | `TODO` | Lock/Pin | 未开始 |
| ISSUE-107 | `TODO` | Issue 依赖与自动引用 | 未开始 |

### M2：CI 与 Gitea Actions 兼容

| ID | 状态 | 任务 | 当前说明 |
|---|---|---|---|
| CI-200 | `DONE` | 第三方 Action 执行与安全边界 ADR | Accepted；第三方 `uses:` 在 runtime 落地前继续 fail closed |
| CI-201 | `TODO` | 仅重跑失败 Job | 依赖 CI-200 |
| CI-202 | `TODO` | Step/Job outputs | 依赖 CI-200 |
| CI-203 | `TODO` | 运行时条件函数 | 当前 `always()`、`failure()` 被显式拒绝 |
| CI-204 | `TODO` | Service Containers | 依赖 CI-200 |
| CI-205 | `TODO` | Reusable Workflow 扩展 | 依赖 CI-202 |
| CI-206 | `TODO` | 第三方 `uses:` Action 执行器 | 依赖 CI-200、CI-204 |
| CI-207 | `TODO` | Workflow 依赖图和失败定位 UI | 依赖 CI-201 |
| CI-208 | `TODO` | Actions 兼容测试集 | 依赖 CI-201～CI-206 |

### M3：Package Registry 与 API 生态

| ID | 状态 | 任务 | 当前说明 |
|---|---|---|---|
| PKG-301 | `TODO` | Package cleanup、保留和配额 | 与 STORAGE-001 协同设计 |
| PKG-302 | `TODO` | Terraform State Registry | 未开始 |
| PKG-303 | `TODO` | Go Package Registry | 未开始 |
| PKG-304 | `TODO` | Alpine/Debian/RPM | 未开始 |
| PKG-305 | `TODO` | Conan/Conda/Pub/Swift/Vagrant | 未开始；长尾协议需按需求排序 |
| PKG-306 | `TODO` | Arch/Chef/CRAN | ALIGN-001 补入的 Gitea 1.26.4 遗漏项 |
| API-301 | `TODO` | Gitea OpenAPI 差异和 contract test | 依赖 ALIGN-001 |
| API-302 | `TODO` | 高频兼容端点 | 依赖 API-301 |
| API-303 | `TODO` | 响应、分页、错误码和 Token scope 兼容 | 依赖 API-301 |
| HOOK-301 | `TODO` | 用户/组织/系统 Webhook | 依赖 API-301 |

### M4：身份、权限与管理细节

| ID | 状态 | 任务 | 当前说明 |
|---|---|---|---|
| AUTH-401 | `TODO` | LDAP 周期同步和组织/团队映射 | 基于现有 LDAP 登录实现扩展 |
| AUTH-402 | `TODO` | Reverse Proxy Header Authentication | 未开始 |
| AUTH-403 | `TODO` | PAM/SMTP Authentication Source | 需先确认部署需求 |
| USER-401 | `TODO` | 用户 Block/Unblock | 未开始 |
| USER-402 | `TODO` | Follow/Unfollow | 未开始 |
| USER-403 | `TODO` | User Badge | 未开始 |
| PERM-401 | `TODO` | 仓库 Unit 级权限 | 未开始 |
| ADMIN-401 | `TODO` | 系统配置、任务和队列管理页 | 依赖 QUEUE-001 |

### M5：生产化与可恢复性

| ID | 状态 | 任务 | 当前说明 |
|---|---|---|---|
| STORAGE-001 | `DONE` | 统一 BlobStorage trait | LFS/Package/OCI/Artifact/Release 已接入；legacy 可读；S3/迁移归 STORAGE-002 |
| STORAGE-002 | `TODO` | S3/MinIO 后端 | 依赖 STORAGE-001 |
| QUEUE-001 | `TODO` | 持久化后台任务抽象 | 未开始 |
| QUEUE-002 | `TODO` | Redis queue、重试和死信 | 依赖 QUEUE-001 |
| OPS-501 | `TODO` | 全实例 backup/restore | 依赖 STORAGE-001 |
| OPS-502 | `TODO` | 升降级与恢复演练 | 依赖 OPS-501 |
| OPS-503 | `TODO` | 三数据库长期压测与故障注入 | 依赖 PERF-001 |
| OPS-504 | `TODO` | 多节点和 HA 支持边界 | 依赖 STORAGE-002、QUEUE-002 |

## 6. 状态更新模板

任务开始时：

```markdown
| YYYY-MM-DD | TASK-ID | READY → DOING | 开始范围；明确不做什么 | 负责人 |
```

任务完成时必须记录：

```markdown
| YYYY-MM-DD | TASK-ID | DOING → DONE | 代码链接；测试命令和结果；文档链接；已知限制 | 负责人 |
```

任务阻塞时：

```markdown
| YYYY-MM-DD | TASK-ID | DOING → BLOCKED | 阻塞原因；已尝试方案；解除条件 | 负责人 |
```

## 7. 变更记录

| 日期 | 任务 | 状态变化 | 说明 | 负责人 |
|---|---|---|---|---|
| 2026-07-13 | ALIGN-001 | `READY → DOING` | 建立进度台账；开始统一状态源、完成度口径和证据矩阵 | 待指定 |
| 2026-07-13 | ALIGN-001 | `DOING → DONE` | 重建 67 项加权矩阵，统一完成度为 70.5%，修正规划估算、依赖和 Package 范围 | Codex |
| 2026-07-13 | GIT-001 | `TODO → READY → DOING` | 选择停止未实现 capability 广告并 fail closed 的安全路径 | Codex |
| 2026-07-13 | GIT-001 | `DOING → DONE` | HTTP/SSH capability 统一，4 个目标测试通过 | Codex |
| 2026-07-13 | GIT-003 | `TODO → READY` | 下一任务为真实 Git 客户端兼容矩阵 | Codex |
| 2026-07-14 | GIT-003 | `READY → DOING` | 开始 HTTP/SSH、V1/V2、clone/fetch/push 与降级行为的真实客户端矩阵 | Codex |
| 2026-07-14 | GIT-003 | `DOING → DONE` | 10 场景通过并进入 CI；修复 V2 fetch section 顺序；总完成度更新为 72.5% | Codex |
| 2026-07-14 | GIT-004 | `TODO → READY` | GIT-003 前置依赖已完成，下一任务为 shallow/deepen | Codex |
| 2026-07-14 | GIT-004 | `READY → DOING → DONE` | 实现 depth/deepen/unshallow/since/not；12 场景通过；总完成度更新为 73.2% | Codex |
| 2026-07-14 | GIT-002 | `TODO → READY` | GIT-004 前置依赖已完成，下一任务为 partial clone/filter | Codex |
| 2026-07-14 | GIT-002 | `READY → DOING → DONE` | 实现 filter pack 选择与能力广告；HTTP/SSH promisor lazy fetch 通过；总完成度更新为 74.0% | Codex |
| 2026-07-14 | CI-200 | `READY → DOING` | 审计 Actions 转换、Runner、网络、Secret、Token 与 fork 信任边界 | Codex |
| 2026-07-14 | CI-200 | `DOING → DONE` | 接受 ADR-0001；冻结专用外部 Runner、临时容器、网络/Secret/fork 和兼容边界；评分保持 74.0% | Codex |
| 2026-07-14 | STORAGE-001 | `TODO → READY` | CI-200 设计门完成；按推荐顺序进入统一 BlobStorage 边界设计与实现 | Codex |
| 2026-07-14 | STORAGE-001 | `READY → DOING` | 开始审计 LFS/Package/OCI/Artifact/附件/归档路径、事务和清理责任 | Codex |
| 2026-07-14 | STORAGE-001 | `DOING → DONE` | 统一 trait/对象键/原子本地 backend；迁移五类 Blob 并保留 legacy 读取；相关 19 项测试与 workspace check 通过；总完成度 74.5% | Codex |
| 2026-07-14 | ISSUE-101 | `TODO → READY` | STORAGE-001 完成，按推荐执行顺序进入 Markdown Issue/PR Template | Codex |
| 2026-07-14 | ISSUE-101 | `READY → DOING` | 开始核对 Gitea 1.26 模板发现、front matter、权限与 Issue/PR 创建流程 | Codex |
| 2026-07-14 | ISSUE-101 | `DOING → DOING` | 实现 API/选择页/权限/文档；核心 4 项、HTTP 2 项、workspace check、前端 check/build 通过；Browser 插件初始化冲突，按 DoD 暂不转 DONE/不加分 | Codex |
| 2026-07-14 | ISSUE-101 | `DOING → DONE` | 经用户确认改用 Playwright fallback；桌面/移动 Issue chooser 与预填、PR 预填通过；总完成度更新为 75.0% | Codex |
| 2026-07-14 | ISSUE-103 | `TODO → READY → DOING` | STORAGE-001 前置已完成；开始附件模型、权限、配额与 Blob 生命周期审计 | Codex |
| 2026-07-14 | ISSUE-103 | `DOING → DONE` | 四类附件、流式上传/下载、私有权限/配额/IDOR、OpenAPI/Web 与桌面/移动浏览器通过；修复 CSP nonce、附件下载路由和 star/watch Cookie 鉴权；总完成度 75.4% | Codex |
| 2026-07-14 | ISSUE-105 | `TODO → READY` | ISSUE-103 完成；下一任务为多 Assignee 数据模型、API、筛选和 UI | Codex |
