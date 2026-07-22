# IronForge P0 功能 PRD

> 版本：v1.0 | 日期：2026-05-08 | 负责人：许清楚（产品经理）

---

## 1. 项目信息

- **Language**: 中文
- **Programming Language**: Rust (Axum/SeaORM) + Svelte 5
- **Project Name**: ironforge
- **项目类型**: 增量功能开发（基于已有 Phase 1~12 的成熟代码库）
- **原始需求复述**: IronForge 是对标 Gitea/Forgejo 的自托管 Git 托管平台，通过与 Gitea 的 Gap Analysis 识别出 10 个 P0 功能缺失，需要分 5 个 Phase 实施补齐，使平台功能完整度达到生产可用标准。

---

## 2. 产品定义

### Product Goals

**PG-1: 补全协作核心能力**
使 IronForge 支持标准的 Git 协作工作流，包括 Fork 派生、仓库 Star/Watch、仓库转移等基础协作操作，缩小与 Gitea 在协作功能上的差距。

**PG-2: 提升平台可发现性与可管理性**
通过全局全文搜索（FTS5）和规范化 Labels/Milestones API，增强内容的可发现性和项目管理能力，让团队能高效管理大规模仓库和 Issue。

**PG-3: 完善平台基础设施**
提供 API Token（PAT）认证支持、Commit Status Checks 集成和仓库删除功能，建立完整的 DevOps 基础设施，为 CI/CD 集成和第三方工具对接铺路。

### User Stories

**US-1: Fork 仓库**
> 作为一个开源贡献者，我想要 Fork 一个公开仓库到我的账号下，这样我就可以在独立的环境中实验和开发，而不影响上游仓库。

**US-2: 发现感兴趣的项目**
> 作为一个开发者，我想要 Star 我关注的仓库并接收 Watch 通知，这样我可以快速在个人面板中找到重要项目并跟踪其更新。

**US-3: 快速定位代码和 Issue**
> 作为一个仓库维护者，我想要通过全局搜索找到特定代码片段、Issue 或 Wiki 内容，这样我可以快速定位和解决问题，而不需要逐个仓库浏览。

**US-4: 规范化项目标签管理**
> 作为一个项目经理，我想要创建和分配标准化的 Labels（bug/enhancement/priority 等），这样团队可以有统一的 Issue 分类和筛选标准。

**US-5: 通过 API 自动化操作**
> 作为一个 DevOps 工程师，我想要通过 Personal Access Token（PAT）调用 IronForge REST API 自动化脚本，这样我可以实现自动化部署、CI/CD 集成和运维操作。

---

## 3. 技术规范

### Requirements Pool

#### P0 — Must Have

| # | 功能 | 描述 | 涉及模块 | 依赖 |
|---|------|------|----------|------|
| R-01 | Fork 仓库 | 用户将他人仓库派生到自己的账号下，复制 Git 数据和元数据，fork_id 双向关联 | `rg-core/src/repo/`, `rg-http/src/api/repos.rs` | 无 |
| R-02 | Star/Watch 仓库 | Star 计数更新、Watch 列表管理（已有 stars_count/forks_count 字段） | `rg-core/src/repo/`, `rg-http/src/api/repos.rs` | 无 |
| R-03 | Releases/Tags 管理 | 创建/编辑/删除 Releases，关联 Tag；Tag 列表 API | `rg-http/src/api/repos.rs`, `rg-http/src/api/repo_content.rs` | 无 |
| R-04 | Labels CRUD（独立表） | 创建 labels 独立表，替代 issues.labels JSON 列，Labels API | `rg-db/src/entities/`, `rg-core/src/issue/`, `rg-http/src/api/issues.rs` | R-06 |
| R-05 | Milestones API | 为已有 milestones 实体补充 REST API（list/create/update/delete） | `rg-http/src/api/issues.rs` | 已有 entity |
| R-06 | API Tokens / PAT | 为已有 access_tokens 实体补充 REST API，支持创建/吊销 PAT | `rg-http/src/api/users.rs` | 已有 entity |
| R-07 | 仓库删除 | 软删除仓库（deleted_at 标记），清理 Git 数据和 DB 记录 | `rg-core/src/repo/service.rs`, `rg-http/src/api/repos.rs` | 无 |
| R-08 | 仓库转移 | 将仓库所有权从用户 A 转移到用户 B 或组织 | `rg-core/src/repo/service.rs`, `rg-http/src/api/repos.rs` | 无 |
| R-09 | 全局搜索（FTS5） | SQLite FTS5 全文检索，覆盖代码/Issue/Wiki/仓库名 | `rg-db/`, `rg-http/src/api/search.rs` | 无 |
| R-10 | Commit Status Checks | 创建/更新 Commit SHA 的 Status；关联 Branch Protection 的 required checks | `rg-db/src/entities/`, `rg-core/src/repo/`, `rg-http/src/api/repos.rs` | 已有 protected_branches |

#### P1 — Should Have

| # | 功能 | 描述 |
|---|------|------|
| R-11 | Labels 与 Issue 关联 | Issue 创建/编辑时可多选 Labels，支持 Label 筛选 |
| R-12 | Webhooks 支持更多事件 | push 之外支持 release/branch/tagcreate 等事件 |
| R-13 | Watch 通知 | 用户 Watch 仓库后，push/PR/Milestone 更新触发通知 |

#### P2 — Nice to Have

| # | 功能 | 描述 |
|---|------|------|
| R-14 | Fork 合并请求 | Fork 向上游发起 PR（需处理跨仓库 diff） |
| R-15 | Release 下载统计 | 统计各 Release Asset 下载量 |
| R-16 | Search API 集成 | GitHub 风格 `/search/code` 和 `/search/issues` 端点 |

### UI 设计稿

#### 3.1 仓库页面 — Fork / Star / Watch 入口

```
┌──────────────────────────────────────────────┐
│ [Logo] 搜索框 [🔔] [+] [Avatar ▼]            │
├──────────────────────────────────────────────
│ 仓库名: owner / repo        [⭐ Star] [👁 Watch ▼] [⚡ Fork] │
│ 分支: main ▼  Tags   Releases(3)              │
├──────────────────────────────────────────────
│ Code  Issues(12)  Pull Requests  Wiki  CI    │
│ ─────────────────────────────────────────────│
│ (文件浏览 / Issue 列表 / Release 列表 等)      │
└──────────────────────────────────────────────┘
```

- **Star**: 点击切换 ⭐/☆，数字 +1/-1，异步更新
- **Watch**: 下拉菜单 three states（Not Watching / Watching / Ignoring）
- **Fork**: 点击后跳转 `/repos/:owner/:repo/fork`，选择目标 namespace

#### 3.2 Releases 页面

```
┌─────────────────────────────────────────────────────┐
│ Releases (3)                              [+ New]  │
│ ─────────────────────────────────────────────────  │
│ 🏷 v1.2.0 — Latest        2026-05-01               │
│    标题：New Features                              │
│    [Browse Files] [Tag: v1.2.0] [Downloads: 42]   │
│ ─────────────────────────────────────────────────  │
│ 🏷 v1.1.0                  2026-04-15              │
│    标题：Bug Fixes                                  │
│    [Browse Files] [Tag: v1.1.0] [Downloads: 18]   │
└─────────────────────────────────────────────────────┘
```

#### 3.3 Labels 管理（设置页）

```
┌──────────────────────────────────────────────────┐
│ Settings > Labels                    [+ New]   │
│ ───────────────────────────────────────────────  │
│ [🐛 bug      #ff0000]  严重缺陷       [✏️] [🗑] │
│ [✨ feature  #00ff00]  新功能         [✏️] [🗑] │
│ [⚠️  warning  #ffaa00]  警告信息       [✏️] [🗑] │
└──────────────────────────────────────────────────┘
```

- 颜色选择器：预设 8 色 + 自定义 hex
- 内置默认 Labels（bug/enhancement/help wanted/documentation）

#### 3.4 全局搜索

```
┌─────────────────────────────────────────────┐
│ 🔍 [搜索框: "authentication error"      ▼]  │
│                                             │
│ 搜索: Everywhere ▼   仓库: All ▼             │
│ ─────────────────────────────────────────── │
│ [All] [Code] [Issues] [Repos] [Wiki]        │
│                                             │
│ 📁 auth module: src/auth.rs                  │
│    ... if auth_result.is_err() {            │
│    →   return Err(AuthenticationError::...  │
│    2 matches in ironforge/rg-core           │
│                                             │
│ 📋 "authentication error" in Issues          │
│    Issue #42: authentication error on login  │
│    open · bug · priority:high               │
└─────────────────────────────────────────────┘
```

#### 3.5 Commit Status Checks

```
┌──────────────────────────────────────────────────────┐
│ Commits > abc1234                                   │
│ ─────────────────────────────────────────────────── │
│ ● abc1234 "fix: auth"  许清楚 · 2h ago             │
│   └── ✅ CI/CD: Test Suite      passed · 5m         │
│   └── ⚠️  Code Review          pending              │
│   └── ❌ Security Scan         failed · 1m          │
│       └── [View details] [Re-run jobs]             │
└──────────────────────────────────────────────────────┘
```

#### 3.6 API Tokens 管理（设置页）

```
┌────────────────────────────────────────────────────────┐
│ Settings > API Tokens                     [+ New]     │
│ ─────────────────────────────────────────────────────  │
│ ✓ deploy-token      scrt_...  ·  read:repo  · 2026-08 │
│ ✓ ci-automation     scrt_...  ·  all       · 2026-12 │
│                                                [Revoke]│
│ ─────────────────────────────────────────────────────  │
│ 创建新 Token:                                      │
│ Token 名称: [____________]  过期: [2026-12-01 ▼]     │
│ 权限范围: [✓] repos [✓] issues [ ] admin            │
│                                       [Generate]     │
└────────────────────────────────────────────────────────┘
```

### Open Questions

| # | 问题 | 建议方案 |
|---|------|---------|
| OQ-1 | Fork 时是否同时复制 Wiki 和 LFS 对象？ | P0 阶段仅复制 Git 数据，Wiki/LFS 在 Phase 2 补充 |
| OQ-2 | 全局搜索 FTS5 索引更新策略？ | 异步后台任务（trigger on write），初版用定时刷新 |
| OQ-3 | Commit Status Checks 的状态来源？ | 预置 IronForge CI 自身 status；支持第三方 webhooks 上报 |
| OQ-4 | 仓库删除是否级联删除 Wiki/LFS/PR 等关联数据？ | 软删除 + 级联软删除，Phase 2 考虑硬删除清理任务 |
| OQ-5 | API Token 的权限粒度？ | P0 简单两级（read/write/admin），不实现 Gitea 的细粒度 scope |
| OQ-6 | Labels 的颜色是否允许自定义？ | 预设 8 色 + 自定义 hex 颜色输入 |
| OQ-7 | Watch 是否支持通知邮件？ | P0 仅存储状态，通知邮件在 Phase 2（已有 email基础设施） |
| OQ-8 | 仓库转移后原 fork 关系如何处理？ | Fork 关系保持，仅更新上游指针；新增 `origin_repo_id` 字段 |

---

## 4. Phase 概览

```
┌─────────────────────────────────────────────────────────────┐
│                    IronForge P0 实施计划                      │
│                                                             │
│  Phase 1 ────────────────────────────────── Phase 5          │
│  协作基础      Phase 2       Phase 3      质量与治理    Phase 4│
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌──────────┐   │
│  │ R-02 Star │  │ R-04     │  │ R-10 Commit │  │ R-01 Fork │   │
│  │ R-07 删除 │  │  Labels   │  │  Status   │  │ R-08 转移 │   │
│  │ R-03     │  │ R-05     │  │           │  │          │   │
│  │ Releases │  │ Milestones│  │           │  │          │   │
│  │ /Tags    │  │ R-06 PAT  │  │           │  │          │   │
│  └──────────┘  └──────────┘  └──────────┘  └──────────┘   │
│       │            │             │              │            │
│       ▼            ▼             ▼              ▼            │
│  Phase 5: 全局搜索 R-09（FTS5）+ 收尾集成测试                 │
└─────────────────────────────────────────────────────────────┘
```

### Phase 1: 协作基础（用户可见度最高）

| 功能 | 说明 | 交付物 |
|------|------|--------|
| Star/Watch 仓库 | stars_count 累加，watch 状态三态（none/watching/ignoring） | API + 前端组件 |
| 仓库删除 | 软删除 + Git 数据清理 | API (`DELETE /repos/:owner/:name`) |
| Releases/Tags | 创建/编辑/删除 Release，关联 Tag 列表 | API + 前端 Releases 页面 |

> **依赖关系**: 无，三项互相独立，可并行开发

### Phase 2: 项目管理 API（Labels + Milestones + PAT）

| 功能 | 说明 | 交付物 |
|------|------|--------|
| Labels CRUD | labels 独立表（替换 issues.labels JSON 列），颜色/描述/仓库关联 | API + 设置页 |
| Milestones API | 已有 entity 补 REST API（list/create/update/delete） | API |
| API Tokens / PAT | 已有 access_tokens entity 补 REST API，支持创建/吊销 | API + 设置页 |

> **依赖关系**: Labels → Issue Labels 关联（先有 Labels 才能关联）；Milestones/PAT 互相独立

### Phase 3: 质量与治理（Commit Status + 集成）

| 功能 | 说明 | 交付物 |
|------|------|--------|
| Commit Status Checks | 创建/更新 SHA 状态（pending/success/failure/error），关联 branch protection required checks | API + Commit 详情页 |
| Status 与 Branch Protection 集成 | branch protection 页面显示 required checks 状态 | 前端集成 |

> **依赖关系**: Phase 3 → 依赖 Phase 1 仓库删除和 Phase 2 完成（因为 Status Checks 需要仓库上下文）

### Phase 4: Fork 与仓库转移

| 功能 | 说明 | 交付物 |
|------|------|--------|
| Fork 仓库 | 复制 Git 数据 + fork_id 双向关联；fork 列表页 | API + 前端 Fork 按钮 |
| 仓库转移 | 更新 owner_id，支持用户→用户、用户→组织 | API (`POST /repos/:owner/:name/transfer`) |

> **依赖关系**: Fork → Phase 1 Star（fork 列表需要 star 基础设施）；转移 → 独立

### Phase 5: 全局搜索与收尾

| 功能 | 说明 | 交付物 |
|------|------|--------|
| 全局搜索 FTS5 | SQLite FTS5 索引代码/Issue/Wiki/仓库名 | API (`GET /search`) + 前端搜索页 |
| 端到端集成测试 | 全流程回归测试（基于 Phase 1-4） | 测试报告 |
| 文档更新 | 更新 CLAUDE.md 实现现状 | docs/p0-prd.md + CLAUDE.md |

> **依赖关系**: Phase 5 → 所有前置 Phase 完成

### 实施优先级矩阵

```
         高用户价值
              ▲
              │
    Phase 4   │   Phase 2
   (Fork/转移) │  (Labels/Mile/PAT)
              │
◄─────────────┼────────────────────► 高技术依赖
   Phase 1    │   Phase 3
  (Star/删除) │  (Status)
  (Releases)  │
              │
              ▼
         低用户价值
```

**推荐实施顺序**: Phase 1 → Phase 2 → Phase 4 → Phase 3 → Phase 5
- Phase 1 用户感知最强（Star/Releases/删除），优先交付
- Phase 2 补充项目管理基础，复杂度适中
- Phase 4（Fork/转移）涉及数据迁移，压后处理
- Phase 3（Status）依赖分支保护已有逻辑，Phase 4 后实施
- Phase 5 收尾，搜索是锦上添花

---

## 附录：数据库变更摘要

| 操作 | 表 | 变更类型 |
|------|-----|---------|
| 新建 | `labels` | CREATE TABLE（含 repo_id, name, color, description）|
| 新建 | `commits_statuses` | CREATE TABLE（含 repo_id, sha, state, context, target_url）|
| 新建 | `repo_watches` | CREATE TABLE（user_id, repo_id, watch_state）|
| 修改 | `repositories` | ADD COLUMN `deleted_at`（软删除）|
| 修改 | `issues` | 将 `labels` 列从 JSON 迁移到 `issue_labels` 关联表 |
| 修改 | `repositories` | ADD COLUMN `origin_repo_id`（Fork 关系）|
| 已有 | `access_tokens` | 无 DDL（仅加 API 层） |
| 已有 | `milestones` | 无 DDL（仅加 API 层） |
| FTS | 新建 | `repos_fts` / `issues_fts` / `wiki_pages_fts` |

---

*文档版本：v1.0 | 如有疑问请联系产品经理 许清楚*
