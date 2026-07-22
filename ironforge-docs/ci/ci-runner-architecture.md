# IronForge CI/CD Runner 调度系统架构设计

> **当前安全边界（2026-07-14）**：本文记录 2026-05 的基础调度设计。第三方 Actions 的现行决策以 [ADR-0001：第三方 Actions 执行与安全边界](./adr-0001-third-party-actions-execution.md) 为准；现有宿主机执行和基础 Docker 参数不得视为不受信 Action 的安全沙箱。

**版本**: v1.0  
**日期**: 2026-05-10  
**设计人**: 齐活林 (Qi) · 交付总监  
**项目**: IronForge (Rust Git 托管平台)

---

## 一、设计目标

### 1.1 核心目标
- ✅ 实现 Runner 调度系统，支持多 Runner 并发执行 Job
- ✅ 支持 Runner 动态注册/下线
- ✅ 支持 Job 分发与负载均衡
- ✅ 支持 Job 日志实时上报与查询
- ✅ 支持 Artifact 上传/下载

### 1.2 设计原则
- **简单可靠**: 使用 HTTP long polling，避免 WebSocket 复杂性
- **易于扩展**: Runner 可以动态加入/离开
- **容错能力**: Runner 离线后，Job 可以重新分发
- **安全性**: Runner 使用 token 认证，Server 验证 Runner 身份

---

## 二、系统架构

### 2.1 架构图

```
┌─────────────────────────────────────────────────────────────┐
│                      IronForge Server                      │
│  ┌────────────┐  ┌────────────┐  ┌────────────┐       │
│  │  Job        │  │  Runner     │  │  Artifact  │       │
│  │  Dispatcher │  │  Manager    │  │  Manager   │       │
│  └─────┬──────┘  └─────┬──────┘  └─────┬──────┘       │
│        │                  │                  │              │
│  ┌─────▼──────┐  ┌─────▼──────┐  ┌─────▼──────┐       │
│  │  Database   │  │  HTTP API  │  │  File      │       │
│  │  (SeaORM)  │  │  (Axum)    │  │  Storage   │       │
│  └────────────┘  └────────────┘  └────────────┘       │
└─────────────────────────────────────────────────────────────┘
         ▲                    ▲                   ▲
         │    HTTP Long       │                   │
         │    Polling        │                   │
         │                    │                   │
┌────────┴─────────┐  ┌─────┴─────┐  ┌───────┴───────┐
│  Runner Agent 1  │  │  Runner 2  │  │  Runner N    │
│  (rg-runner)     │  │  (rg-runner)│  │  (rg-runner) │
└──────────────────┘  └────────────┘  └───────────────┘
```

### 2.2 通信协议：HTTP Long Polling

**为什么选择 HTTP Long Polling？**
- ✅ 简单易实现（不需要维护 WebSocket 连接）
- ✅ Runner 可以在任何网络环境下工作（不需要开放端口）
- ✅ 与 GitHub Actions Runner 工作方式类似
- ⚠️ 实时性稍差（最长延迟 = long polling timeout）

**工作流程**:
1. Runner 启动后注册到 Server（获取 token）
2. Runner 每 30 秒发送一次心跳（防止被标记为离线）
3. Runner 发送 long polling 请求获取 Job（timeout=30 秒）
4. Server 如果有待分配的 Job，立即返回
5. 如果没有待分配的 Job，Server hold 住请求最多 30 秒
6. Runner 收到 Job 后，执行并上报日志
7. Runner 完成 Job 后，上报结果并继续 long polling

---

## 三、数据库模型设计

### 3.1 runners 表

```sql
CREATE TABLE runners (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,                  -- Runner 名称（用户定义）
    token TEXT NOT NULL UNIQUE,          -- 认证 token（Server 生成）
    status TEXT NOT NULL DEFAULT 'offline',  -- online|offline|busy
    labels TEXT NOT NULL DEFAULT '[]',  -- JSON array（如 ["docker", "linux"]）
    last_seen_at TEXT NOT NULL,         -- 最后心跳时间（ISO 8601）
    version TEXT DEFAULT 'unknown',     -- Runner 版本
    os TEXT DEFAULT 'unknown',          -- 操作系统（linux|windows|darwin）
    arch TEXT DEFAULT 'unknown',        -- 架构（amd64|arm64）
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    INDEX idx_status (status),
    INDEX idx_last_seen (last_seen_at)
);
```

**字段说明**:
- `token`: Runner 注册时由 Server 生成，用于后续认证
- `status`:
  - `online`: Runner 在线，可接收 Job
  - `offline`: Runner 超过 90 秒未发送心跳
  - `busy`: Runner 正在执行 Job
- `labels`: JSON array，用于 Job 选择（如 `runs-on: ["docker"]`）
- `last_seen_at`: 用于离线检测（当前时间 - last_seen_at > 90 秒 → offline）

### 3.2 扩展 pipeline_jobs 表

```sql
ALTER TABLE pipeline_jobs ADD COLUMN runner_id INTEGER REFERENCES runners(id);
ALTER TABLE pipeline_jobs ADD COLUMN started_at TEXT;
ALTER TABLE pipeline_jobs ADD COLUMN finished_at TEXT;
```

---

## 四、API 接口设计

### 4.1 Runner 注册

**POST** `/api/v1/runners/register`

**请求体**:
```json
{
  "name": "my-runner",
  "labels": ["docker", "linux", "amd64"],
  "version": "0.1.0",
  "os": "linux",
  "arch": "amd64"
}
```

**响应** (201 Created):
```json
{
  "id": 1,
  "token": "abc123...",
  "message": "Runner registered successfully"
}
```

**说明**:
- Server 生成唯一 token，Runner 后续请求必须携带此 token
- Token 使用 HTTP Bearer Authentication 发送

---

### 4.2 Runner 心跳

**POST** `/api/v1/runners/:id/heartbeat`  
**Headers**: `Authorization: Bearer <token>`

**请求体**: (empty)

**响应** (200 OK):
```json
{
  "status": "ok",
  "server_time": "2026-05-10T04:20:00Z"
}
```

**说明**:
- Runner 每 30 秒发送一次心跳
- Server 更新 `runners.last_seen_at`
- 如果 token 无效，返回 401 Unauthorized

---

### 4.3 Runner 获取 Job (Long Polling)

**GET** `/api/v1/runners/:id/jobs/poll?timeout=30`  
**Headers**: `Authorization: Bearer <token>`

**响应** (200 OK, 有 Job):
```json
{
  "job_id": 42,
  "pipeline_id": 10,
  "stage_id": 5,
  "script": ["echo 'Hello'", "make build"],
  "image": "rust:1.75",
  "variables": {"RUST_BACKTRACE": "1"},
  "timeout": 3600
}
```

**响应** (204 No Content, 无 Job, timeout):
- Server hold 住请求最多 `timeout` 秒
- 如果有 Job 可用，立即返回
- 如果 timeout 内无 Job，返回 204

**说明**:
- Server 选择状态为 `pending` 的 Job
- 按 `created_at` 顺序分发（FIFO）
- 如果 Job 指定了 `runs-on`，只分发到匹配的 Runner
- Job 状态更新为 `assigned`，`runner_id` 设置为当前 Runner

---

### 4.4 Runner 开始执行 Job

**POST** `/api/v1/runners/:id/jobs/:job_id/start`  
**Headers**: `Authorization: Bearer <token>`

**请求体**: (empty)

**响应** (200 OK):
```json
{
  "status": "ok"
}
```

**说明**:
- Server 更新 Job 状态为 `running`
- 更新 `started_at`
- 更新 Runner 状态为 `busy`

---

### 4.5 Runner 上报日志

**POST** `/api/v1/runners/:id/jobs/:job_id/log`  
**Headers**: `Authorization: Bearer <token>`  
**Content-Type**: `application/octet-stream` (raw log bytes)

**请求体**: (raw log bytes, e.g., "Running script...\nBuild success!\n")

**响应** (200 OK):
```json
{
  "status": "ok"
}
```

**说明**:
- Runner 实时上报日志（可以分段上报）
- Server 将日志追加到 `job_logs` 表或文件
- 前端可以通过 WebSocket 订阅日志更新

---

### 4.6 Runner 完成 Job

**POST** `/api/v1/runners/:id/jobs/:job_id/finish`  
**Headers**: `Authorization: Bearer <token>`

**请求体**:
```json
{
  "status": "success",        // success | failure | error
  "exit_code": 0,
  "message": "Job completed successfully"
}
```

**响应** (200 OK):
```json
{
  "status": "ok"
}
```

**说明**:
- Server 更新 Job 状态为 `success`/`failure`/`error`
- 更新 `finished_at`
- 更新 Runner 状态为 `online`
- 如果 Stage 中所有 Job 完成，更新 Stage 状态
- 如果 Pipeline 中所有 Stage 完成，更新 Pipeline 状态

---

### 4.7 管理员 API：列出所有 Runner

**GET** `/api/v1/admin/runners`  
**Headers**: `Authorization: Bearer <admin_jwt>`

**响应** (200 OK):
```json
{
  "runners": [
    {
      "id": 1,
      "name": "my-runner",
      "status": "online",
      "labels": ["docker", "linux"],
      "last_seen_at": "2026-05-10T04:20:00Z",
      "version": "0.1.0",
      "os": "linux",
      "arch": "amd64"
    }
  ]
}
```

---

## 五、Runner Agent 设计

### 5.1 Runner Agent 功能

**二进制名**: `rg-runner`

**功能**:
1. 注册到 Server（获取 token）
2. 发送心跳（每 30 秒）
3. Long polling 获取 Job
4. 执行 Job（调用 `PipelineRunner`）
5. 上报日志（实时）
6. 上报 Job 完成状态

### 5.2 Runner Agent 配置

**配置文件**: `~/.ironforge/runner.toml`

```toml
[runner]
name = "my-runner"
labels = ["docker", "linux", "amd64"]
server_url = "https://git.example.com"
token = "abc123..."  # 注册后自动保存

[runner.poll]
timeout = 30        # long polling timeout (秒)
heartbeat_interval = 30  # 心跳间隔 (秒)

[runner.job]
timeout = 3600      # Job 执行超时 (秒)
```

### 5.3 Runner Agent 执行流程

```
1. 读取配置文件
2. 如果 token 不存在，注册到 Server（获取 token）
3. 启动心跳线程（每 30 秒发送一次心跳）
4. 进入主循环：
   a. 发送 long polling 请求获取 Job
   b. 如果获取到 Job：
      - 调用 PipelineRunner 执行 Job
      - 实时上报日志
      - 上报 Job 完成状态
   c. 如果 timeout（204 No Content）：
      - 继续 long polling
   d. 如果出错：
      - 上报 Job 失败状态
      - 继续 long polling
```

---

## 六、Job 分发策略

### 6.1 简单 FIFO（第一阶段）

- Server 按 `created_at` 顺序分发 Job
- 不考虑 Runner 负载
- 不考虑 Job 优先级

### 6.2 标签匹配（第二阶段）

- Job 可以指定 `runs-on: ["docker", "linux"]`
- Server 只将 Job 分发给匹配的 Runner
- 如果不指定 `runs-on`，分发给任何在线 Runner

### 6.3 负载均衡（第三阶段）

- Server 优先将 Job 分发给 `online` 且当前没有执行 Job 的 Runner
- 如果所有 Runner 都 busy，将 Job 加入队列等待

---

## 七、离线检测与容错

### 7.1 离线检测

- Server 定时任务（每 30 秒）检查 `runners` 表
- 如果 `last_seen_at` 距离当前时间超过 90 秒，标记 `status = 'offline'`
- 离线 Runner 不会被分发新的 Job

### 7.2 Job 容错

- 如果 Runner 在执行 Job 时离线（心跳中断）：
  - Server 检测到 Runner 离线
  - 将该 Runner 的所有 `running` Job 标记为 `pending`
  - 重新分发给其他 Runner
- 如果 Job 重试次数超过 3 次，标记为 `failed`

---

## 八、Artifact 管理设计

### 8.1 Artifact 上传

**POST** `/api/v1/runners/:id/jobs/:job_id/artifacts`  
**Headers**: `Authorization: Bearer <token>`  
**Content-Type**: `multipart/form-data`

**请求体**:
- `file`: 文件内容
- `name`: Artifact 名称
- `path`: 文件路径（可选）
- `expires_at`: 过期时间（可选，默认 30 天）

**响应** (201 Created):
```json
{
  "artifact_id": 1,
  "download_url": "/api/v1/artifacts/1/download"
}
```

### 8.2 Artifact 存储

- **存储位置**: `~/.ironforge/artifacts/<job_id>/<filename>`
- **数据库记录**: `artifacts` 表（id, job_id, name, path, size, expires_at）
- **清理**: 定时任务删除过期 Artifact

### 8.3 Artifact 下载

**GET** `/api/v1/artifacts/:id/download`  
**Headers**: `Authorization: Bearer <user_jwt>`

**响应**: 文件流（如果未过期）

---

## 九、前端 UI 设计

### 9.1 Pipeline 列表页面

- URL: `/repo/:owner/:repo/ci`
- 显示所有 Pipeline（按时间倒序）
- 显示 Pipeline 状态（pending/running/success/failure）
- 点击进入 Pipeline 详情

### 9.2 Pipeline 详情页面

- URL: `/repo/:owner/:repo/ci/:pipeline_id`
- 显示 Pipeline 信息（commit, ref, trigger）
- 显示 Stages（横向排列）
- 显示每个 Stage 的 Jobs（纵向排列）
- 点击 Job 显示日志

### 9.3 Job 日志查看

- WebSocket 实时推送日志
- 支持滚动查看历史日志
- 支持下载完整日志

### 9.4 Runner 管理页面（管理员）

- URL: `/admin/runners`
- 显示所有 Runner（名称、状态、标签、最后在线时间）
- 允许管理员禁用/删除 Runner

---

## 十、实施计划

### 阶段 1：Runner 调度系统后端（1 周）

- [ ] 创建 `runners` 表（migration）
- [ ] 实现 Runner 注册 API
- [ ] 实现 Runner 心跳 API
- [ ] 实现 Job 分发逻辑（long polling）
- [ ] 实现离线检测定时任务

### 阶段 2：Runner Agent（1-2 周）

- [ ] 创建 `rg-runner` crate（二进制）
- [ ] 实现 Runner 注册
- [ ] 实现心跳发送
- [ ] 实现 Job 接收（long polling）
- [ ] 集成 `PipelineRunner` 执行 Job
- [ ] 实现日志上报

### 阶段 3：Artifact 管理（1 周）

- [ ] 创建 `artifacts` 表（migration）
- [ ] 实现 Artifact 上传 API
- [ ] 实现 Artifact 下载 API
- [ ] 实现过期清理定时任务
- [ ] Runner agent 集成 Artifact 上传

### 阶段 4：前端 UI（1 周）

- [ ] Pipeline 列表页面
- [ ] Pipeline 详情页面
- [ ] Job 日志查看（WebSocket）
- [ ] Runner 管理页面

---

## 十一、技术栈

| 组件 | 技术 |
|------|------|
| **后端框架** | Axum 0.8 |
| **数据库** | SeaORM 1.1 (SQLite/PostgreSQL) |
| **Runner Agent** | Tokio (async runtime) |
| **HTTP Client** | reqwest |
| **日志存储** | 文件系统（`~/.ironforge/artifacts/`） |
| **实时日志** | Axum WebSocket |
| **认证** | Bearer Token (Runner) + JWT (User) |

---

## 十二、风险评估

### 12.1 技术风险

| 风险 | 影响 | 缓解措施 |
|------|------|----------|
| HTTP Long Polling 性能瓶颈 | 中 | 使用连接池，限制并发 long polling 请求数 |
| Runner 离线导致 Job 丢失 | 高 | 实现 Job 容错机制（重新分发） |
| Artifact 存储占用磁盘空间 | 中 | 实现自动过期清理，限制单文件大小 |
| Runner token 泄露 | 高 | 使用 HTTPS，支持 token 吊销 |

### 12.2 时间风险

| 风险 | 影响 | 缓解措施 |
|------|------|----------|
| Runner Agent 实现复杂 | 中 | 第一阶段使用 Shell 执行，后续迁移到 Docker |
| 前端 UI 工作量大 | 中 | 使用 SvelteKit 组件库，参考 Gitea UI |
| 测试工作量被低估 | 高 | 引入自动化测试，模拟 Runner 离线等场景 |

---

## 十三、后续优化方向

1. **Runner 并行执行**: 支持单个 Runner 同时执行多个 Job
2. **Job 优先级**: 支持优先级队列（高优先级 Job 先执行）
3. **Runner 自动缩放**: 根据 Job 队列长度自动启动/关闭 Runner
4. **Artifact 压缩**: 上传时自动压缩，下载时自动解压
5. **分布式 Runner**: 支持跨地域 Runner 调度

---

**审批记录**:
- [ ] 架构师审核
- [ ] 产品经理审批
- [ ] 技术负责人批准

**文档状态**: ✅ 草稿完成，待审核

---

_生成时间: 2026-05-10 12:15 GMT+8_  
_作者: 齐活林 (Qi) · 交付总监_
