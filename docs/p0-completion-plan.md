# IronForge P0 完善方案 — 剩余缺口与实施计划

> 版本：v1.0 | 日期：2026-05-22 | 编制：WorkBuddy
> 基于 Phase 1~20 全部完成后的项目现状，识别 P0 级别的剩余缺口，制定逐项完善方案。

---

## 一、P0 剩余缺口总览

| # | 缺口 | 当前状态 | 复杂度 | 预估工时 |
|---|------|----------|--------|---------|
| P0-1 | `/search/code` 代码搜索端点 | ❌ 完全未实现 | **高** | 3-5 天 |
| P0-2 | Runner 内部端点 OpenAPI 注解 | ⚠️ 5/8 缺 utoipa | **低** | 0.5 天 |
| P0-3 | SSH Protocol V2 完整实现 | ⚠️ 仅发送 capabilities，命令处理为空壳 | **中** | 2-3 天 |

---

## 二、P0-1：`/search/code` 代码搜索端点

### 2.1 需求定义

| 维度 | 说明 |
|------|------|
| **目标** | 支持在仓库源代码中搜索关键词，返回匹配的文件路径、行号和上下文片段 |
| **参考** | GitHub `/search/code`、Gitea 代码搜索 |
| **范围** | P0 仅支持精确关键词搜索，不支持正则/语义搜索 |
| **排除** | 代码高亮（P2）、中文分词（P2）、搜索建议（P3） |

### 2.2 技术方案

#### 方案选择：gix + 内存索引（推荐）

| 方案 | 优点 | 缺点 | 评估 |
|------|------|------|------|
| **A. gix 遍历 + grep** | 无额外依赖，实时搜索 | 首次搜索慢，大仓库耗内存 | ✅ 推荐 P0 |
| B. SQLite FTS5 索引 | 复用现有基础设施 | 需要索引管道，代码量大 | ⚠️ 可选 P1 |
| C. 外部搜索引擎 (Meilisearch) | 性能最好 | 违反单二进制部署原则 | ❌ 不考虑 |

**推荐方案 A**：在搜索请求到达时，用 gix 打开仓库，遍历 blob 对象，对文本文件做关键词匹配。

#### 核心实现逻辑

```
GET /api/v1/search/code?q=keyword&repo=owner/name&page=1&per_page=20

1. 解析 q（关键词）和 repo（限定仓库，必填）
2. gix::open(repo_path) 打开 bare repo
3. 获取 HEAD commit 的 tree
4. 递归遍历 tree → blob
5. 对每个 blob 判断是否文本文件（检测 NUL 字节，跳过二进制）
6. 对文本 blob 做 line-by-line 关键词匹配
7. 收集结果：(file_path, line_number, line_content)
8. 分页返回
```

#### 数据结构

```rust
/// 代码搜索结果
#[derive(Debug, Serialize)]
pub struct CodeSearchResult {
    pub file_path: String,
    pub line_number: u64,
    pub line_content: String,
    pub repo_owner: String,
    pub repo_name: String,
}

/// 代码搜索响应
#[derive(Debug, Serialize)]
pub struct CodeSearchResponse {
    pub total: i64,
    pub page: u64,
    pub per_page: u64,
    pub results: Vec<CodeSearchResult>,
}
```

### 2.3 涉及文件

| 操作 | 文件路径 | 说明 |
|------|---------|------|
| **修改** | `crates/rg-core/src/search/service.rs` | 新增 `search_code()` 函数 |
| **修改** | `crates/rg-http/src/api/search.rs` | 新增 `search_code` handler + 路由 |
| **修改** | `crates/rg-http/src/lib.rs` | 注册 `/search/code` 路由 |
| **修改** | `web/src/lib/api/client.ts` | 新增 `search.code()` 方法 |
| **修改** | `web/src/routes/search/+page.svelte` | 新增 Code tab |

### 2.4 gix API 用法

```rust
use gix::Repository;

fn search_code_in_repo(
    repo_path: &Path,
    keyword: &str,
    limit: usize,
) -> Result<Vec<CodeSearchResult>> {
    let repo = Repository::open(repo_path)?;
    let head = repo.head_commit()?;
    let tree = head.tree()?;

    let mut results = Vec::new();

    tree.traverse().breadthfirst().for_each(|entry| {
        if let Entry::Blob(blob) = entry {
            // 跳过二进制文件
            let data = blob.contents();
            if is_binary(data) { return; }

            let text = String::from_utf8_lossy(data);
            for (i, line) in text.lines().enumerate() {
                if line.contains(keyword) {
                    results.push(CodeSearchResult {
                        file_path: entry.path().to_string_lossy().to_string(),
                        line_number: i as u64 + 1,
                        line_content: truncate(line, 200),
                        ..
                    });
                    if results.len() >= limit { return; }
                }
            }
        }
    });

    Ok(results)
}

/// 检测二进制文件：前 8KB 中包含 NUL 字节则判定为二进制
fn is_binary(data: &[u8]) -> bool {
    data[..data.len().min(8192)].iter().any(|&b| b == 0)
}
```

### 2.5 性能约束

| 场景 | 约束 | 方案 |
|------|------|------|
| 大仓库 (>10000 blobs) | 遍历耗时 >5s | 设置搜索超时（3s），超时返回部分结果 + `truncated: true` |
| 二进制文件 | 浪费 I/O | 前 8KB 检测 NUL，跳过二进制 |
| 搜索结果过多 | 返回数据量大 | `per_page` 上限 50，`total` 上限 1000 |
| 并发搜索 | CPU 密集 | 用 `tokio::task::spawn_blocking` 隔离 |

### 2.6 API 设计

| Method | Path | 说明 | 鉴权 | Request | Response |
|--------|------|------|------|---------|----------|
| GET | `/search/code` | 代码搜索 | Optional | `?q=keyword&repo=owner/name&page=1&per_page=20` | `200 CodeSearchResponse` |

**Query 参数：**

| 参数 | 类型 | 必填 | 默认值 | 说明 |
|------|------|------|--------|------|
| q | string | ✅ | — | 搜索关键词 |
| repo | string | ✅ | — | 限定仓库（owner/name） |
| page | number | ❌ | 1 | 页码 |
| per_page | number | ❌ | 20 | 每页条数（上限 50） |

**Response 示例：**

```json
{
  "total": 15,
  "page": 1,
  "per_page": 20,
  "truncated": false,
  "results": [
    {
      "file_path": "src/auth/service.rs",
      "line_number": 42,
      "line_content": "fn verify_token(token: &str) -> Result<Claims> {",
      "repo_owner": "alice",
      "repo_name": "ironforge"
    }
  ]
}
```

### 2.7 前端变更

搜索页面新增 "Code" tab：

```
[All] [Code] [Issues] [Repos] [Wiki]
        ↑ 新增
```

- 选择 Code tab 后，`repo` 参数变为必填，显示仓库选择器
- 搜索结果以文件路径 + 行号列表展示
- 点击文件跳转到 `/[owner]/[repo]/blob/[...path]`

---

## 三、P0-2：Runner 内部端点 OpenAPI 注解

### 3.1 当前状态

Runner 共 8 个端点，其中 3 个已有 `#[utoipa::path]` 注解，5 个缺失：

| 端点 | 函数 | 行号 | utoipa 注解 | 状态 |
|------|------|------|-------------|------|
| `POST /runners/register` | `register` | L82 | ✅ 有 | 完成 |
| `POST /runners/:id/heartbeat` | `heartbeat` | L115 | ❌ 缺 | **待补** |
| `GET /runners/:id/jobs/poll` | `poll_job` | L133 | ❌ 缺 | **待补** |
| `POST /runners/:id/jobs/:job_id/start` | `start_job` | L200 | ❌ 缺 | **待补** |
| `POST /runners/:id/jobs/:job_id/log` | `upload_log` | L234 | ❌ 缺 | **待补** |
| `POST /runners/:id/jobs/:job_id/finish` | `finish_job` | L276 | ❌ 缺 | **待补** |
| `GET /admin/runners` | `list_runners_admin` | L335 | ✅ 有 | 完成 |
| `DELETE /admin/runners/:id` | `delete_runner_admin` | L424 | ✅ 有 | 完成 |

### 3.2 实施方案

为 5 个缺失端点添加 `#[utoipa::path]` 注解，格式与已有注解保持一致。

#### 注解模板

```rust
/// POST /api/v1/runners/:id/heartbeat
#[utoipa::path(
    post,
    path = "/runners/{id}/heartbeat",
    tag = "Runners",
    params(
        ("id" = i64, Path, description = "Runner ID")
    ),
    responses(
        (status = 200, description = "Heartbeat acknowledged", body = serde_json::Value),
        (status = 401, description = "Unauthorized - invalid runner token", body = serde_json::Value),
        (status = 404, description = "Runner not found", body = serde_json::Value),
    ),
)]
pub async fn heartbeat(...)
```

每个端点的注解参数：

| 端点 | method | path | params | request_body |
|------|--------|------|--------|-------------|
| heartbeat | post | `/runners/{id}/heartbeat` | `id: i64` | — |
| poll_job | get | `/runners/{id}/jobs/poll` | `id: i64` + `timeout: Option<u64>` | — |
| start_job | post | `/runners/{id}/jobs/{job_id}/start` | `id: i64, job_id: i64` | — |
| upload_log | post | `/runners/{id}/jobs/{job_id}/log` | `id: i64, job_id: i64` | `content = String` |
| finish_job | post | `/runners/{id}/jobs/{job_id}/finish` | `id: i64, job_id: i64` | `content = serde_json::Value` |

### 3.3 涉及文件

| 操作 | 文件路径 | 说明 |
|------|---------|------|
| **修改** | `crates/rg-http/src/api/runners.rs` | 为 5 个端点添加 `#[utoipa::path]` |

### 3.4 验证标准

1. `cargo build` 通过
2. Swagger UI (`/api-docs/`) 中 Runners tag 下显示 8 个端点
3. OpenAPI spec 导出包含所有 Runner 路由

---

## 四、P0-3：SSH Protocol V2 完整实现

### 4.1 当前状态

| 组件 | 实现状态 | 说明 |
|------|----------|------|
| Capability Advertisement | ✅ 完成 | 发送 version 2 + ls-refs/fetch/shallow 等能力 |
| ls-refs 命令 | ✅ 完成（HTTP 模式） | HTTP V2 模式下 ls-refs 正常工作 |
| fetch 命令 | ✅ 完成（HTTP 模式） | HTTP V2 模式下 fetch 正常工作 |
| object-info 命令 | ✅ 完成（HTTP 模式） | HTTP V2 模式下 object-info 可用 |
| **SSH V2 命令处理** | ❌ 空壳 | `handle_v2_stream_impl` 仅发送 capabilities，然后直接返回 |

**问题**：SSH V2 的 `handle_v2_stream_impl` 函数（L68-77）只发送 capability advertisement 就结束了，没有进入命令处理循环。这意味着 SSH 客户端请求 V2 时，只能看到服务器支持什么能力，但无法执行任何命令。

### 4.2 技术挑战

SSH 与 HTTP 的关键区别：

| 维度 | HTTP V2 | SSH V2 |
|------|---------|--------|
| I/O 模型 | 分离的 reader/writer | 单一双向 stream |
| 命令处理 | 一个请求-响应周期 | 持久连接，多轮命令 |
| 状态管理 | 无状态 | 有状态（需要维护 session） |
| 实现 | ✅ 已有 `handle_v2_impl` | ❌ 需要改造 |

**核心难点**：SSH stream 是全双工的，需要在一个 stream 上同时读写。当前 `handle_v2_impl` 使用分离的 reader/writer，不能直接用于 SSH 模式。

### 4.3 实施方案

#### 方案：拆分 SSH stream 为读写两半

```rust
pub async fn handle_v2_stream_impl<S>(repo_path: &Path, stream: &mut S) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // 发送 capability advertisement
    send_capability_advertisement(stream).await?;

    // 将 stream 拆分为 reader 和 writer 两半
    // 使用 tokio::io::split 或手动管理读写
    let (read_half, write_half) = tokio::io::split(stream);

    let mut reader = BufReader::new(read_half);
    let mut writer = write_half;

    // 命令处理循环（复用 HTTP V2 的逻辑）
    loop {
        match read_command_request(&mut reader).await? {
            CommandRequest::LsRefs { .. } => {
                handle_ls_refs(repo_path, &mut writer, ...).await?;
            }
            CommandRequest::Fetch { .. } => {
                handle_fetch(repo_path, &mut writer, ...).await?;
            }
            CommandRequest::ObjectInfo { .. } => {
                handle_object_info(repo_path, &mut writer, ...).await?;
            }
            CommandRequest::Flush => break,
            CommandRequest::Unknown(cmd) => {
                tracing::warn!("Unknown V2 command: {}", cmd);
                skip_until_flush(&mut reader).await?;
                write_flush(&mut writer).await?;
            }
        }
    }

    Ok(())
}
```

**问题**：`tokio::io::split` 需要 stream 的所有权，但 `handle_v2_stream_impl` 收到的是 `&mut S`。需要调整函数签名或使用 `DuplexStream` 桥接。

#### 推荐方案：调整函数签名

```rust
// 改为接收 stream 的所有权（由调用方 clone/move）
pub async fn handle_v2_stream<S>(repo_path: &Path, stream: S) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    send_capability_advertisement(/* 需要先写一半 */).await?;

    let (read_half, write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut writer = write_half;

    // ... 命令处理循环
}
```

或者更简单的方案——**直接用 `stream` 交替读写**（不需要 split）：

```rust
pub async fn handle_v2_stream_impl<S>(repo_path: &Path, stream: &mut S) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    send_capability_advertisement(stream).await?;

    // 交替读写：读一个命令，写一个响应
    loop {
        let pkt = read_pkt_line(stream).await?;
        // 解析命令...
        // 处理命令...
        // 写响应到同一个 stream...
        write_pkt_line(stream, ...).await?;
    }
}
```

这个方案在 V1 的 `upload_pack`/`receive_pack` 中已有先例，风险最低。

### 4.4 涉及文件

| 操作 | 文件路径 | 说明 |
|------|---------|------|
| **修改** | `crates/rg-git/src/protocol/v2.rs` | 重写 `handle_v2_stream_impl`，添加 SSH 命令处理循环 |
| **修改** | `crates/rg-ssh/src/lib.rs` | 确认 SSH 入口正确调用 V2 handler |

### 4.5 验证标准

```bash
# 1. SSH V2 clone
GIT_SSH_COMMAND="ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null" \
  git -c protocol.version=2 clone ssh://git@localhost:2222/testuser/testrepo /tmp/v2_test

# 2. SSH V2 fetch
cd /tmp/v2_test
git -c protocol.version=2 fetch

# 3. SSH V2 ls-remote
git -c protocol.version=2 ls-remote ssh://git@localhost:2222/testuser/testrepo
```

---

## 五、实施路线图

### Phase 1：Runner OpenAPI 注解（0.5 天）

最简单、风险最低的任务，可以立即动手。

```
T1: 为 5 个 Runner 端点添加 utoipa::path 注解
    → 文件: crates/rg-http/src/api/runners.rs
    → 验证: cargo build + Swagger UI
```

### Phase 2：SSH Protocol V2（2-3 天）

中等复杂度，需要深入理解 SSH 流处理。

```
T2: 重写 handle_v2_stream_impl
    → 文件: crates/rg-git/src/protocol/v2.rs
    → 复用已有 handle_ls_refs/handle_fetch/handle_object_info
    → 验证: git -c protocol.version=2 clone/fetch/ls-remote
```

### Phase 3：代码搜索（3-5 天）

最高复杂度，需要 gix tree 遍历 + 前端联动。

```
T3a: 后端 - search_code() 函数
    → 文件: crates/rg-core/src/search/service.rs
    → gix tree 遍历 + 关键词匹配 + 分页

T3b: API - search_code handler
    → 文件: crates/rg-http/src/api/search.rs
    → 新增 GET /search/code 路由

T3c: 前端 - Code tab
    → 文件: web/src/routes/search/+page.svelte
    → 新增 Code 搜索标签页
```

### 时间线

```
Week 1:  Phase 1 (0.5d) + Phase 2 (2-3d)
Week 2:  Phase 3 (3-5d)
────────────────────────
Total:   6-8.5 天
```

---

## 六、风险与缓解

| 风险 | 概率 | 影响 | 缓解 |
|------|------|------|------|
| gix tree 遍历 API 不稳定 | 中 | 高 | 先写 PoC 验证 API，保留 git CLI fallback |
| SSH V2 流拆分导致数据丢失 | 低 | 高 | 使用交替读写方案，避免 split |
| 代码搜索性能不达标 | 中 | 中 | 设置超时 + 结果截断 + spawn_blocking |
| utoipa 注解格式与现有代码不一致 | 低 | 低 | 参照 register/list_runners_admin 的格式 |

---

## 七、完成标准

| 项目 | 验收标准 |
|------|---------|
| P0-1 代码搜索 | `GET /search/code?q=fn+main&repo=owner/name` 返回正确结果；Swagger UI 可见；前端 Code tab 可用 |
| P0-2 Runner OpenAPI | Swagger UI Runners tag 显示 8 个端点；`cargo build` 无 warning |
| P0-3 SSH V2 | `git -c protocol.version=2 clone` / `fetch` / `ls-remote` 通过 SSH 正常工作 |

---

_文档版本：v1.0 | 如有疑问请参考 `CLAUDE.md` 和 `ARCHITECTURE.md`_
