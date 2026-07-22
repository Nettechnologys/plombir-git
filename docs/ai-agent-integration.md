# IronForge AI Agent 集成方案

> 目标：让 IronForge 成为各大 AI Agent（Claude Code / Cursor / Copilot / Codex / Trae / CodeBuddy）的**首选文档与知识中枢**
> 
> 现状：已有 142 个 REST API 端点、OpenAPI/Swagger UI、文件浏览、FTS5 搜索、WebSocket 通知

---

## 方案概览

三层架构，渐进式实现：

```
┌─────────────────────────────────────────────────────────────┐
│  第一层：MCP 服务器 (rg-mcp)                                   │
│  ─────── 让支持 MCP 的 AI 工具直接调用 IronForge 能力         │
│  Claude Code / Cursor / Cline / Continue.dev                │
│       │                                                     │
│       ▼ MCP Protocol (stdio)                                │
├─────────────────────────────────────────────────────────────┤
│  第二层：AI 专用 API (rg-http 扩展)                            │
│  ─────── 语义化接口，AI 一次调用获取完整上下文                 │
│  所有 AI Agent（通过 HTTP 调用）                              │
│       │                                                     │
│       ▼ REST API                                            │
├─────────────────────────────────────────────────────────────┤
│  第三层：AI 元数据约定 (.ai/ 目录)                             │
│  ─────── 仓库自描述，告诉 AI 如何理解项目                     │
│  仓库作者定义，AI 自动读取                                    │
└─────────────────────────────────────────────────────────────┘
```

---

## 第一层：MCP 服务器（优先级最高）

### 什么是 MCP？

[MCP (Model Context Protocol)](https://modelcontextprotocol.io) 是 Anthropic 提出的开放协议，让 AI 助手能安全地连接到外部数据源和工具。支持 MCP 的 AI 工具包括：
- Claude Code / Claude Desktop
- Cursor (内置 MCP 支持)
- Cline (VS Code 插件)
- Continue.dev
- 未来：Copilot / CodeBuddy 等

### 当前实现

当前 `crates/rg-mcp/` 是 stdio-only MCP server，通过 IronForge REST API 读取数据。

```
rg-mcp/
├── Cargo.toml
├── src/
│   ├── main.rs         # MCP server 入口 (stdio-only)
│   ├── tools/mod.rs    # list_repos/read_file/read_dir/get_issue/get_pr
│   ├── resources/mod.rs # repo:// file:// issue://
│   └── client.rs       # IronForge REST API 客户端封装
```

### 当前 MCP Tools

| Tool | 功能 | 对应 IronForge API |
|------|------|-------------------|
| `list_repos` | 列出可访问的仓库 | `GET /api/v1/repos` |
| `read_file` | 读取文件内容 | `GET /api/v1/repos/{owner}/{name}/contents/{path}` |
| `read_dir` | 列出目录内容 | `GET /api/v1/repos/{owner}/{name}/tree/{ref}` |
| `get_issue` | 获取 Issue 详情 | `GET /api/v1/repos/{owner}/{name}/issues/{number}` |
| `get_pr` | 获取 PR 详情 | `GET /api/v1/repos/{owner}/{name}/pulls/{number}` |

### 当前 MCP Resources

| Resource URI | 内容 | MIME Type |
|-------------|------|-----------|
| `repo://{owner}/{name}` | 仓库元数据 JSON | `application/json` |
| `file://{owner}/{name}/{path}` | 文件内容 | `text/plain` |
| `issue://{owner}/{name}/{number}` | Issue 完整内容 | `application/json` |

后续可扩展到更多只读工具，例如代码搜索、Issue/PR 列表、commit/wiki/CI 状态和聚合项目上下文；写操作不应默认加入 MCP，只在权限模型和审计策略明确后单独设计。

### 使用方式

**Claude Code 配置** (`~/.claude/settings.json`):
```json
{
  "mcpServers": {
    "ironforge": {
      "command": "ironforge-mcp",
      "env": {
        "IRONFORGE_URL": "https://git.mycompany.com",
        "IRONFORGE_PAT": "pat_xxx"
      }
    }
  }
}
```

**Cursor 配置** (Settings > MCP，使用 stdio 子进程):
```json
{
  "mcpServers": {
    "ironforge": {
      "command": "ironforge-mcp",
      "env": {
        "IRONFORGE_URL": "https://git.mycompany.com",
        "IRONFORGE_PAT": "pat_xxx"
      }
    }
  }
}
```

**使用示例**（用户与 Claude Code 对话）：
```
用户: "看看 ironforge 仓库里 rg-http 的 rate_limit 实现"
Claude: [调用 MCP: read_file("lengyuqu/ironforge", "crates/rg-http/src/rate_limit.rs")]
Claude: "rg-http 的 rate_limit 实现了一个基于 Token Bucket 的限流器..."

用户: "这个仓库最近有什么 Issue 讨论性能问题？"
Claude: [调用 MCP: list_repos()]
Claude: "当前 MCP 只提供仓库列表、文件读取、目录读取、单个 Issue 和单个 PR 读取。Issue 搜索需要走 REST API 或后续 MCP 扩展。"

用户: "给我这个项目的技术栈总结"
Claude: [调用 MCP: read_file("lengyuqu/ironforge", "Cargo.toml")]
Claude: "IronForge 是一个 Rust 实现的 Git 托管平台，workspace 依赖包括 Axum、SeaORM、gix 等..."
```

---

## 第二层：AI 专用 REST API（后续设想）

当前代码库尚未实现 `/api/v1/ai/*` 前缀端点。以下内容是后续可在 `rg-http` 中新增的 AI 专用 REST API 设计，用于在现有只读 MCP 能力之外提供更聚合的上下文读取接口。

### 设想端点

#### 1. `GET /api/v1/ai/repos/{owner}/{name}/context`

**一次调用，返回 AI 需要的完整项目上下文**：

```json
{
  "repo": {
    "name": "ironforge",
    "description": "Rust Git hosting platform",
    "language": "Rust",
    "default_branch": "main"
  },
  "readme": {
    "content": "# IronForge...",
    "truncated": false
  },
  "structure": {
    "crates": ["rg-cli", "rg-core", "rg-http", "rg-git", "rg-ssh", "rg-db", "rg-ci", "rg-runner"],
    "web_framework": "SvelteKit 5",
    "database": "SeaORM + SQLite",
    "key_files": [
      {"path": "Cargo.toml", "type": "manifest", "description": "Workspace root"},
      {"path": "AGENT.md", "type": "ai_context", "description": "AI quick entry guide"},
      {"path": "CLAUDE.md", "type": "ai_context", "description": "Deep AI collaboration guide"},
      {"path": "ironforge-docs/architecture/project-architecture-2026-07.md", "type": "docs", "description": "Current architecture overview"},
      {"path": "ironforge-docs/architecture/frontend-backend-structure-2026-07.md", "type": "docs", "description": "Current frontend/backend mapping"}
    ]
  },
  "recent_activity": {
    "open_issues": 12,
    "open_prs": 3,
    "last_commit": {
      "sha": "4c2afb0",
      "message": "Phase 20: engineering improvements",
      "author": "lengyuqu",
      "date": "2026-05-11T10:00:00Z"
    }
  },
  "dependencies": {
    "axum": "0.8",
    "sea-orm": "1.1",
    "gix": "0.84",
    "russh": "0.51"
  },
  "ai_metadata": {
    "agent_instructions": "Read AGENT.md, CLAUDE.md, and ironforge-docs/architecture/project-architecture-2026-07.md before architectural modifications",
    "test_command": "cargo test --all",
    "build_command": "cargo build --release"
  }
}
```

**为什么需要这个端点？**
- AI Agent 通常需要"先了解项目全貌再动手"
- 现有 API 需要 5-10 次调用才能获取相同信息
- 减少 token 消耗和延迟

#### 2. `POST /api/v1/ai/search`

**语义化搜索**（基于现有 FTS5 扩展）：

```json
// Request
{
  "query": "how does authentication work",
  "scope": "code",        // code | issues | prs | wiki | all
  "repo": "lengyuqu/ironforge",
  "limit": 10
}

// Response
{
  "results": [
    {
      "type": "file",
      "path": "crates/rg-core/src/auth/mod.rs",
      "score": 0.95,
      "snippet": "pub async fn authenticate_token(...)",
      "line_start": 42,
      "line_end": 58
    },
    {
      "type": "issue",
      "number": 15,
      "title": "Add OAuth2 support",
      "score": 0.72
    }
  ]
}
```

#### 3. `GET /api/v1/ai/repos/{owner}/{name}/changes`

**增量变更摘要**（AI 需要知道"上次之后发生了什么"）：

```json
// Request: GET /api/v1/ai/repos/lengyuqu/ironforge/changes?since=2026-05-01T00:00:00Z
{
  "commits": 15,
  "files_changed": 23,
  "new_issues": 4,
  "closed_issues": 2,
  "merged_prs": 3,
  "summary": "Phase 18-20 completed: gix migration (~60%), CI/CD Runner, P2 features",
  "key_changes": [
    {
      "type": "feature",
      "description": "Added external Runner Agent binary",
      "files": ["crates/rg-runner/src/main.rs"]
    },
    {
      "type": "refactor",
      "description": "Migrated CI config reading from git CLI to gix",
      "files": ["crates/rg-ci/src/lib.rs"]
    }
  ]
}
```

#### 4. `GET /api/v1/ai/repos/{owner}/{name}/files/tree?format=ai`

**AI 友好的文件树**（带类型标注和重要性评分）：

```json
{
  "files": [
    {"path": "Cargo.toml", "type": "manifest", "importance": 1.0},
    {"path": "AGENT.md", "type": "ai_context", "importance": 0.98},
    {"path": "CLAUDE.md", "type": "ai_context", "importance": 0.95},
    {"path": "ironforge-docs/architecture/project-architecture-2026-07.md", "type": "docs", "importance": 0.94},
    {"path": "ironforge-docs/architecture/frontend-backend-structure-2026-07.md", "type": "docs", "importance": 0.92},
    {"path": "crates/rg-http/src/lib.rs", "type": "source", "importance": 0.85, "language": "rust"},
    {"path": "web/src/lib/api/client.ts", "type": "source", "importance": 0.7, "language": "typescript"}
  ],
  "directories": [
    {"path": "crates/rg-core/src", "file_count": 42, "description": "Core business logic"}
  ]
}
```

---

## 第三层：AI 元数据约定（.ai/ 目录）

让仓库作者能**主动告诉 AI 如何理解项目**。

### 文件结构

```
repo/
├── .ai/
│   ├── agent.md              # AI Agent 通用上下文（类似现有 CLAUDE.md）
│   ├── instructions.md       # 项目特定操作指令
│   ├── ignore.md             # AI 应该忽略的文件/目录
│   ├── prompts/              # 预定义提示词模板
│   │   ├── review.md         # 代码审查提示词
│   │   ├── summarize.md      # 提交摘要提示词
│   │   └── onboard.md        # 新贡献者引导
│   └── context/              # 附加上下文文件
│       ├── architecture.md   # 架构决策记录
│       └── api-guide.md      # API 使用指南
├── CLAUDE.md                 # 现有：Claude Code 专用
├── ARCHITECTURE.md           # 现有：历史架构设计
├── ironforge-docs/           # 现有：分析报告（索引见 README.md）
│   ├── README.md
│   └── architecture/         # 架构总览 + 前后端结构 + 后续待办
└── README.md                 # 现有：项目说明
```

### 文件格式规范

**`.ai/agent.md`**:
```markdown
# AI Agent 指南

## 项目概述
IronForge 是一个 Rust 实现的 Git 托管平台，对标 Gitea。

## 技术栈
- 后端：Rust (Axum, SeaORM, gix, russh)
- 前端：SvelteKit 5 (TypeScript)
- 数据库：SQLite；PostgreSQL 是后续生产化方向

## 开发规范
1. 修改代码前，先阅读 `AGENT.md`、`CLAUDE.md` 和 `ironforge-docs/architecture/project-architecture-2026-07.md`
2. 新增模块需要在 `rg-core/src/lib.rs` 中导出
3. API 变更需同步更新 OpenAPI 注解和前端 client.ts
4. 测试命令：`cargo test --all`
5. 构建命令：`cargo build --release`

## 文件导航
- 新增 HTTP API → `crates/rg-http/src/api/`
- 新增业务逻辑 → `crates/rg-core/src/`
- 新增数据库实体 → `crates/rg-db/src/entities/`
- 前端页面 → `web/src/routes/`
- 前端组件 → `web/src/lib/components/`
```

**`.ai/ignore.md`**:
```markdown
# AI 忽略列表

## 不重要的文件
- `target/` - 构建产物
- `web/node_modules/` - 前端依赖
- `*.lock` - 锁文件（除非分析依赖变更）
- `migrations/*.rs` - 自动生成的迁移文件

## 不需要修改的文件
- `Cargo.toml` workspace 定义（除非新增 crate）
- `web/svelte.config.js` - 前端构建配置
```

### IronForge 自动读取逻辑

当 AI Agent 通过 MCP 或 API 访问仓库时，IronForge 自动：
1. 检查 `.ai/agent.md` → 作为系统提示词的一部分
2. 检查 `.ai/ignore.md` → 过滤搜索结果和文件列表
3. 检查 `.ai/instructions.md` → 附加到上下文
4. 检查 `.ai/prompts/*.md` → 作为可用提示词模板

### 与现有文件的兼容

| 现有文件 | AI 用途 | 建议 |
|---------|--------|------|
| `CLAUDE.md` | Claude Code 专用入口 | 保留，同时在 `.ai/agent.md` 中引用 |
| `AGENT.md` | 通用 AI 轻量入口 | 保留，作为 `.ai/agent.md` 的简化版 |
| `ironforge-docs/architecture/project-architecture-2026-07.md` | 当前架构事实 | 优先引用，作为 `.ai/context/architecture.md` 的内容来源 |
| `ARCHITECTURE.md` | 历史架构设计 | 保留作为设计背景，不作为当前事实唯一来源 |
| `CONTRIBUTING.md` | 贡献指南 | 保留，`.ai/agent.md` 中引用 |

---

## 实施路线图

### Phase A：MCP 服务器（1-2 周）

**目标：让 Claude Code / Cursor 能直接调用 IronForge**

1. **已完成**：
   - 创建 `crates/rg-mcp/` crate
   - 实现 MCP stdio 传输层
   - 实现 5 个核心 Tool：`list_repos`, `read_file`, `read_dir`, `get_issue`, `get_pr`
   - 实现 3 个核心 Resource：`repo://`, `file://`, `issue://`
   - 编写 MCP 客户端配置文档，并统一 stdio-only 口径

2. **后续增强**：
   - 按实际需求扩展更多只读 Tool，例如搜索、Issue/PR 列表、commit/wiki/CI 状态
   - SSE 传输模式单独排期；当前不要作为可用 transport 发布
   - 继续使用 `IRONFORGE_PAT` Bearer Token；如新增写操作，需先明确权限模型和审计策略
   - 测试与 Claude Code / Cursor 的集成
   - 发布安装指南

**技术选型**：
- Rust MCP SDK：`rmcp` (https://github.com/modelcontextprotocol/rust-sdk)
- 传输：stdio；SSE 未实现
- 认证：IronForge PAT Bearer Token

### Phase B：AI 专用 API（1 周）

**目标：减少 AI Agent 的 API 调用次数**

1. 在 `rg-http` 中新增 `/api/v1/ai/` 路由模块
2. 实现 `GET /api/v1/ai/repos/{owner}/{name}/context`
3. 实现 `POST /api/v1/ai/search`
4. 实现 `GET /api/v1/ai/repos/{owner}/{name}/changes`
5. 实现 `GET /api/v1/ai/repos/{owner}/{name}/files/tree?format=ai`
6. 更新 OpenAPI 文档（utoipa 注解）

### Phase C：AI 元数据约定（0.5 周）

**目标：让仓库能自描述**

1. 在文件浏览 API 中自动检测并返回 `.ai/` 目录内容
2. 在设想的 `get_project_context` 聚合端点中自动注入 `.ai/agent.md`
3. 在搜索 API 中自动过滤 `.ai/ignore.md` 中列出的文件
4. 为 IronForge 自身仓库创建 `.ai/` 示例
5. 编写 `.ai/` 规范文档

### Phase D：生态扩展（持续）

1. **VS Code 扩展**：IronForge 侧边栏，显示 AI 上下文面板
2. **CodeBuddy 插件**：让 CodeBuddy 原生支持 IronForge MCP
3. **GitHub Copilot 聊天插件**：`/ironforge` 命令
4. **Web UI AI 助手**：在前端中内置 AI 聊天面板（调用 MCP）

---

## 技术细节

### MCP 协议通信示例

**AI → MCP Server (Tool Call)**:
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "tools/call",
  "params": {
    "name": "read_file",
    "arguments": {
      "owner": "lengyuqu",
      "repo": "ironforge",
      "path": "crates/rg-http/src/rate_limit.rs",
      "ref": "main"
    }
  }
}
```

**MCP Server → IronForge API**:
```bash
curl -H "Authorization: Bearer pat_xxx" \
  https://git.example.com/api/v1/repos/lengyuqu/ironforge/contents/crates/rg-http/src/rate_limit.rs?ref=main
```

**MCP Server → AI (Tool Result)**:
```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "result": {
    "content": [
      {
        "type": "text",
        "text": "use std::...\npub struct RateLimiter {...}"
      }
    ],
    "isError": false
  }
}
```

### 认证流程

```
AI Agent (Claude Code)
    │
    │ 1. 用户配置 MCP Server 时提供 PAT
    │
    ▼
MCP Server (rg-mcp)
    │
    │ 2. 每个 Tool Call 携带 Bearer Token
    │
    ▼
IronForge HTTP API
    │
    │ 3. authenticate_bearer() 验证 PAT
    │ 4. 查询 user_id + scopes
    │ 5. 检查 repo 的 can_read/can_write
    │
    ▼
业务逻辑执行
```

### 性能考虑

| 场景 | 优化策略 |
|------|----------|
| AI 频繁读取大文件 | MCP Server 本地缓存（TTL 60s）+ 文件内容哈希比对 |
| `get_project_context` 聚合慢 | 服务端预计算 + Redis 缓存（5分钟 TTL） |
| 大量仓库列表 | 分页 + 增量同步（ETag / If-None-Match） |
| WebSocket 实时通知 | MCP Server 订阅 WebSocket，主动推送变更给 AI |

---

## 竞争优势分析

| 特性 | IronForge + AI | GitHub | GitLab | Gitea |
|------|---------------|--------|--------|-------|
| MCP 原生支持 | ✅ 内置 | ❌ 需第三方 | ❌ 需第三方 | ❌ 需第三方 |
| AI 专用 API | ✅ 专用 `/ai/*` 端点 | ❌ GraphQL 通用 | ❌ REST 通用 | ❌ REST 通用 |
| 仓库自描述 | ✅ `.ai/` 目录约定 | ❌ 无 | ❌ 无 | ❌ 无 |
| 私有化部署 | ✅ 单二进制 <50MB | ❌ 企业版 | ⚠️ 复杂 | ✅ 但无 AI 功能 |
| 代码搜索 | ✅ FTS5 + 语义 | ✅ 高级搜索 | ✅ 高级搜索 | ❌ 基础 |
| 实时通知 | ✅ WebSocket | ✅ | ✅ | ❌ |

---

## 下一步行动

如果你认可这个方案，建议按以下顺序推进：

1. **保持当前 stdio MCP 可用**：继续维护 5 个核心只读 Tool 和 3 个 Resource。
2. **补真实需求优先的只读 Tool**：优先考虑代码搜索、Issue/PR 列表、commit/wiki/CI 状态。
3. **SSE 单独设计**：在 HTTP server、认证、连接生命周期和部署方式明确前，不把 SSE 写成可用 transport。
4. **AI 专用 API 后续评估**：`GET /api/v1/ai/context` 等聚合端点需结合前端/Agent 真实使用场景再实现。
5. **持续**：完善 `.ai/` 生态，保持 `AGENT.md`、`CLAUDE.md` 和 2026-07 架构文档为当前事实入口。

这个方案的核心价值是：**IronForge 不只是一个 Git 托管平台，而是一个"AI Ready"的知识基础设施** —— 每个仓库都能被 AI 理解、查询、分析，成为团队知识的活文档。
