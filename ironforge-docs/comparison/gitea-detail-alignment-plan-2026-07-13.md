# IronForge 对齐 Gitea 1.26.4 细节功能计划

> 制定日期：2026-07-13
> 对比基线：Gitea 1.26.4、IronForge `39e33a9` 之后的当前工作区
> 当前完成度：按可验证加权矩阵计算为 **75.4%**
> 目标：把细节完成度提升到 90% 左右，优先覆盖小团队真实使用路径。
> 范围：只对齐 Gitea OSS 能力；不把 GitHub/GitLab 独有的 DevSecOps、Roadmap、Workspace 等能力混入本计划。
> 当前状态与变更记录：[gitea-alignment-progress-2026-07-13.md](./gitea-alignment-progress-2026-07-13.md)
> 评分口径与证据矩阵：[gitea-alignment-matrix-2026-07-13.md](./gitea-alignment-matrix-2026-07-13.md)

## 1. 执行方式

任务状态统一使用：

- `TODO`：尚未对齐需求；
- `READY`：范围和验收标准已确认，可以开发；
- `DOING`：正在开发，同一时间尽量只保留一个主任务；
- `BLOCKED`：存在明确外部依赖或决策阻塞；
- `DONE`：代码、测试、文档和兼容性验收全部完成。

任务推进顺序：

1. 每次只选择一个 `READY` 任务进入 `DOING`；
2. 开发前确认“不做什么”、数据迁移、权限边界和兼容目标；
3. 合并前按统一 Definition of Done 验收；
4. 完成后更新本文状态、实际工时和偏差，再选择下一项。

## 2. 统一 Definition of Done

除纯文档任务外，每个功能必须同时满足：

- 后端业务、REST API/OpenAPI 和前端闭环；没有 UI 的协议类功能需提供真实客户端验证；
- 私有仓库、用户/协作者/团队权限、跨仓库 IDOR 和资源归属校验齐全；
- 数据库变更支持 SQLite、PostgreSQL、MySQL，并包含 fresh migration 与升级迁移测试；
- 单元测试、HTTP 集成测试以及对应真实协议/E2E 测试通过；
- 前端 `npm run check` 为 0 error / 0 warning，涉及页面时增加浏览器 E2E；
- OpenAPI、i18n、配置样例、用户文档和 `CLAUDE.md` 当前事实同步更新；
- 不支持的兼容语义必须 fail closed，不允许静默成功；
- 不以“表、接口或页面存在”作为完成依据，必须验证完整用户路径。

## 3. 里程碑总览

| 里程碑 | 目标 | 预计投入 | 目标完成度 |
|---|---|---:|---:|
| M0：基线与协议正确性 | 消除能力误报，建立真实 Git 客户端回归 | 4–6 周 | 74% |
| M1：仓库与 Issue 高频体验 | 补齐日常协作中最明显的缺口 | 11–18 周 | 80% |
| M2：CI/Actions 兼容 | 从 YAML 转换器提升为可迁移的 Actions 子集 | 11–18 周 | 85% |
| M3：Package 与 API 生态 | 补主流协议、治理能力和高频 Gitea API | 16–25 周 | 88% |
| M4：身份、权限与管理细节 | 补认证源、用户关系和仓库 Unit 权限 | 9–14 周 | 90% |
| M5：生产化 | 对象存储、队列、完整备份、升级与性能 | 11–17 周 | 90%+ 可替代性 |

以上按原子任务上下限直接求和并按每周 5 个开发日折算，是完成该里程碑全部任务的人力串行估算；3–4 人可并行，但协议、数据模型和基础设施任务不能简单按人数线性压缩。

## 4. 原子任务清单

### M0：基线与 Git 协议正确性

| ID | 状态 | 优先级 | 任务 | 依赖 | 估算 |
|---|---|---|---|---|---:|
| ALIGN-001 | DONE | P0 | 建立代码可验证的 Gitea 对齐矩阵，废止旧 CSV 中已过时状态 | 无 | 实际 1 天 |
| GIT-001 | DONE | P0 | 停止广告未实现的 shallow/filter，并对显式请求 fail closed | ALIGN-001 | 实际 1 天 |
| GIT-002 | DONE | P1 | 实现 partial clone/filter，并用真实 Git 客户端验证 blob/tree filter | GIT-004 | 实际 1 天 |
| GIT-003 | DONE | P1 | 增加 HTTP/SSH、V1/V2、clone/fetch/push、shallow/partial 兼容矩阵 | GIT-001 | 实际 1 天 |
| GIT-004 | DONE | P1 | 实现 Protocol V2 shallow/deepen，真实客户端验证后重新广告能力 | GIT-003 | 实际 1 天 |
| PERF-001 | TODO | P1 | 建立大仓库 pack/clone/fetch/push 基线与回归阈值 | GIT-003 | 3–5 天 |

M0 验收门：文档声明与实际 capability 完全一致；真实客户端矩阵进入主 CI；不再用单元测试替代 Git wire protocol 回归。

### M1：仓库与 Issue 高频体验

| ID | 状态 | 优先级 | 任务 | 依赖 | 估算 |
|---|---|---|---|---|---:|
| REPO-101 | TODO | P1 | Push Mirror：多目标、凭证加密、手动/定时/按 push 同步、错误状态 | GIT-003 | 5–8 天 |
| REPO-102 | TODO | P1 | `git archive --remote` / upload-archive，覆盖 HTTP 与 SSH 权限 | GIT-003 | 4–6 天 |
| REPO-103 | TODO | P1 | Template Repository：生成仓库、可选内容、分支、Issue/标签复制 | 无 | 5–8 天 |
| REPO-104 | TODO | P1 | Blame、Go to file、目录删除三个 Web 代码浏览闭环 | 无 | 5–7 天 |
| REPO-105 | TODO | P2 | 自动生成 Release Notes，按合并 PR、作者、标签分组 | 无 | 3–5 天 |
| REPO-106 | TODO | P2 | 仓库内 OpenAPI 文件识别与安全渲染 | 无 | 3–5 天 |
| ISSUE-101 | DONE | P1 | Markdown Issue/PR Template 与模板选择页 | 无 | 实际 1 天 |
| ISSUE-102 | TODO | P1 | YAML Issue Form：字段校验、预填充、权限与兼容文件位置 | ISSUE-101 | 5–8 天 |
| ISSUE-103 | DONE | P1 | Issue/PR/评论附件上传、下载、删除和存储配额入口 | STORAGE-001 或本地存储接口 | 实际 1 天 |
| ISSUE-104 | TODO | P1 | Issue/PR/评论 Reaction，含唯一性、计数和通知策略 | 无 | 3–5 天 |
| ISSUE-105 | READY | P1 | 多 Assignee 数据模型、API、筛选和 UI | 无 | 4–6 天 |
| ISSUE-106 | TODO | P2 | Lock/Unlock、锁定原因、Pin/Unpin 和列表排序 | 无 | 4–6 天 |
| ISSUE-107 | TODO | P2 | Issue 依赖/阻塞关系与自动关联引用 | 无 | 5–8 天 |

M1 验收门：从模板创建 Issue、上传附件、多人指派、Reaction、锁定/依赖均可在 Web 与 API 完成；Push Mirror 和远程归档有真实 Git/远端服务回归。

### M2：CI 与 Gitea Actions 兼容

| ID | 状态 | 优先级 | 任务 | 依赖 | 估算 |
|---|---|---|---|---|---:|
| CI-200 | DONE | P0 | ADR：确定第三方 Action 的执行模型、信任边界、网络/Secret 策略和兼容范围 | 无 | 实际 1 天 |
| CI-201 | TODO | P1 | 只重跑失败 Job，并正确重建依赖 Job/Stage 状态 | CI-200 | 4–6 天 |
| CI-202 | TODO | P1 | Step/Job outputs 与 `needs.<job>.outputs` | CI-200 | 6–9 天 |
| CI-203 | TODO | P1 | `always()`、`failure()`、`cancelled()` 等运行时条件 | CI-202 | 5–8 天 |
| CI-204 | TODO | P1 | Service Containers、健康检查、网络隔离和凭证脱敏 | CI-200 | 7–10 天 |
| CI-205 | TODO | P1 | Reusable Workflow outputs、命名 Secret 映射和跨仓库调用 | CI-202 | 7–12 天 |
| CI-206 | TODO | P1 | 第三方 `uses:` Action 执行器，首批兼容官方常用 Actions | CI-200, CI-204 | 15–25 天 |
| CI-207 | TODO | P2 | Workflow 依赖图和失败 Job 精准定位 UI | CI-201 | 4–6 天 |
| CI-208 | TODO | P1 | Actions 兼容测试集：官方示例 + 固定版本真实 Actions | CI-201~206 | 5–8 天 |

M2 验收门：对支持的 Actions 子集提供版本化兼容声明；不支持的 workflow 在创建 pipeline 前给出可操作错误；第三方 Action 不能默认获得宿主机或无边界 Secret 权限。

### M3：Package Registry 与 API 生态

| ID | 状态 | 优先级 | 任务 | 依赖 | 估算 |
|---|---|---|---|---|---:|
| PKG-301 | TODO | P1 | Package cleanup rules、保留策略、配额和管理员回收 | STORAGE-001 可并行设计 | 6–9 天 |
| PKG-302 | TODO | P1 | Terraform State Registry，含 lock/unlock | 无 | 5–8 天 |
| PKG-303 | TODO | P1 | Go Package Registry 原生协议 | 无 | 4–7 天 |
| PKG-304 | TODO | P2 | Alpine/Debian/RPM 原生仓库协议 | 无 | 12–18 天 |
| PKG-305 | TODO | P2 | Conan/Conda/Pub/Swift/Vagrant 长尾协议，按用户需求排序 | 无 | 15–25 天 |
| PKG-306 | TODO | P2 | Arch/Chef/CRAN 原生协议，补齐 Gitea 1.26.4 官方协议矩阵遗漏 | 无 | 8–12 天 |
| API-301 | TODO | P1 | 生成 Gitea 1.26.4 OpenAPI 对比清单和 contract test | ALIGN-001 | 3–5 天 |
| API-302 | TODO | P1 | 补高频 Repository/Issue/User/Organization API 兼容端点 | API-301 | 10–15 天 |
| API-303 | TODO | P2 | Gitea 响应结构、分页、错误码和 Token scope 兼容层 | API-301 | 8–12 天 |
| HOOK-301 | TODO | P1 | 用户/组织/系统 Webhook 及 Gitea 事件矩阵补全 | API-301 | 6–10 天 |

M3 验收门：协议支持必须通过真实包管理器 publish/install/search/yank 测试；Generic fallback 不再计为对应专用协议完成；API 兼容以 contract test 而非端点数量验收。

### M4：身份、权限与管理细节

| ID | 状态 | 优先级 | 任务 | 依赖 | 估算 |
|---|---|---|---|---|---:|
| AUTH-401 | TODO | P1 | LDAP 周期同步、停用/恢复和组织/团队映射 | 现有 LDAP | 7–10 天 |
| AUTH-402 | TODO | P2 | Reverse Proxy Header Authentication 与可信代理边界 | 无 | 4–6 天 |
| AUTH-403 | TODO | P2 | PAM/SMTP Authentication Source，按部署需求决定是否实现 | 无 | 6–10 天 |
| USER-401 | TODO | P1 | 用户 Block/Unblock 及仓库、Issue、评论、通知联动限制 | 无 | 6–9 天 |
| USER-402 | TODO | P2 | Follow/Unfollow、Follower/Following 列表 | 无 | 3–5 天 |
| USER-403 | TODO | P2 | 管理员 User Badge 与个人页展示 | 无 | 3–5 天 |
| PERM-401 | TODO | P1 | 仓库 Unit 级 Code/Issues/PR/Wiki/Packages/Actions 读写权限 | 无 | 10–15 天 |
| ADMIN-401 | TODO | P2 | Gitea 风格系统配置查看、任务状态和队列管理页面 | QUEUE-001 | 6–9 天 |

M4 验收门：外部身份生命周期可回收；用户屏蔽在所有写路径统一生效；仓库 Unit 权限由统一授权层执行，不能由 handler 手工遗漏。

### M5：生产化与可恢复性

| ID | 状态 | 优先级 | 任务 | 依赖 | 估算 |
|---|---|---|---|---|---:|
| STORAGE-001 | DONE | P0 | 统一 BlobStorage trait，覆盖 LFS/Package/OCI/Artifact/附件/归档 | 无 | 实际 1 天 |
| STORAGE-002 | TODO | P1 | S3/MinIO 后端、签名 URL、迁移与一致性校验 | STORAGE-001 | 8–12 天 |
| QUEUE-001 | TODO | P1 | 持久化后台任务抽象，先覆盖 mail/mirror/webhook/index/archive | 无 | 8–12 天 |
| QUEUE-002 | TODO | P2 | Redis queue、重试、退避、死信和可观测性 | QUEUE-001 | 6–10 天 |
| OPS-501 | TODO | P0 | 全实例 backup/restore：DB、repos、LFS、packages、OCI、artifacts、配置 | STORAGE-001 | 8–12 天 |
| OPS-502 | TODO | P1 | 升级/降级边界、备份恢复演练和版本矩阵 | OPS-501 | 5–8 天 |
| OPS-503 | TODO | P1 | SQLite/PostgreSQL/MySQL 长期并发压测与故障注入 | PERF-001 | 6–10 天 |
| OPS-504 | TODO | P2 | 多节点部署边界、共享状态清单和明确的 HA 支持等级 | STORAGE-002, QUEUE-002 | 5–8 天 |

M5 验收门：从全实例备份能恢复到新主机并通过仓库 clone、LFS、Package、OCI、CI Artifact 和权限抽查；故障恢复目标和不支持的部署拓扑被明确记录。

## 5. 推荐实际执行顺序

不要按文档章节完全串行。建议前 14 个开发任务依次为：

1. `ALIGN-001`：冻结真实基线（DONE）；
2. `GIT-001`：停止错误 capability 广告（DONE）；
3. `GIT-003`：把 Git 真实客户端矩阵放入 CI（DONE）；
4. `GIT-004`：实现 shallow/deepen 并重新广告（DONE）；
5. `GIT-002`：实现 partial clone/filter（DONE）；
6. `CI-200`：确定 Actions 执行与安全边界（DONE）；
7. `STORAGE-001`：统一 BlobStorage，冻结后续附件和备份的存储边界（DONE）；
8. `ISSUE-101`：Issue/PR Markdown Template（DONE）；
9. `ISSUE-103`：附件上传、下载与权限闭环（DONE）；
10. `ISSUE-105`：多 Assignee；
11. `ISSUE-104`：Reaction；
12. `REPO-101`：Push Mirror；
13. `CI-201`：失败 Job 重跑；
14. `OPS-501`：完整实例备份恢复。

这样可以先消除协议能力误报，再补用户最常遇到的协作细节，同时尽早解决 Actions 和存储两项会影响后续架构的大决策。

## 6. 第一个待对齐任务：ALIGN-001

开发前需要确认：

- 对比版本固定为 Gitea 1.26.4，升级基准必须单独变更本文；
- 完成度按“真实闭环”计算，Generic fallback、空 UI、未执行字段不算完成；
- 对齐矩阵至少记录：功能、Gitea 行为、IronForge 行为、代码证据、测试证据、状态、任务 ID；
- `gitea-gap-list.csv` 已重建为 100 分加权、67 项代码可验证矩阵，旧状态通过 Git 历史追溯；
- `ALIGN-001`、`GIT-001`、`GIT-003`、`GIT-004`、`GIT-002`、`CI-200`、`STORAGE-001`、`ISSUE-101`、`ISSUE-103` 已完成；下一任务为 `ISSUE-105` 多 Assignee。
