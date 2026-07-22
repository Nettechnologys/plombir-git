## IronForge 项目审计与进度报告

**审计日期：** 2026-06-06

---

### 项目概况

IronForge（铁匠铺）是一个纯 Rust 实现的轻量级 Git 托管平台，对标 Gitea/Forgejo。项目采用 Cargo workspace 组织，包含 9 个 crate，总计约 28,800 行 Rust 代码、174 个源文件、23 个数据库迁移。前端使用 SvelteKit 5（SPA 模式），已构建产出 build 目录。Release 二进制产物包括 ironforge（28MB）、ironforge-runner（3.4MB）、ironforge-mcp（2.4MB）。

---

### 一、功能完成度

README 声称所有已规划功能均为"完成"状态。经代码验证，绝大部分功能确实已实现，少数存在"名义完成但实际有缺陷"的情况。

| 功能 | 声称状态 | 实际状态 | 说明 |
|------|---------|---------|------|
| Git clone/push (HTTPS) | 完成 | 完成 | Smart Protocol V1/V2 均已实现 |
| Git clone/push (SSH) | 完成 | 完成 | russh 0.51，公钥/密码认证 |
| 用户注册/登录 | 完成 | 完成 | argon2 + JWT HS256 |
| 仓库管理 | 完成 | 完成 | SeaORM + SQLite REST API |
| Issue CRUD + 评论 | 完成 | 完成 | 标签/里程碑/评论 |
| Pull Request + Diff | 完成 | 完成 | 三种合并策略 |
| Wiki CRUD | 完成 | 完成 | 页面 CRUD |
| Git LFS | 完成 | 部分完成 | 后端实现完整，但 **LFS 端点无鉴权** |
| Webhook | 完成 | 部分完成 | 后端实现完整，但 **前端无管理页面** |
| CI/CD Pipeline | 完成 | 完成 | YAML 解析 + 后台执行 + Docker Runner |
| 代码审查 (PR Review) | 完成 | 完成 | approve/request_changes/comment/dismiss |
| 分支保护 | 完成 | 部分完成 | 后端实现完整，但 **前端无管理页面** |
| 组织/团队系统 | 完成 | 完成 | organization + team + 成员管理 |
| 通知系统 | 完成 | 完成 | WebSocket 实时推送 |
| 邮件通知 | 完成 | 完成 | SMTP + HTML 模板 |
| TLS/HTTPS | 完成 | 完成 | axum-server + rustls |
| API Rate Limiting | 完成 | 部分完成 | Token Bucket 存在但可被伪造 IP 绕过 |
| GPG 签名验证 | 完成 | 完成 | git log --format=%G? |
| Git Smart Protocol V2 | 完成 | 完成 | ls-refs/fetch |
| Release 管理 | 未声称 | 完成 | 含 asset 上传 |
| MCP 服务 | 未声称 | 新增 | rg-mcp crate（5 月 26 日创建） |

**前端缺失页面：** 用户个人设置页、Release 编辑页、Webhook 管理页、分支保护规则管理页、自定义 404 页面、用户公开主页。

**前端页面完成度：约 85%**（28 个页面覆盖核心功能，缺少辅助页面）。

---

### 二、严重缺陷（Critical）

以下问题可直接导致安全风险或数据泄漏，建议立即修复。

**C-01: Runner 注册与管理员端点无鉴权**

`POST /api/v1/runners/register`、`GET /api/v1/admin/runners`、`DELETE /api/v1/admin/runners/:id` 三个端点完全没有身份验证。代码中存在 `// TODO: authenticate as admin` 注释。攻击者可注册恶意 Runner 截取 CI Job（含代码和密钥），或随意删除合法 Runner。

文件：`rg-http/src/api/runners.rs`

**C-02: CI 脚本执行无沙箱隔离**

本地执行模式直接通过 `sh -c <script>` 运行用户提交的 CI 脚本，无 chroot、namespace、cgroup 限制，无资源限制。执行权限等同于 IronForge 服务进程。Docker 模式不可用时会静默回退到本地执行。

文件：`rg-ci/src/runner.rs`

**C-03: SSH 公钥指纹使用 DefaultHasher 而非 SHA-256**

`sha256()` 函数名暗示使用 SHA-256，但实际使用 Rust 标准库的 `DefaultHasher`（SipHash 变体），生成的 32 字节数组后 24 字节全为 0。这会导致指纹碰撞概率极高，与标准 SSH 指纹格式不兼容。

文件：`rg-core/src/auth/ssh_key.rs`

**C-04: Job Log WebSocket 无认证**

`/api/v1/ws/job/{job_id}` WebSocket 端点注释写着 "Does not require JWT auth"。任何未认证客户端可监听任意 Job 的实时日志，CI 日志中通常包含环境变量、部署密钥等敏感信息。

文件：`rg-http/src/ws.rs`

---

### 三、高危缺陷（High）

**H-01: 多个关键端点缺少鉴权**

LFS 上传/下载（`PUT/GET /lfs/objects/:oid`）、LFS batch API、AI 端点（`/ai/repos/...`）、仓库内容浏览（`/tree`、`/blob`、`/log`、`/branches`、`/tags`）均无身份验证，私有仓库内容可被未授权用户读取。

**H-02: 路径遍历风险**

SSH 命令解析和 HTTP Git 端点的路径拼接未检查 `..` 遍历字符。`repo_root.join(&repo_path)` 可能导致访问仓库目录之外的文件。

文件：`rg-ssh/src/lib.rs`、`rg-http/src/lib.rs`

**H-03: 用户名/邮箱缺乏格式校验**

注册接口只检查密码长度 >= 8，对 username 和 email 无格式验证。恶意用户名（如 `../../admin`）可能被用于路径构造攻击。

文件：`rg-core/src/user/service.rs`

**H-04: CORS 配置过于宽松**

`CorsLayer::permissive()` 允许任何来源、方法、请求头。恶意网站可通过用户浏览器发起跨域请求，利用认证 token 操作仓库。

文件：`rg-http/src/lib.rs`

**H-05: 内部错误信息泄漏**

约 90 处将 `anyhow::Error` 的 `to_string()` 直接作为 HTTP 响应返回，暴露数据库结构、内部路径、函数调用栈等敏感信息。同时所有 anyhow::Error 都映射为 500，业务逻辑错误（如"仓库已存在"、"权限不足"）无法与服务器故障区分。

**H-06: 前端编译错误**

`settings/+page.svelte` 引用了不存在的 `$lib/types` 模块，会导致编译失败。`createT()` 的解构方式也不正确。

---

### 四、中危缺陷（Medium）

**M-01: gix 集成不完整** — 仍有 37 处 `Command::new("git")` 调用（pack-objects、index-pack、rebase、merge、diff 等），部署环境必须安装 git。

**M-02: `extract_user_id` 函数重复 9 次** — 9 个 API 模块中完全相同的 JWT 解析函数，违反 DRY 原则，应提取为共享 Axum Extractor。

**M-03: Rate Limiter 可被绕过** — 依赖可伪造的 `X-Forwarded-For` 头识别客户端 IP，攻击者可在每个请求中使用不同 IP 值绕过限流。HashMap 无容量上限，存在内存耗尽风险。

**M-04: 生产代码中的 `.unwrap()`** — 约 40 处生产代码使用 `.unwrap()`，配合 release profile 的 `panic = "abort"` 设置，一次 panic 会导致整个服务器进程退出。

**M-05: `serde_yaml` 已废弃** — workspace 依赖使用 `serde_yaml = "0.9"`，该 crate 已被作者标记为 deprecated，不再接收安全更新。

**M-06: JWT Token 无法撤销** — 无 token 黑名单机制，签发后 7 天内始终有效，用户登出或改密码后旧 token 仍可用。

**M-07: rg-core 缺少领域错误类型** — 核心业务层全局使用 `anyhow::Result`，已声明 `thiserror` 依赖但未使用，无法区分 NotFound/Forbidden/Conflict 等不同语义。

---

### 五、测试覆盖

项目共有 68 个测试（33 个同步 + 35 个异步），集中在 pkt-line 协议（17 个）、分页逻辑（14 个）、HTTP API 集成（10 个）、JWT（8 个）、CI 配置解析（7 个）等模块。

**完全无测试的关键模块：**

| 模块 | 风险 |
|------|------|
| rg-ssh（SSH 服务器、公钥鉴权、命令解析） | 极高 |
| rg-ci/runner（CI 执行引擎） | 极高 |
| rg-core/repo/service（仓库访问控制 can_read/can_write） | 极高 |
| rg-core/auth/ssh_key（SSH 密钥指纹计算） | 极高 |
| rg-core/pull_request（PR 创建/合并逻辑） | 高 |
| rg-core/lfs/service（LFS 存储） | 高 |
| rg-core/webhook（Webhook 投递） | 高 |
| rg-git/protocol/upload_pack、receive_pack（Git 协议核心） | 高 |
| rg-db（数据库操作层） | 高 |
| rg-http/api/admin（管理员 API） | 高 |

现有测试以正向路径为主，缺少负面测试（恶意输入、越权访问、边界条件）。无 benchmark 测试。

---

### 六、前端质量

前端在技术栈选型上较为现代（Svelte 5 + SvelteKit 2 + Vite 8 + TypeScript strict），302 个中英文翻译 key 完全对称，API 路径与后端 100% 匹配。

主要问题集中在工程化不足：仅 4 个可复用组件（其中 1 个未使用），大量 CSS 和 UI 逻辑在页面间重复；缺少 ESLint/Prettier 配置；广泛使用 `any` 类型丧失了 TypeScript 类型保护；存在编译错误（`$lib/types` 不存在）。

---

### 七、技术债清单（TODO/FIXME）

| 位置 | 内容 | 影响 |
|------|------|------|
| rg-core/repo/service.rs | `TODO(gix): Local bare clone` | fork 依赖 git CLI |
| rg-git/protocol/v2.rs | `TODO(gix): Replace with gix pack generation` | V2 协议依赖 git CLI |
| rg-git/protocol/receive_pack.rs（2 处） | `TODO(gix): Replace with gix pack indexing` | push 依赖 git CLI |
| rg-git/protocol/upload_pack.rs | `TODO(gix): Replace with gix pack generation` | clone 依赖 git CLI |
| rg-core/pull_request/service.rs | `TODO(gix): Replace rebase with gix rebase API` | rebase 依赖 git CLI |
| rg-http/api/repo_content.rs | `TODO: extract commit time` | author_date 返回空字符串 |
| rg-http/tests/api_tests.rs | `TODO: toggle_star returns 500` | star 切换已知 bug |
| rg-http/api/runners.rs | `TODO: authenticate as admin` | admin 端点无认证 |

---

### 八、安全亮点（做得好的地方）

项目整体也有许多值得肯定的安全实践：全项目 0 处 unsafe 代码；拒绝使用默认 JWT secret；Access Token 存储 SHA-256 哈希而非明文；SSH 认证正确设置 `partial_success: false`；启动前校验 repo_root 可写性和 TLS 文件存在性；Release profile 启用 LTO/strip/panic=abort；使用 `x-request-id` 实现全链路请求追踪；日志中无密码/token 明文泄漏。

---

### 九、修复优先级建议

**立即修复（1-2 周）：**

1. Runner 注册和管理端点添加 admin 鉴权中间件
2. Job Log WebSocket 添加 JWT 认证
3. 修复 SSH 密钥指纹函数，使用 sha2 crate 实现真正的 SHA-256
4. LFS、仓库内容浏览、AI 端点添加鉴权
5. 修复前端 `$lib/types` 编译错误

**短期修复（2-4 周）：**

6. 所有路径拼接处添加路径遍历防护
7. 添加用户名/邮箱格式校验
8. 内部错误消息改为通用响应，详细信息仅写入日志
9. 引入 rg-core 领域错误枚举，区分 4xx/5xx
10. CORS 从 permissive 改为白名单配置

**中期改进（1-2 月）：**

11. 提取共享的 Auth Extractor 消除 `extract_user_id` 重复
12. 为 SSH 服务器、CI 执行引擎、访问控制、PR 逻辑补充测试
13. 消除生产代码中的 `.unwrap()` 调用
14. 前端组件化重构（Modal、Pagination、Form 等通用组件）
15. 补充用户个人设置页、Webhook 管理页、分支保护管理页

**长期规划（2-6 月）：**

16. 逐步将 37 处 git CLI 调用替换为 gix 0.83 API
17. Rate Limiter 添加容量上限，改用不可伪造的客户端标识
18. CI 本地执行添加沙箱隔离（chroot/cgroup/独立用户）
19. JWT Token 撤销机制
20. 添加 benchmark 测试套件
