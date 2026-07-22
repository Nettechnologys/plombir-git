# IronForge 与主流 Git 平台差距分析（2026-07-10）

> 分析日期：2026-07-10
> 本地代码基线：`ececb16` 及当前工作区
> 对比对象：GitHub、GitLab、Gitea 1.26、Forgejo 15
> 分析方法：当前源码与测试静态核验 + 主流平台官方文档校准
> 注意：本文区分“模块/接口存在”和“功能闭环、权限安全、生产成熟”。旧报告中的完成度百分比不能直接作为平台替代能力结论。

## 实施进度更新（2026-07-11）

- PR/Review 仓库权限、资源归属与越权测试已完成；
- PAT `repo` / `user` / `admin` scope 已在 REST 与 Git HTTP 强制执行；
- SSH Key 用户 CRUD、设置页、唯一性/所有权校验及使用时间记录已完成；
- PostgreSQL/MySQL 运行时 SQLite 硬编码、PostgreSQL 原始 SQL 占位符、MySQL 表重命名，以及 Wiki revision/Runner 字段跨后端迁移已修复；
- 主 CI 已增加 PostgreSQL 17 与 MySQL 8.4 的真实 migration + CRUD + counter + FTS smoke；2026-07-13 又在独立 PostgreSQL/MySQL 服务上完成 migration、CRUD、counter、FTS 与并发认证实测。
- 前端 `svelte-check` 基线已恢复为 0 error / 0 warning，并补齐登录页 OAuth2/OIDC SSO provider 入口。
- Draft PR、多人 Reviewer 请求/移除、评论线程 Resolve/Reopen 已完成后端权限、OpenAPI、前端与集成测试闭环。
- CODEOWNERS 已支持标准文件位置、通配符/覆盖规则，以及用户和 `@org/team` 自动 Reviewer 请求；团队仅限仓库所属组织且要求 write/admin 权限，展开后按用户去重。
- PR Diff 已改为逐文件结构化行数据，前端支持新增/删除/上下文行号、行级评论与线程解决/重开。
- 修复分支 ref 指向 commit/tag 时未 peel 到 tree、导致真实仓库 Diff 失败的问题，并增加真实 bare 仓库 + 双分支 + CODEOWNERS 的 API 集成覆盖。
- Auto-merge 已支持 merge/squash/rebase 策略、启停 API、前端等待状态，以及审批、push、内置/外部 Runner CI 成功后的自动重试；并通过数据库认领避免并发重复合并。
- PR 创建和 push 现在持续维护 `head_sha`；审批按 Reviewer 去重并绑定当前提交，新提交会使旧审批失效。
- Merge Queue 已支持仓库级 FIFO、merge/squash/rebase、排队/取消/列表 API、前端队列位置，以及审批/push/CI 事件驱动；队首通过保护规则后原子合并，失败项会记录原因并继续队列。
- 结构化代码建议已支持单行/连续多行 replacement 与范围删除、同仓/Fork 源仓库权限校验、一键应用并生成提交；建议绑定 PR `head_sha`，越界、过期或已应用建议会被拒绝。
- 批量代码建议已支持选择 1—100 条建议在单个提交中原子应用；同文件按原始行号倒序处理，重叠范围、重复 ID、无效路径、symlink 路径及并发分支更新会被拒绝，成功后只触发一次 CI/Auto-merge/Merge Queue 评估。
- 2026-07-11 阶段，PR 会话页已增加统一审查时间线 API/UI；当时仍依赖既有状态表，Reviewer 移除、线程重开等历史尚未保留（该项已由下一条 2026-07-12 更新闭环）。
- 2026-07-12：新增 append-only `pr_events`，Reviewer 请求/移除、线程解决/重开、Draft/Ready、Review/评论/建议、Auto-merge、Merge Queue 与合并状态变更均持久化；时间线对新事件使用不可变记录，对迁移前 PR 保留兼容回放。
- 2026-07-12：Merge Queue 新增 speculative merge-group commit；当前 base/head 组合先生成隐藏 ref 并运行 `merge_group` CI，流水线成功后才更新目标分支，base/head 变化会自动使旧结果失效。
- 2026-07-12：仓库 Deploy Key 已完成 CRUD、前端设置页、全局指纹冲突检查、仓库强隔离、只读/读写 SSH 授权和使用时间记录；管理操作要求仓库 admin 权限。
- 2026-07-12：CI job variables 已持久化并注入内置/外部 Runner；Gitea Actions adapter 对非 `actions/checkout` 的 `uses:` 改为显式拒绝，避免静默跳过后产生假成功。完整 Actions runtime 仍不在当前兼容范围。
- 2026-07-12：补齐仓库级加密 CI Secrets（AES-256-GCM、管理员 API/UI、本地/Docker/外部 Runner 注入、服务端日志脱敏）、原生与 Actions `strategy.matrix` 笛卡尔展开（上限 256），以及 Tag 通配保护规则（管理员 API/UI、HTTP/SSH 推送统一拒绝）。
- 2026-07-12：受保护分支新增“要求签名提交”，在 pack 入库、ref 更新之前对本次引入的每个 commit 执行密码学 `verify-commit`，HTTP/SSH 推送共用；由于当前没有服务端签名密钥，平台 PR/Auto-merge/Merge Queue 在该规则下 fail closed。Actions job 级 `uses:`（Reusable Workflow）改为 fail closed，避免空 job 假成功。
- 2026-07-12：修复 Runner 执行基础：内置 Runner 使用精确 commit 的 detached worktree，独立 Runner 通过受认证 tar 快照获取同一提交，Variables/Secrets 真正进入独立 Runner 本地/Docker executor。补齐仓库隔离 CI Cache，原生 `cache` 与 `actions/cache@v4` 共用 key/path 模型，并覆盖内置、Docker、外部 Runner restore/save。
- 2026-07-12：支持仓库内 Reusable Workflow：`./.gitea/workflows/*.yml`、`on: workflow_call`、最多 4 层递归、inputs、`secrets: inherit`、单值/数组 needs 和调用前后依赖重写；远程 workflow、命名 Secret 重映射、outputs 与循环引用 fail closed。
- 2026-07-12：修复 CI 执行策略假支持：`allow_failure` / Actions `continue-on-error` 已影响内置与外部 Runner 的 stage/pipeline 结果；原生 `timeout_seconds` / Actions `timeout-minutes` 已进入数据库与两类 Runner 的强制超时；原生 `when: manual` 已具备持久化暂停、写权限 play API/UI、幂等释放、服务重启后恢复及独立 Runner 前置阶段门控。当时非平凡 Actions `if:` 仍 fail closed，现由下一条静态条件更新部分闭环。
- 2026-07-12：有限但真实支持静态 CI `if`：ref/ref-name/event/SHA、env/matrix、布尔组合、比较和常用字符串函数由无 `eval` 的解释器执行；Job 条件持久化并按 Matrix 变体生成 `skipped`，Step 条件在转换阶段裁剪。Actions 脚本补 `set -e` 防止后续 Step 掩盖失败；依赖运行时状态的 `always/failure/cancelled` 与未知语法继续 fail closed。
- 2026-07-12：LDAP 已进入主登录链路：启用 Provider 可认证既有目录用户并在首次成功 bind 后自动建号；本地账号不回退、目录密码不落库、LDAP 身份绑定 Provider、过滤值按 RFC 4515 转义、唯一结果/空密码/超时/TLS 校验均 fail closed，并复用 MFA 与登录审计；管理员可使用脱敏的连接测试入口验证 service bind。
- 2026-07-12：修复 MFA 第一因素未绑定问题：密码/LDAP/SSO 成功后只签发五分钟、HttpOnly、签名域隔离的 MFA challenge，MFA API 校验 challenge 后才签发用户会话；登录成功/失败进入专用日志，已知账户连续五次失败锁定 15 分钟。
- 2026-07-12：修复 OAuth account 历史迁移表名 `o_auth_accounts` 与实体 `oauth_accounts` 不一致导致的运行时 500；兼容迁移仅在检测到旧表且目标表不存在时改名。
- 2026-07-12：认证事件进入管理员查询界面：密码、LDAP、SSO、MFA 的成功/失败记录可按用户名、Provider、结果和时间分页筛选，并展示来源 IP、User-Agent 与规范化失败原因。
- 2026-07-12：管理员用户列表展示认证来源、失败次数、锁定时间与最近登录，并提供带审计事件的账号解锁入口，误锁不再需要直接修改数据库。
- 2026-07-12：补齐受保护部署环境：原生/Actions `environment` 解析与 job 持久化、仓库管理员环境规则 API/UI、审批人白名单与 1-10 票阈值、重复审批去重、`waiting_approval` 状态、内置/外部 Runner 恢复及多 gate 阶段级防并发恢复。审批历史关联的环境禁止删除。
- 2026-07-12：补齐 CI workload OIDC：公开 discovery/JWKS、由实例密钥确定性派生的 Ed25519 非对称签名、5 分钟 audience-bound token，以及 repo/pipeline/job/ref/SHA claims；exchange 只能使用有效 `CI_JOB_TOKEN`，并再次绑定数据库资源关系与 assigned/running 状态。配置 `external_url` 后两类 Runner 均注入 `CI_OIDC_TOKEN_URL`。
- 2026-07-12：补齐 CI Artifact/Cache retention：仓库管理员可配置 1-3650 天保留期；Artifact 上传写入实际过期时间，Cache 建立仓库隔离元数据并按最后访问滑动续期；内置、Docker、外部 Runner 共用策略。后台每小时和手动 API/UI 执行文件感知清理，先验证受管根目录并删除磁盘，再删除 DB，避免旧实现只删记录造成泄漏。

因此，第 4.1—4.3 节记录的是修复前基线；第 4.4 节的代码与 CI 缺口已进入验证阶段，不再是“完全未闭环”状态。

## 1. 结论

IronForge 已具备小团队自托管 Git 平台的主要骨架，功能广度接近早期 Gitea/Forgejo；但距离当前 Gitea/Forgejo 仍有中等功能差距和较大的生产成熟度差距，距离 GitHub/GitLab 则是平台级差距。

当前最合理的产品定位是：

- Rust 实现的轻量自托管 Git Forge；
- 以单机/小团队部署为主要场景；
- 以 MCP/AI Agent 集成为差异化优势；
- 暂不应宣传为 GitHub/GitLab 的直接替代品；
- PR 授权、PAT scope、SSH Key 管理及 PostgreSQL/MySQL 首轮实库 P0 验证已闭环。生产就绪判断仍需结合后续版本矩阵、备份恢复、长期压测和安全审计结果。

## 2. 当前已经具备的能力

当前代码已经覆盖：

- Git Smart HTTP、SSH、Git Protocol V2、LFS；
- 仓库、Fork、Star/Watch、Mirror、Release、归档和导入；
- Issue、PR、Review、Wiki、组织、团队、通知、邮件；
- 原生 CI YAML、Gitea Actions 转换、内置/外部 Runner、Artifact、实时日志、Pipeline 重试和并发控制；
- Package Registry、OCI Registry；
- OAuth2/OIDC、TOTP MFA、审计日志；
- 看板、工时、全文/代码搜索；
- OpenAPI、Prometheus、健康检查、Docker 部署；
- MCP stdio server。

顶层事实见 `ironforge-docs/architecture/project-architecture-2026-07.md`。该文档在数据库口径上已落后于 2026-07-08 的多后端代码变更，数据库现状应结合本文第 4.4 节判断。

## 3. 主要差距矩阵

| 领域 | IronForge 当前情况 | 与主流平台的差距 |
|---|---|---|
| Git/仓库治理 | Clone/Push、Fork、Mirror、LFS、分支/Tag 保护、签名提交强制、Deploy Key、用户/团队 CODEOWNERS、Auto-merge 和 merge-group CI FIFO Merge Queue 已具备 | 缺统一规则集和批量队列；大仓库 pack 性能未充分验证 |
| PR 与代码审查 | 支持三种合并、Review、Draft PR、Reviewer 请求、逐文件行级 Diff/评论、单行/多行及批量代码建议、append-only 审查时间线、线程 Resolve、提交级审批失效、用户/团队 CODEOWNERS、Auto-merge 和 Merge Queue | 尚缺批量 Merge Queue、复杂规则集等高级能力 |
| CI/CD | 原生 YAML、隔离提交工作区、Docker/外部 Runner、Artifact、Cache、保留/清理策略、日志、重试、并发控制、加密 Secrets/Variables、Matrix、本地 Reusable Workflow、受保护 Environment、workload OIDC、merge-group CI；Actions adapter 对不支持的 `uses:` fail closed | 明确不提供完整 Actions runtime；缺远程 Reusable Workflow/outputs、服务容器和多项目流水线 |
| DevSecOps | 有 GPG 验签和审计日志 | 缺 Dependency Graph、依赖更新、SAST/DAST、Secret Scanning、Push Protection、SBOM、容器漏洞扫描、安全公告和漏洞修复工作流 |
| Package Registry | OCI 加 Cargo/npm/Maven/PyPI/NuGet/RubyGems/Helm/Composer 等原生适配 | 约 10 个原生 adapter，其余声明类型落到 Generic；缺更完整的协议覆盖、清理规则、配额、保留策略和供应链证明 |
| 企业身份与治理 | OAuth2/OIDC、TOTP、LDAP 主登录与首次建号、审计查询 | 缺 SAML、SCIM、目录用户自动停用/回收、团队同步、IP Allowlist、审计流式导出等 |
| 项目与社区协作 | Issue、Milestone、简单看板、工时、Wiki | 缺 Issue Form/模板、Discussions、跨仓库项目、自定义字段、Iteration、Roadmap/Epic、Service Desk、Snippets/Gists |
| 开发者体验 | 单文件 Web 编辑、代码搜索、MCP 只读工具 | 缺完整 Web IDE、Pages、远程开发环境、成熟 CLI 和 IDE 插件 |
| API 与生态 | REST、OpenAPI、Webhook、MCP | 缺 GraphQL、GitHub/GitLab API 兼容层、App 安装模型、插件市场和第三方生态；MCP 目前只有少量只读工具且仅 stdio |
| 部署与扩展 | 单机 Docker、Prometheus、健康检查、SQLite DB 备份 | 缺对象存储、分布式任务队列、水平扩展、仓库存储分片、HA、跨地域复制、完整实例备份/恢复和零停机升级 |
| 测试与发布成熟度 | 现有 Rust 测试、22 个 HTTP 集成测试文件、前端编译检查及 PostgreSQL/MySQL runtime smoke CI | 主 CI 未运行真实浏览器 E2E、大仓库性能或升级/恢复演练；CLI/MCP/Runner/SSH 测试仍偏少 |

## 4. 上线阻断项（P0）

### 4.1 PR/Review 权限边界不完整

静态调用链显示：

- `pulls::list_prs`、`get_pr` 和 `get_diff` 没有仓库读权限检查；
- `pulls::update_pr` 没有认证参数；
- `pulls::merge_pr` 只检查是否存在登录用户，没有调用 `can_write_repo`；
- Review 的多个 GET 端点没有仓库读权限检查；
- 按 review id 读取/驳回时，部分路径没有验证 review 是否属于 URL 指定仓库和 PR。

可能影响：私有仓库 PR 信息泄露、越权更新、越权合并、跨仓库 IDOR。

主要代码：

- `crates/rg-http/src/api/pulls.rs`
- `crates/rg-http/src/api/reviews.rs`
- `crates/rg-core/src/pull_request/service.rs`

### 4.2 PAT scope 保存但未执行

`access_tokens.scopes` 保存了 scope；但 `resolve_pat()` 只返回 token 的 `user_id`，随后 middleware 为该用户签发等价 JWT。scope 没有进入 JWT，也没有在 API/Git 路径执行授权判断。

可能影响：用户创建的受限 PAT 实际获得完整用户 API 权限。

主要代码：

- `crates/rg-db/src/entities/access_token.rs`
- `crates/rg-http/src/lib.rs::resolve_pat`
- `crates/rg-http/src/lib.rs::pat_auth_middleware`

### 4.3 SSH 公钥缺少用户自助入口

当前存在 `ssh_keys` entity、DB ops 和 SSH 指纹认证，但没有发现用户可调用的 HTTP API、前端页面或 CLI 命令。公钥认证底层可用，但普通用户无法自行添加、查看、轮换和删除 SSH Key，也没有 Deploy Key 模型。

主要代码：

- `crates/rg-db/src/entities/ssh_key.rs`
- `crates/rg-db/src/ops/ssh_key_ops.rs`
- `crates/rg-ssh/src/lib.rs`

### 4.4 PostgreSQL/MySQL 首轮实库兼容已闭环

2026-07-13 已在独立 PostgreSQL/MySQL 服务完成 migration、CRUD、stars counter、Wiki/仓库 FTS、并发登录锁定及完整服务启动 `/health` 验证。实测修复包括跨后端时间默认值、严格外键顺序与类型、MySQL TEXT 默认值和 FTS 语法、PostgreSQL `TIMESTAMPTZ`/`DateTimeUtc` 对齐，以及数据库 URL 密码日志脱敏；主 CI 也保留 PostgreSQL 17/MySQL 8.4 service smoke。

当前可描述为“首轮运行时兼容已闭环”，但完整生产级仍需要更多数据库版本矩阵、备份恢复、升级演练、HA 和长期压力测试。

主要代码：

- `ironforge-docs/architecture/db-multi-backend-design-2026-07.md`
- `crates/rg-core/src/wiki/service.rs`
- `crates/rg-db/src/ops/repo_ops.rs`
- `.github/workflows/regression.yml`

## 5. 与 Gitea/Forgejo 的重点差距

### 5.1 CI 生态

Gitea Actions 已用于其自身生产仓库；Forgejo Actions 有独立 Runner、日志/Artifact 保留、缓存和多 Runner 分发。IronForge 当前是 Actions YAML 到内部 `CiConfig` 的有限转换：`actions/checkout` 视为隐式完成，step 级其他 `uses:` 和 job 级 Reusable Workflow `uses:` 都会在创建 pipeline 前明确报错，并引导改写为显式 job/`run:` 或原生 `.ironforge-ci.yml`，不再静默跳过。

### 5.2 Package Registry

Gitea 1.26 官方列出的专用包管理器包括 Alpine、Arch、Cargo、Chef、Composer、Conan、Conda、Container、CRAN、Debian、Generic、Go、Helm、Maven、npm、NuGet、Pub、PyPI、RPM。Forgejo 还覆盖 RubyGems、Swift、Vagrant 等，并提供 package cleanup rules 与 blob deduplication。

IronForge 当前原生 adapter 映射为 Cargo、npm、NuGet、PyPI、RubyGems、Maven、Docker、Generic、Helm、Composer；其他类型使用 Generic fallback。

### 5.3 运维能力

Gitea 已提供 MySQL/PostgreSQL/MSSQL/SQLite、Redis/LevelDB queue，以及 local/MinIO-S3/Azure Blob storage。IronForge 的 Git、LFS、Package、OCI、Artifact 仍主要依赖单机文件系统，没有分布式 queue/storage 抽象。

## 6. 与 GitHub/GitLab 的平台级差距

### 6.1 CI/CD 与部署

GitHub Actions 支持 Matrix、Reusable Workflows、缓存、Environments、部署审批、OIDC、Runner Groups/Scale Sets、Artifact Attestations 和 Marketplace。GitLab CI/CD 支持可复用 Components、Matrix、Parent-child/Multi-project pipelines、环境审批、受保护变量和多种 Runner executor。

IronForge 当前 CI 更接近“阶段 + Shell/Docker Job 执行器”，尚不是兼容主流生态的 Workflow 平台。

### 6.2 软件供应链安全

GitHub Code Security/Secret Protection 和 GitLab Application Security 已形成代码、依赖、Secret、容器、IaC、DAST、SBOM、策略与告警治理闭环。IronForge 尚无对应领域模型和 UI。

### 6.3 企业 IAM 与合规

GitHub/GitLab 均提供 SAML SSO、SCIM、企业托管用户、团队/群组同步和企业审计能力。IronForge 当前 OAuth2/OIDC、LDAP 主登录和管理员认证事件查询已闭环，但 SAML/SCIM 和目录生命周期治理仍未实现。

### 6.4 规划、社区和开发环境

GitHub 提供 Discussions、Projects 自定义字段与 Iterations；GitLab 提供 Epic/Roadmap、Service Desk、Pages、Web IDE 和 Workspaces。IronForge 当前只有仓库级简单看板、Issue/Milestone、Wiki 和单文件编辑。

### 6.5 API 和扩展生态

GitHub/GitLab 均有 GraphQL API；GitHub Apps 提供细粒度权限、短期安装 token、Webhook 和 Marketplace。IronForge 当前主要是 REST/OpenAPI/Webhook，MCP 是优势但仍为少量只读 stdio 工具。

## 7. 建议执行顺序

### 第一波：权限与身份 P0

1. PR/Review 全链路 `can_read_repo` / `can_write_repo`；
2. 资源 ID 必须校验属于 URL 指定 repo/PR；
3. PAT scope 在 REST 与 Git HTTP 强制执行；
4. SSH Key 用户 CRUD API + 前端设置页 + 唯一性/所有权测试；
5. 增加私有仓库泄露、越权更新、越权合并和跨仓库 IDOR 集成测试。

### 第二波：CI 与代码审查闭环

1. Actions 执行兼容或明确采用独立格式；
2. Secrets/Variables、Cache、Matrix、Reusable Workflow、环境审批和 OIDC；
3. merge-group 合成提交 CI/批量队列；
4. 为 Reviewer 移除、线程重开、Draft/Ready 切换等补充不可变审查事件存储。

状态（2026-07-12）：第 3、4 项已完成；第 1 项已明确采用“有限 adapter + 不支持能力 fail closed + 原生格式”边界；第 2 项已完成加密 Secrets、Variables、Cache、Matrix、仓库内 Reusable Workflow、环境审批和 workload OIDC，远程 Reusable Workflow/outputs 仍为后续独立阶段。

### 第三波：生产化

1. ~~PostgreSQL/MySQL 实库 migration + CRUD + FTS smoke~~（✅ 2026-07-13，并扩展认证并发计数）；
2. ~~全量消除 runtime 路径中的 SQLite backend 硬编码~~（✅ 已完成关键运行路径与回归守卫）；
3. 对象存储、后台任务队列、全实例备份恢复；
4. 多节点/HA/升级与恢复演练；
5. 大仓库 clone/fetch/push 性能和兼容矩阵。

### 第四波：平台能力

1. DevSecOps 与软件供应链；
2. SAML/SCIM 和企业治理；
3. GraphQL/App 扩展模型；
4. Discussions、Roadmap、Pages、Web IDE 等体验功能。

## 8. 竞品官方资料

- GitHub Actions Matrix：<https://docs.github.com/en/actions/how-tos/write-workflows/choose-what-workflows-do/run-job-variations>
- GitHub Reusable Workflows：<https://docs.github.com/en/actions/how-tos/reuse-automations/reuse-workflows>
- GitHub Code Security：<https://docs.github.com/en/code-security/getting-started/quickstart-for-securing-your-repository>
- GitHub GraphQL：<https://docs.github.com/en/graphql>
- GitHub Apps：<https://docs.github.com/en/apps/overview>
- GitHub SAML/SCIM：<https://docs.github.com/en/enterprise-cloud%40latest/organizations/managing-saml-single-sign-on-for-your-organization/connecting-your-identity-provider-to-your-organization>
- GitLab CI/CD：<https://docs.gitlab.com/ci/>
- GitLab Downstream Pipelines：<https://docs.gitlab.com/ci/pipelines/downstream_pipelines/>
- GitLab Application Security：<https://docs.gitlab.com/user/application_security/detect/>
- GitLab Reference Architectures：<https://docs.gitlab.com/administration/reference_architectures/>
- GitLab Geo：<https://docs.gitlab.com/administration/geo/>
- GitLab GraphQL：<https://docs.gitlab.com/api/graphql/>
- Gitea Actions：<https://docs.gitea.com/usage/actions/overview>
- Gitea Packages：<https://docs.gitea.com/usage/packages/overview>
- Gitea Configuration：<https://docs.gitea.com/administration/config-cheat-sheet>
- Forgejo Actions：<https://forgejo.org/docs/latest/user/actions/overview/>
- Forgejo Packages：<https://forgejo.org/docs/latest/user/packages/>
