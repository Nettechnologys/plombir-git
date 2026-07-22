# IronForge 架构差异与后续待办（2026-07）

**生成日期**: 2026-07-05  
**最近复核**: 2026-07-13
**来源**: `project-architecture-analysis-notes-2026-07.md` 第 0-10 轮 + 修复波次回填  
**排序原则**: 优先列出安全、权限、可用性、部署失败和文档误导风险。

---

## 已修复（2026-07-05）

| 问题 | 修复范围 | 验证 |
|------|----------|------|
| `GET /users/me` 等接口仍只读 Bearer，不读 HttpOnly cookie | `users/me`、PAT token 管理、Admin helper、SSO refresh/unlink 改为复用 `extract_user_id`，支持 HttpOnly cookie 或 Bearer | `cargo check -p rg-http --tests`；`test_me_accepts_httponly_cookie_without_bearer`；`admin_sso_accepts_httponly_cookie_without_bearer` |
| MFA disable 未检查 `verify_password` 返回的 `bool` | `crates/rg-http/src/api/mfa.rs::disable_mfa` 显式拒绝 `verify_password == false` | `test_disable_mfa_rejects_wrong_password` |
| SSO callback 缺少 state 时继续执行 | `crates/rg-http/src/api/sso.rs::callback` 缺失 query state 时返回 forbidden | `cargo check -p rg-http --tests` |
| Pipeline API 未看到认证或仓库权限检查 | list/get/job 接入 `can_read_repo`，trigger/retry/cancel 接入 `can_write_repo`，并绑定 pipeline/job 到 URL 仓库，修复跨仓库 IDOR 风险 | `cargo check -p rg-http --tests`；`cargo test -p rg-http --test ci_permission_tests -- --nocapture` |
| Package REST/protocol endpoints 权限边界不完整 | REST 读、protocol metadata/download 接入 `can_read_repo`；publish/delete/yank 接入 `can_write_repo`；publish 显式调用 adapter validate；补 package 表名单复数纠偏迁移 | `cargo check -p rg-http --tests`；`cargo test -p rg-http --test package_permission_tests -- --nocapture` |
| OCI anonymous pull 未见 repo visibility 校验 | `/v2` pull/push handlers 改为基于 IronForge repo `can_read_repo` / `can_write_repo`；token endpoint 只签发实际允许的 scope；测试 router 补挂 `/v2` | `cargo check -p rg-http --tests`；`cargo test -p rg-http --test oci_permission_tests -- --nocapture` |
| SSH Git 未看到 repo-level `can_read/can_write` | SSH exec path 在启动 `git-upload-pack` / `git-receive-pack` 前解析 `owner/repo`，并分别接入 `can_read_repo` / `can_write_repo`；无 DB 模式保持 Phase 1 兼容开放 | `cargo check -p rg-ssh -j1`；`git diff --check`；`cargo test -p rg-ssh -j1 -- --nocapture` 两次卡在依赖测试编译阶段后中断 |
| Runner CLI 自动注册不带 Authorization，但后端注册要求 Bearer JWT | `/runners/register` 收紧为 admin-only 且支持 HttpOnly cookie；`ironforge-runner register/run` 与 `ironforge runner` 自动注册新增 `--auth-token` / `IRONFORGE_AUTH_TOKEN`，已有 runner id/token 路径不受影响 | `cargo test -p rg-http --test runner_auth_tests -- --nocapture`；`cargo check -p rg-runner`；`cargo check -p rg-cli` |
| `deploy/docker-compose.yml` 使用被拒绝的默认 secret；Prometheus target 端口不一致 | 新增 `deploy/.env.example`，主 compose 改为读取 `deploy/.env`，移除被拒绝的默认 secret；主服务和观测栈共享 `ironforge-net`，Prometheus 改抓 `ironforge:8080`；部署 README 补生成 secret 步骤 | `docker compose -f deploy/docker-compose.yml config`；`docker compose -f deploy/docker-compose.observability.yml config` |
| 缺少正式 CI workflow | 新增 `.github/workflows/regression.yml`，覆盖 Rust test compile、`rg-http` 集成测试、workspace tests、fresh SQLite migration smoke、前端 `npm run check/build` 和 compose config 静态校验 | `ruby -e "require 'yaml'; YAML.load_file('.github/workflows/regression.yml')"`；`git diff --check` |
| LDAP TLS 证书校验禁用 | `LdapConfig` 新增显式 `insecure_skip_tls_verify`，LDAP 连接默认不再调用 `set_no_tls_verify(true)`；仅 LDAPS 且显式 insecure 时跳过证书校验 | `cargo check -p rg-core`；`cargo test -p rg-core ldap -- --nocapture` 卡在测试 profile 依赖编译阶段后中断 |
| SSO callback 返回 JSON，未见设置 auth cookie 或回跳前端 | OAuth callback URL 改为 `/api/v1/auth/sso/{slug}/callback` 且优先使用 `external_url`；state/PKCE cookies 使用 `append` 且 `Path=/`；callback 成功后设置 `ironforge_token` HttpOnly cookie 并 redirect `/dashboard`；MFA 用户 redirect 到登录页并进入 MFA 表单 | `cargo check -p rg-http --tests`；`cargo test -p rg-http sso_ -- --nocapture`；`npm run build`；`npm run check` 卡在 `svelte-kit sync` 后中断 |
| Rate limit 信任 `X-Forwarded-For` | 限流默认只按 socket IP 计数；新增 `rate_limit.trusted_proxies` / `--rate-limit-trusted-proxies`，只有可信代理来源才读取 `X-Forwarded-For` / `X-Real-IP` | `cargo test -p rg-http rate_limiter -- --nocapture`；`cargo check -p rg-http --tests`；`cargo check -p rg-cli` |
| 外部 runner Docker 不可用时回退 local | `ironforge-runner` 指定 `image` 的 job 在 Docker 不可用时直接失败并上报日志，不再调用 local shell fallback；内置 runner 已保持同样 fail-closed 语义 | `cargo test -p rg-runner docker_unavailable -- --nocapture`；`cargo check -p rg-runner` |
| CI_JOB_TOKEN 生成后 HTTP handler 未接入验证 | `repo_content` / `archive` 读路径接入同仓库 `repo:read` CI token；Package REST/protocol 读路径接入同仓库 `packages:read` CI token；写接口仍只接受用户身份 | `cargo test -p rg-http --test ci_job_token_tests -- --nocapture`；`cargo check -p rg-http --tests` |
| MCP tools/resources 可能无 Tokio runtime | `ironforge-mcp` 启动时创建并进入单 worker Tokio runtime，现有同步 JSON-RPC dispatch 中的 async HTTP 调用不再因缺少 runtime panic | `cargo check -p rg-mcp`；stdio `tools/call list_repos` smoke 返回 JSON-RPC 错误文本而非 panic |
| `/metrics` 未初始化时 panic | `metrics_handler` 在 registry 未初始化时返回 503 文本响应，不再 `expect` panic | `cargo test -p rg-http metrics_handler_returns_503 -- --nocapture`；`cargo check -p rg-http --tests` |
| Artifact API 只管理 metadata | runner artifact raw body 上传会保存到 `{repo_root}/_artifacts` 并落 DB；新增 `/artifacts/{id}/download`；list/get/download/delete 均校验 artifact 所属 pipeline 的 repo read 权限 | `cargo test -p rg-http --test artifact_file_tests -- --nocapture`；`cargo test -p rg-http --test runner_auth_tests -- --nocapture`；`cargo check -p rg-http --tests` |
| 缺少 DB 备份/恢复方案 | 新增 `ironforge backup-db` 和 `ironforge restore-db`；备份使用 SQLite `VACUUM INTO`；恢复默认拒绝覆盖，需服务离线并显式 `--force`；部署文档补容器命令 | `cargo check -p rg-cli`；`cargo build -p rg-cli` 本地链接阶段超时后中断，未完成 runtime smoke |
| Docker 镜像只包含主服务二进制 | Docker runtime 镜像现在同时构建并复制 `ironforge`、`ironforge-runner`、`ironforge-mcp`；compose 可选 runner 示例改用独立 runner 二进制 | `rg -n "ironforge-runner|ironforge-mcp|cargo build --release --bin" Dockerfile deploy/docker-compose.yml deploy/README.md`；`git diff --check` |
| Git receive-pack 分支保护不是 pre-receive 强拦截 | `rg-git` receive-pack 新增 rejected refs 前置拒绝入口；HTTP Git 和 SSH Git 在读 pack/写 ref 前加载 protected branch 规则并把禁止直推的 `refs/heads/*` 标记为 `ng` | 静态检查通过；`cargo check -p rg-git` 本地 `rustc` 睡眠挂起，已中断，需后续环境复跑 |
| MCP `--sse` 参数未实现但文档容易误写为可用能力 | `rg-mcp` 注释改为 stdio-only；`--sse` 现在返回非零错误；AGENT/AGENTS 与 AI Agent 集成文档统一写 stdio 可用、SSE 未实现 | `cargo check -p rg-mcp`；`cargo run -p rg-mcp -- --sse` 返回错误 |
| job log WebSocket 后端有、前端未接入 | 前端 API client 新增 `connectJobLogWebSocket(jobId)`；pipelines 日志弹窗打开时订阅 `/ws/job/{job_id}`，实时追加 runner log chunk，关闭/切换时清理连接 | `npm run build` |
| CSP `connect-src 'self'` 与跨域 API base 不兼容 | CSP `connect-src` 现在会从 `IRONFORGE_CORS_ORIGINS` 和 `IRONFORGE_CSP_CONNECT_SRC` 生成，并自动加入 HTTP(S) origin 对应的 WS(S) origin | `cargo test -p rg-http security::tests::connect_src -- --nocapture`；`cargo check -p rg-http --tests` |
| README/CONTRIBUTING 有旧 E2E 段落 | README 和 CONTRIBUTING 不再引用待创建脚本或长篇临时 shell 片段；统一指向 `full-interface-regression`、OpenAPI smoke、console smoke 和 API client contract check | `rg -n 'scripts/e2e_test\\.sh|端到端测试脚本|待创建' README.md CONTRIBUTING.md` 无结果；`git diff --check` |
| Package 17 种格式与专用协议支持不一致 | 前端 package 格式元数据区分 native adapter 和 Generic fallback；列表/上传页对 `go/conan/conda/alpine/debian/rpm/swift` 标注 Generic fallback，避免误写为完整专用协议 | `npm run build` |
| 旧拆分 API 文件重复实现风险 | `auth.ts/repos.ts/...` 旧领域文件改为从 `client.svelte.ts` 显式 re-export，避免重复实现继续漂移，同时保留老导入路径兼容性 | `npm run build` |
| Markdown sanitizer allowlist | `sanitizeHtml` 改为浏览器 DOMParser + 标签/属性/URL allowlist，非浏览器 fallback 也按 allowlist 收紧，并新增 sanitizer smoke | `npm run smoke:markdown-sanitizer`；`npm run build` |
| API client WebSocket helper 混在聚合文件 | notification/job log WebSocket helper 抽到 `websockets.ts`，`client.svelte.ts` 继续 re-export 保持调用方兼容 | `npm run build` |
| Job log WebSocket 全局广播、连接任务残留 | 后端改为每个 `job_id` 独立 broadcast channel，通知端不再接收 Job 日志；连接退出同步释放 receiver 并回收空 channel，订阅升级前校验 Job 所属仓库读取权限 | `cargo test -p rg-http --lib ws::tests`；`cargo test -p rg-http --test job_websocket_tests` |
| LFS action URL 无签名/过期分级，公有仓库上传未强制写权限 | Batch action URL 改为 HMAC-SHA256 绑定用途、仓库、OID 与过期时间；下载 1h、上传 6h，过期返回 410；上传 Batch/直传对公有仓库同样要求写权限，合法签名可承接 Batch 授权 | `cargo test -p rg-http --test lfs_signed_url_tests` |
| API client Package Registry 混在聚合文件 | Package Registry API 抽到 `packages.ts`，`client.svelte.ts` 继续 re-export `packages` 保持页面导入兼容 | `npm run build` |
| API client Runner 管理混在聚合文件 | Runner 管理 API、响应类型和 labels 归一化抽到 `runners.ts`，`client.svelte.ts` 继续 re-export 保持页面导入兼容 | `npm run build` |
| API client Boards/Time/Search 混在聚合文件 | Boards、Time Tracking、Search API 分别抽到 `boards.ts`、`timeTracking.ts`、`search.ts`，主入口继续 re-export 保持页面导入兼容 | `npm run build` |
| API client Auth/Releases/Issues/PR/CI/Wiki 混在聚合文件 | Auth、Releases、Issues、Pull Requests/Reviews、Pipelines、Wiki API 分别抽到独立领域文件，主入口继续 re-export 保持页面导入兼容 | `npm run build` |
| API client 剩余领域仍在聚合文件 | Repos、Collaborators、Labels、Notifications、Orgs、Branch Protection、Mirrors、Webhooks、Imports、Milestones、Tokens、MFA、Admin 全部抽到独立领域文件，`client.svelte.ts` 只保留兼容 re-export | `npm run build` |
| PR rebase 在 bare 仓库直接执行导致必然失败 | rebase 改为唯一临时工作区执行，成功后以普通 fast-forward push 更新目标分支；冲突或并发推进不会覆盖 base，并统一清理临时目录；同仓库与 fork 共用实现 | `cargo test -p rg-http --test pr_merge_strategy_tests -- --nocapture`（merge/squash/rebase 拓扑与冲突恢复共 2 项通过） |
| SSH push 缺少真实协议回归 | 新增临时 russh 服务 + 数据库注册 Ed25519 密钥 + 系统 OpenSSH/Git 全链路测试，覆盖 push、clone、bare ref/内容和 `last_used_at` | `cargo test -p rg-ssh --test ssh_push_tests -- --nocapture` |
| 通用 OIDC discovery 未用于 token/userinfo，callback 缺 PKCE verifier 时继续请求 | authorize/token/refresh/userinfo 统一解析 discovery document；HTTP 错误 fail closed；callback 缺 verifier 直接 403；新增本地模拟 OIDC 的 S256、Cookie、token、userinfo、账户创建全链路回归 | `cargo test -p rg-http --test oauth_pkce_tests -- --nocapture` |
| Package 9 类原生 REST/协议缺少统一 E2E | 为 Cargo/npm/Maven/PyPI/NuGet/RubyGems/Helm/Composer/Generic 生成最小合法制品，覆盖发布、列表、版本、下载和 8 类专用索引；同时修复 npm publish/list 路由冲突、RubyGems `.json` 查询及 Composer dist URL 缺版本/编码 | `cargo test -p rg-http --test package_format_e2e_tests -- --nocapture` |
| 审计归档器未接入且固定 NDJSON 文件可能覆盖 | 服务启动默认接入可配置归档任务；按 cutoff oldest-first 限量读取，唯一 `.ndjson.zst` 文件、临时文件原子重命名并在落盘后删除 DB；支持关闭、保留天数、间隔和批大小配置 | `cargo test -p rg-core --lib audit::archiver -- --nocapture`；`cargo check -p rg-cli -p rg-core --tests` |

---

## P0：应优先修复

当前 P0 已清零。

---

## P1：高优先级增强

当前 P1 安全/部署强优先级项已清零。剩余体验、文档口径和中长期生产化任务见 P2。

---

## P2：中期整理

| 问题 | 影响 | 建议 |
|------|------|------|
| API client 新增领域回归到聚合入口 | 前端 API 维护成本可能再次抬升 | 当前 `client.svelte.ts` 已降至 38 行纯 re-export；后续新增领域应优先放独立模块，再由主入口 re-export |
| 多数据库生产化余量 | SQLite/PostgreSQL/MySQL 已完成首轮实库迁移、CRUD、FTS、并发认证与服务启动验证，但尚未覆盖版本矩阵、备份恢复演练、HA 和长期压测 | 以支持版本矩阵和恢复目标为边界补生产验证；不再把“实现 PostgreSQL/MySQL 基础支持”列为待办 |
| MCP SSE transport | 当前 MCP 仅 stdio 可用，网页端 Agent 场景仍缺 transport | 等 HTTP/SSE server 设计明确后再实现；现阶段继续保持 `--sse` fail-fast |
| Package 专用协议补全 | `go/conan/conda/alpine/debian/rpm/swift` 仍按 Generic fallback 展示 | 根据真实用户需求逐个补 adapter，不把 type 枚举写成完整协议支持 |
| gix 后续迁移 | pack/rebase/archive/unified diff 等仍依赖 git CLI 或等待上游能力 | 每次 gix 升级复查阻塞表，优先迁移可对拍验证的只读路径 |

---

## 文档口径修正

以下表述在最终文档中应避免或修正：

| 旧口径 | 当前应写成 |
|--------|------------|
| Phase 1-21 全部完成即可代表功能闭环 | 以当前代码路径为准，部分安全/权限/运维闭环仍有缺口 |
| PostgreSQL/MySQL 支持 | 默认 SQLite，同时支持 PostgreSQL/MySQL URL；2026-07-13 已完成真实服务首轮 smoke，版本矩阵、备份恢复、HA 与压测仍是生产化余量 |
| MCP 支持 stdio/SSE | 当前 stdio 可用，`--sse` 未实现 |
| 前端登录态已完全迁移到 HttpOnly cookie | 关键浏览器用户 API 已支持 cookie；其他领域接口仍需按功能逐步复查 |
| Package Registry 支持 17 种完整协议 | 17 种 type 枚举存在；其中 Cargo/npm/Maven/PyPI/Docker/NuGet/RubyGems/Helm/Composer/Generic 有 native adapter，其余 type 走 Generic fallback |
| Artifact 管理完整 | ✅ 已补 runner raw 上传、服务端文件保存、下载端点和 repo read 权限 |
| Docker compose 可直接启动 | 需要先从 `deploy/.env.example` 生成 `deploy/.env` 并设置强 `IRONFORGE_JWT_SECRET`；镜像内已包含 `ironforge` / `ironforge-runner` / `ironforge-mcp` |
| API docs 不支持 PAT | 本轮核验有测试覆盖 JWT 和 PAT 均可访问 docs |

---

## 建议执行顺序

1. 先冻结本轮 P0/P1 修复成果：保留现有回归测试入口，避免认证、权限、部署和 API client 结构再次漂移。
2. 回填最终架构文档：把“已修复事实”和“长期方向”分开写，避免 Phase 完成状态被误读为生产化全闭环。
3. 再推进长期能力：数据库版本矩阵与恢复演练、MCP SSE、Package 专用协议补全、gix 后续迁移。
