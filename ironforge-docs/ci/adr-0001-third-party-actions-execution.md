# ADR-0001：第三方 Actions 执行与安全边界

> 状态：**Accepted**
> 日期：2026-07-14
> 任务：`CI-200`
> 对齐基线：Gitea 1.26.4
> 影响后续任务：`CI-202`、`CI-203`、`CI-204`、`CI-206`、`CI-208`

## 1. 背景

IronForge 已能读取 `.gitea/workflows/*.yml`，支持 `run`、Matrix、静态条件、本地 reusable workflow，以及 `actions/checkout`、`actions/cache` 的内置适配。除此之外的 `uses:` 会在创建 Pipeline 时显式拒绝，不会被静默忽略。

当前实现仍是“Actions YAML 到 IronForge Job”的有限转换层，并不是第三方 Action runtime：

| 当前行为 | 代码证据 | 安全/兼容影响 |
|---|---|---|
| 多个 step 被拼接为单个 `JobConfig.script` | `crates/rg-ci/src/gitea_actions.rs` | 丢失 step 生命周期、outputs、`pre/main/post` 和运行时条件边界 |
| 未指定 image 的内置 Job 在 IronForge 主机执行 | `crates/rg-ci/src/runner.rs` | 不能用于执行不受信任的第三方代码 |
| 外部 Runner 未指定 image 时也在 Runner 主机执行 | `crates/rg-runner/src/main.rs` | Action 工作负载可能越过容器隔离 |
| Docker Job 只有 `--rm` 和工作区挂载 | `crates/rg-ci/src/runner.rs`、`crates/rg-runner/src/main.rs` | 尚无默认断网、资源上限、capability 收敛和 `no-new-privileges` |
| 仓库 Secret 当前整体注入 Job | `crates/rg-ci/src/runner.rs`、`crates/rg-http/src/api/runners.rs` | 不符合按引用最小授权；外部 Runner 会接收解密值 |
| `CI_JOB_TOKEN` 当前固定为 `repo:read packages:read` | `crates/rg-ci/src/runner.rs` | 尚未解析并收敛 workflow `permissions`，也没有 fork 来源策略 |

Gitea 1.26 的官方模型把 Job 委托给外部 `act_runner`，通常每个 Job 使用新容器；官方文档同时明确宿主机模式具有安全风险，Action 下载和 Job 网络访问需要由部署者控制。Gitea.com 对 fork Pull Request 的 Actions 采用审批和隔离策略。参考：

- [Gitea Actions Overview](https://docs.gitea.com/usage/actions/overview)
- [Gitea Actions Design](https://docs.gitea.com/usage/actions/design)
- [Gitea Actions FAQ](https://docs.gitea.com/usage/actions/faq)
- [Gitea Actions comparison](https://docs.gitea.com/usage/actions/comparison)
- [Gitea token permissions](https://docs.gitea.com/usage/actions/token-permissions)

本 ADR 冻结 IronForge 的兼容目标和信任边界。它不代表第三方 Action 已经可执行。

## 2. 决策

### 2.1 执行位置与 Runner 匹配

1. 第三方 Action 只能分配给声明 `actions-v1` 能力的**专用外部 Runner**，不得在 IronForge 服务进程或服务主机中执行。
2. 每个 Job 创建一个新的临时容器和临时网络；同一 Job 的 step/action 在该 Job 沙箱中顺序执行，Job 结束后销毁。
3. 第三方 Action 禁止宿主机模式。无 image 或无可匹配沙箱能力时必须 fail closed。
4. 原生 IronForge Job 可在管理员显式标记的受信 Runner 上保留宿主机模式，但不会因此获得 `actions-v1` 能力。
5. Runner capability 至少包含执行器版本、`docker`、`node20`、可用网络 profile 和架构；调度器必须满足 Job 的全部要求，不能只按普通 label 猜测兼容性。

IronForge 不直接嵌入 `act_runner` 或复制其私有调度协议。未来可以增加独立适配器，但仍必须满足本 ADR 的相同隔离与审计门槛。

### 2.2 保留结构化 Step 语义

引入版本化的结构化执行计划，替代把 Actions steps 拼接成一个 shell script：

- step 类型至少包含 `Run`、`Uses`、`Checkout`、`Cache`；
- 保留 `id`、`name`、`with`、`env`、`if`、`continue-on-error`、timeout 和 shell；
- 执行器实现 step 级状态、outputs、`pre/main/post` 生命周期和日志分段；
- 解析阶段即可确定的不支持语义必须在 Pipeline 创建前失败；依赖运行时状态的表达式在对应 step 前求值并 fail closed。

该模型是 `CI-202`、`CI-203` 和 `CI-206` 的共同前置设计约束。

### 2.3 Action 来源、解析与固定版本

新增 `ActionResolver`，执行以下规则：

1. 默认允许同一 IronForge 实例内的仓库 Action；外部 HTTPS host 必须进入实例级 allowlist。
2. 相对路径只允许当前 workflow commit 内的本地 Action。隐式 HTTP、SSH、`file://` 和任意本机路径均拒绝。
3. `owner/repo/path@ref` 在执行前解析为不可变 commit SHA；实际 SHA、来源 host 和路径写入审计记录。实例策略可强制 workflow 直接使用完整 SHA。
4. 下载结果按 `host/owner/repo/commit-sha` 隔离缓存；校验归档大小、展开大小、文件数、路径穿越和越界 symlink。
5. 默认不递归拉取 Action 仓库 submodule；确需支持时必须成为显式策略和测试项。
6. mutable tag/branch 只用于解析，执行和重跑必须复用首次记录的 SHA，避免同一 Pipeline 漂移。

### 2.4 首期兼容范围

按以下顺序实现，而不是宣称完整 GitHub/Gitea Actions 兼容：

| Action 类型 | 决策 |
|---|---|
| `actions/checkout`、`actions/cache` | 保留现有内置适配，并迁入结构化 step 模型 |
| 本地 composite Action | 第一阶段支持；所有子 step 继续受本 ADR 策略约束 |
| 远程 composite Action | `ActionResolver` 和隔离执行器完成后支持 |
| Node.js Action | 第一阶段只支持声明 `node20` 的 JavaScript Action，在 Job 容器内执行 |
| Docker Action | `CI-204` 完成临时网络和 service container 隔离后支持；禁止复用宿主 Docker socket |
| Gitea/Go Action 和插件式本机二进制 | 首期不支持，显式 fail closed |
| 未知 `runs.using`、嵌套权限提升或所需 Runner 能力缺失 | Pipeline 创建或调度前 fail closed |

首批兼容测试集由 `CI-208` 固定具体 Action 与版本，不能用“任意第三方 Action”作为模糊验收结论。

### 2.5 容器安全基线

Action Job 的容器必须满足：

- 禁止 `--privileged`、Docker socket、设备映射和任意宿主路径挂载；
- 只挂载该 Job 的工作区，以及由 Runner 管理的 cache/artifact 暂存目录；
- `cap-drop=ALL`、`no-new-privileges`，不发布入站端口；
- 配置 CPU、内存、PID、磁盘配额和总 timeout；超时取消必须终止整个容器和后代进程；
- root filesystem 首期允许临时可写以兼容 Action setup，但 Job 后销毁，不作为持久化状态；
- Job image 受实例 allowlist/digest 策略约束，审计记录实际 image digest；
- Runner 工作区、Action cache、artifact staging 按实例/仓库/Pipeline/Job 分层，拒绝路径穿越和跨租户读取。

在上述基线落地前，第三方 `uses:` 继续保持显式拒绝。

### 2.6 网络策略

网络 profile 是 Runner capability 和 Job 策略的一部分：

| Profile | 行为 | 默认用途 |
|---|---|---|
| `none` | 无出站网络，无入站端口 | 默认 Action Job |
| `proxy` | 仅经实例管理的 egress proxy 访问 allowlist | 下载依赖、访问本实例 API |
| `full` | 允许普通出站网络，仍禁止入站暴露 | 仅管理员显式批准的受信 workflow |

Action 源码由 Runner 控制面在 Job 启动前解析和获取，不因 Action Job 的 `none` profile 自动开放网络。`CI-204` 引入的 service containers 只能加入该 Job 的临时网络，Job 外不可达。请求的 profile 无匹配 Runner 或违反仓库/实例策略时 fail closed。

### 2.7 Secret、Token 与 fork Pull Request

1. Job 默认只获得表达式中**实际引用**且策略允许的 Secret，不再注入仓库全部 Secret。
2. Secret 名称在服务端解析和授权；值在调度确认后按 Job 即时传递，不进入命令行、Action cache、审计字段或普通执行计划。
3. 日志 masking 是纵深防御，不是安全边界；结构化日志和 artifact 上传仍要扫描已知 Secret 值及常见编码形式。
4. 来自 fork 的 Pull Request 默认不运行；仓库维护者批准后才能运行，并且始终不注入仓库、组织、用户或 environment Secret。
5. fork Pull Request 的 `CI_JOB_TOKEN` 强制只读，不能写仓库、Package、Release、Issue 或其他资源，也不能访问其他私有仓库。
6. 解析 Actions `permissions`：workflow/job 可以减少默认权限，不能超过实例、仓库、事件来源和 Runner 策略共同允许的上限。
7. `pull_request_target` 在建立“受信基准 workflow + 不受信 fork 内容”分离模型之前不支持，遇到时显式拒绝。
8. environment 审批在 Secret 发放前完成；取消或超时后令牌失效，未开始的 Secret 不发送。

### 2.8 审计和失败语义

每个 Action Job 至少记录：

- workflow commit、触发事件和是否来自 fork；
- Action 原始引用、解析后的 commit SHA、Action 类型；
- Runner ID/capability、Job image digest、网络 profile；
- 获准的 Secret **名称**、Token 最终 scope、审批人和审批时间；
- 每个 step 的开始/结束/退出状态，以及取消或策略拒绝原因。

不得记录 Secret 值或带凭证的下载 URL。来源、类型、权限、网络、Runner 或隔离能力不满足策略时，应在尽可能早的阶段 fail closed，不允许转换成空 step 或成功状态。

## 3. 明确不采用的方案

| 方案 | 拒绝原因 |
|---|---|
| 在 IronForge 服务主机上把 `uses:` 翻译为 shell 执行 | 等同允许仓库内容在控制面远程执行代码，无法建立可信边界 |
| 继续把所有 step 拼成单个 script | 无法正确实现 outputs、生命周期、运行时条件、权限和逐 step 审计 |
| 默认给完整网络和全部仓库 Secret | 第三方 Action 供应链风险会直接放大为凭证泄漏和横向移动 |
| 静默忽略未知 Action/字段 | 产生“CI 成功但实际未执行”的错误结论 |
| 直接把 Gitea `act_runner` 当作内部库嵌入 | 引入独立 Go runtime/协议和强耦合，且不能替代 IronForge 自身的权限与审计模型 |

## 4. 实施关卡

| 关卡 | 对应任务 | 通过条件 |
|---|---|---|
| A：执行计划 | `CI-202`、`CI-203` | 结构化 step、outputs、生命周期、运行时条件和 fail-closed 测试完成 |
| B：沙箱与网络 | `CI-204` | 临时容器/网络、资源限制、service container 和 Secret 脱敏验收完成 |
| C：Action runtime | `CI-206` | Resolver、SHA 固定、composite/Node20 执行和 Runner capability 匹配完成 |
| D：兼容回归 | `CI-208` | 固定版本的常用 Actions、fork/Secret/网络负向场景进入 CI |

`CI-201` 的失败 Job 重跑可独立实施，但重跑 Action Job 时必须复用首次解析的 Action SHA 和原执行策略。

## 5. 验收清单

- [ ] 不受信 `uses:` 永远不会落到 IronForge 服务主机或普通宿主机 Runner；
- [ ] 无 `actions-v1`、容器或网络能力的 Runner 会被调度器拒绝；
- [ ] mutable ref 被解析、记录并在重跑中固定到同一 SHA；
- [ ] Action 不能获得 Docker socket、privileged、设备或任意宿主挂载；
- [ ] 默认网络为 `none`，`proxy/full` 只能由策略显式放行；
- [ ] Job 只获得已引用且获准的 Secret，值不出现在命令行、日志、审计和 cache；
- [ ] fork Pull Request 需要审批且没有 Secret，Token 权限被压到只读；
- [ ] 未知 Action 类型、来源、权限或表达式在执行前 fail closed；
- [ ] composite、Node20 和后续 Docker Action 均有正向及越权负向回归；
- [ ] 官方常用 Actions 以固定版本进入 `CI-208` 真实兼容矩阵。

## 6. 结果与已知差距

本决策优先保护 IronForge 控制面和部署主机，代价是初期兼容范围小于 Gitea `act_runner`。需要网络、Docker socket、自定义宿主工具或未知 runtime 的 Action 将继续失败，直到有显式能力和隔离设计。

`CI-200` 完成后，矩阵中的“Gitea Actions YAML 有限适配”仍为 `PARTIAL`、得分不变；只有 `CI-206` 与 `CI-208` 完成真实第三方 Action 执行和兼容回归后，才重新评分。
