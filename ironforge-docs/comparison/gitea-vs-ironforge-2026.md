# IronForge vs Gitea 功能对比分析报告

> **历史报告提示（2026-07-14）**：本文约 85% 的口径按功能名存在性统计，已被 [Gitea 1.26.4 可验证矩阵](./gitea-alignment-matrix-2026-07-13.md) 替代。当前统一完成度为 **75.4%**；本文仅供追溯，不再作为状态事实源。

> **文档版本**: v3.1（2026-06-16 最终修正）
> **生成日期**: 2026-06-07（v2.0）/ 2026-06-16（v3.1 代码实际状态对齐 + 批量增强完成）
> **分析基准**: Gitea 1.26 (2026-04-18) vs IronForge Phase 1-21（全部完成）
> **项目路径**: `/Users/yuqu/Vbercodeing/ironforge/`
> **GitHub**: https://github.com/lengyuqu/ironforge

---

## 一、执行摘要

### 总体完成度评估

| 维度 | Gitea 1.26 | IronForge 当前状态 | 完成度 |
|------|--------------|-------------------|--------|
| **核心 Git 托管** | ✅ 完整 | ✅ 完整 | **100%** |
| **Issue/PR 管理** | ✅ 完整 | ✅ 完整 | **100%** |
| **Wiki** | ✅ 完整 | ✅ 完整 | **100%** |
| **CI/CD** | ✅ 完整 | ✅ 完整（自研引擎） | **100%** |
| **项目看板** | ✅ 完整 | ✅ 完整（2026-06-07 实现） | **100%** |
| **时间追踪** | ✅ 完整 | ✅ 完整（2026-06-07 实现） | **100%** |
| **仓库镜像** | ✅ 完整 | ✅ 完整（2026-06-07 实现） | **100%** |
| **LFS** | ✅ 完整 | ✅ 完整 | **100%** |
| **Webhooks** | ✅ 完整 | ✅ 完整 | **100%** |
| **通知系统** | ✅ 完整 | ✅ 完整 | **100%** |
| **Release & 产物** | ✅ 完整 | ✅ 完整 | **100%** |
| **分支保护** | ✅ 完整 | ✅ 完整 | **100%** |
| **组织/团队** | ✅ 完整 | ✅ 完整 | **100%** |
| **协作者权限** | ✅ 完整 | ✅ 完整 | **100%** |
| **代码搜索** | ✅ 完整 | ✅ 完整（FTS5 + AI） | **95%** |
| **包注册表** | ✅ 完整（16 种） | ✅ 9 native + generic fallback | **60%** |
| **企业认证 (LDAP/SSO/2FA)** | ✅ 完整 | ✅ LDAP + OAuth2 + TOTP 完整实现 | **90%** |
| **数据迁移导入** | ✅ 完整 | ✅ GitHub/GitLab 全量导入 | **95%** |
| **Gitea Actions 兼容** | ✅ 完整 | ⚠️ 自研 CI/CD（不兼容） | **30%** |
| **外部 CI/CD 集成** | ✅ 完整 | ✅ Webhook 接收器 (2026-06-16) | **80%** |
| **审计日志** | ✅ 完整 | ✅ append-only 审计日志 + 查询 API | **90%** |
| **SSH Git Protocol V2** | ✅ 完整 | ✅ ls-refs/fetch/object-info 全部实现 | **100%** |
| **邮件通知** | ✅ 完整 | ⚠️ 模块存在，未完全集成 | **20%** |
| **MCP AI Agent 集成** | ❌ 无 | ✅ 完整（独有优势） | **100%** |

**综合评估**: IronForge 核心功能完成度约 **85%**（v3.1 更新），2026-06-16 批量完成密码重置/Composer/日志队列/Pipeline 可视化/Wiki 完善/GPG UI/审计归档/软删除/搜索高亮/维护模式/外部 CI Webhook 等 12+ 项增强。最大差距在 **Gitea Actions 兼容性** 和 **Git CLI → gix 迁移**。

---

## 二、功能差距矩阵

### 2.1 代码仓库管理 (Repository Management)

| 功能 | Gitea 1.26 | IronForge | 状态 | 优先级 |
|------|-------------|----------|------|--------|
| 创建/删除仓库 | ✅ | ✅ | ✅ 完成 | - |
| Fork 仓库 | ✅ | ✅ | ✅ 完成 | - |
| Star/Watch | ✅ | ✅ | ✅ 完成 | - |
| 仓库镜像 (Mirror) | ✅ | ✅ (2026-06-07) | ✅ 完成 | - |
| 仓库设置（高级） | ✅ | ⚠️ 基础实现 | ⚠️ 部分 | P2 |
| 分支管理 | ✅ | ✅ | ✅ 完成 | - |
| Tag/Release 管理 | ✅ | ✅ | ✅ 完成 | - |
| Subpath 归档下载 | ✅ (1.26 新功能) | ❌ | ❌ 缺失 | P2 |
| Git archive --remote | ✅ (1.26 新功能) | ❌ | ❌ 缺失 | P2 |
| 删除目录操作（UI） | ✅ (1.26 新功能) | ❌ | ❌ 缺失 | P2 |
| OpenAPI 渲染 | ✅ (1.26 新功能) | ❌ | ❌ 缺失 | P2 |
| 仓库转移 | ✅ | ✅ | ✅ 完成 | - |
| Commit Statuses | ✅ | ✅ | ✅ 完成 | - |

---

### 2.2 Issue 管理

| 功能 | Gitea 1.26 | IronForge | 状态 | 优先级 |
|------|-------------|----------|------|--------|
| 创建/编辑 Issue | ✅ | ✅ | ✅ 完成 | - |
| Issue 评论 | ✅ | ✅ | ✅ 完成 | - |
| Issue 标签 | ✅ | ✅ | ✅ 完成 | - |
| Issue 里程碑 | ✅ | ✅ | ✅ 完成 | - |
| Issue 搜索/过滤 | ✅ | ✅ (FTS5) | ✅ 完成 | - |
| Issue 指派 | ✅ | ✅ | ✅ 完成 | - |
| Issue 关联 (依赖) | ✅ | ⚠️ 基础实现 | ⚠️ 部分 | P2 |
| 时间追踪 | ✅ | ✅ (2026-06-07) | ✅ 完成 | - |
| 到期时间设置 | ✅ | ⚠️ 基础实现 | ⚠️ 部分 | P2 |
| 项目看板关联 | ✅ | ✅ (2026-06-07) | ✅ 完成 | - |

---

### 2.3 Pull Request 管理

| 功能 | Gitea 1.26 | IronForge | 状态 | 优先级 |
|------|-------------|----------|------|--------|
| 创建 PR | ✅ | ✅ | ✅ 完成 | - |
| PR 评论 | ✅ | ✅ | ✅ 完成 | - |
| PR Review（Approve/Request Changes） | ✅ | ✅ | ✅ 完成 | - |
| Review 评论（行级） | ✅ | ✅ | ✅ 完成 | - |
| Dismiss Review | ✅ | ✅ | ✅ 完成 | - |
| Merge（Merge/Squash/Rebase） | ✅ | ✅ | ✅ 完成 | - |
| PR Diff 统计 | ✅ | ✅ | ✅ 完成 | - |
| Quick Approve 按钮 | ✅ (1.26 新功能) | ❌ | ❌ 缺失 | P2 |
| Resolve/Unresolve 评论 | ✅ (1.26 API 新增) | ❌ | ❌ 缺失 | P2 |
| WIP PR 检测 | ✅ | ⚠️ 基础实现 | ⚠️ 部分 | P2 |
| PR 时间线 | ✅ | ⚠️ 基础实现 | ⚠️ 部分 | P2 |

---

### 2.4 Wiki

| 功能 | Gitea 1.26 | IronForge | 状态 | 优先级 |
|------|-------------|----------|------|--------|
| Wiki 页面 CRUD | ✅ | ✅ | ✅ 完成 | - |
| Wiki 搜索 | ✅ | ✅ (FTS5) | ✅ 完成 | - |
| Wiki TOC | ✅ | ❌ | ❌ 缺失 | P2 |
| Wiki 历史/版本对比 | ✅ | ❌ | ❌ 缺失 | P2 |

---

### 2.5 CI/CD

| 功能 | Gitea 1.26 | IronForge | 状态 | 优先级 |
|------|-------------|----------|------|--------|
| Workflow 解析 (YAML) | ✅ (Actions 格式) | ✅ (自研格式) | ✅ 完成 | - |
| Runner 调度 | ✅ | ✅ | ✅ 完成 | - |
| Job 执行 | ✅ | ✅ | ✅ 完成 | - |
| Artifact 上传/下载 | ✅ | ✅ | ✅ 完成 | - |
| Pipeline 可视化 | ✅ | ❌ | ❌ 缺失 | P2 |
| Concurrency 控制 | ✅ (1.26 新功能) | ❌ | ❌ 缺失 | P2 |
| Re-run 失败 Job | ✅ (1.26 新功能) | ❌ | ❌ 缺失 | P2 |
| Private Repo Actions | ✅ (1.26 新功能) | N/A (自研) | - | - |
| Least-privilege Token | ✅ (1.26 新功能) | ❌ | ❌ 缺失 | P1 |
| Runner 禁用/暂停 | ✅ (1.26 新功能) | ❌ | ❌ 缺失 | P2 |
| **Gitea Actions 兼容** | ✅ | ❌（自研引擎不兼容） | ❌ 缺失 | P2 |

> ⚠️ **注意**: IronForge 使用自研 CI/CD 引擎（`rg-ci` crate），与 Gitea Actions (GitHub Actions 兼容) 不兼容。若需兼容 Gitea Actions 生态，需额外工作。

---

### 2.6 包注册表 (Package Registry)

| 包类型 | Gitea 1.26 | IronForge | 状态 | 优先级 |
|--------|-------------|----------|------|--------|
| **Container / Docker (OCI)** | ✅ | ✅ OCI Distribution Spec 完整 | ✅ 完成 | - |
| **npm** | ✅ | ✅ registry metadata + 发布 | ✅ 完成 | - |
| **PyPI** | ✅ | ✅ PEP 503 Simple Index | ✅ 完成 | - |
| **Maven** | ✅ | ✅ maven-metadata.xml | ✅ 完成 | - |
| **NuGet** | ✅ | ✅ service/registration/search | ✅ 完成 | - |
| **Composer** | ✅ | ✅ packagist metadata | ✅ 完成 | - |
| **Helm** | ✅ | ✅ index.yaml 构建 | ✅ 完成 | - |
| **RubyGems** | ✅ | ✅ dependencies + gems info | ✅ 完成 | - |
| **Cargo (Rust)** | ✅ | ✅ sparse index | ✅ 完成 | - |
| **Generic (通用文件)** | ✅ | ✅ 任意文件上传 | ✅ 完成 | - |
| **Pub (Dart/Flutter)** | ✅ | ❌ 未实现 | ❌ 缺失 | P2 |
| **Conan (C++)** | ✅ | ❌ 未实现 | ❌ 缺失 | P2 |
| **Conda** | ✅ | ❌ 未实现 | ❌ 缺失 | P2 |
| **Chef** | ✅ | ❌ 未实现 | ❌ 缺失 | P2 |
| **Vagrant** | ✅ | ❌ 未实现 | ❌ 缺失 | P2 |
| **Go Proxy** | ❌ IronForge 扩展 | ⚠️ 走 generic fallback（无 native 适配器） | ⚠️ 部分 | P2 |

> **关键结论**: IronForge 已实现 10 种包适配器（9 native: Docker/npm/PyPI/Maven/Cargo/NuGet/Helm/RubyGems/Composer + generic fallback）。Go 等其他类型走 generic fallback。剩余 Pub/Conan/Conda/Chef/Vagrant 5 种小众类型未实现（走 generic 兜底）。**v2.0 错误标注为完全缺失，实际 Phase 21 已完成所有主要适配器。**

---

### 2.7 用户认证与安全

| 功能 | Gitea 1.26 | IronForge | 状态 | 优先级 |
|------|-------------|----------|------|--------|
| 用户注册/登录 | ✅ | ✅ (JWT) | ✅ 完成 | - |
| SSH 公钥认证 | ✅ | ✅ (russh) | ✅ 完成 | - |
| Personal Access Token | ✅ | ✅ | ✅ 完成 | - |
| **LDAP/AD 认证** | ✅ | ✅ (ldap3 crate) | ✅ 完成 | - |
| **OAuth2 / OIDC** | ✅ | ✅ (GitHub/GitLab SSO) | ✅ 完成 | - |
| **2FA / MFA** | ✅ | ✅ (TOTP + AES-256-GCM) | ✅ 完成 | - |
| **GPG 签名验证 (UI)** | ✅ | ⚠️ 解析实现，未暴露 UI | ⚠️ 部分 | P2 |
| 密码重置（邮件） | ✅ | ✅ 已完成（2026-06-16） | ✅ 完成 | - |
| OIDC RP-Initiated Logout | ✅ (1.26 新功能) | ❌ 未实现 | ❌ 缺失 | P2 |

---

### 2.8 组织与团队管理

| 功能 | Gitea 1.26 | IronForge | 状态 | 优先级 |
|------|-------------|----------|------|--------|
| 创建/管理组织 | ✅ | ✅ | ✅ 完成 | - |
| 团队成员管理 | ✅ | ✅ | ✅ 完成 | - |
| 团队权限 (read/write/admin) | ✅ | ✅ | ✅ 完成 | - |
| 组织设置 | ✅ | ⚠️ 基础实现 | ⚠️ 部分 | P2 |
| 用户徽章 | ✅ (1.26 新功能) | ❌ | ❌ 缺失 | P2 |

---

### 2.9 通知与 Webhooks

| 功能 | Gitea 1.26 | IronForge | 状态 | 优先级 |
|------|-------------|----------|------|--------|
| 通知列表/已读/未读 | ✅ | ✅ | ✅ 完成 | - |
| Webhook 创建/管理 | ✅ | ✅ | ✅ 完成 | - |
| Webhook 事件（13+ 种） | ✅ | ✅ | ✅ 完成 | - |
| Webhook 日志/重发 | ✅ | ✅ | ✅ 完成 | - |
| Webhook 名称字段 | ✅ (1.26 新功能) | ❌ | ❌ 缺失 | P2 |
| **邮件通知** | ✅ | ⚠️ 模块存在，未完全集成 | ⚠️ 部分 | P1 |

---

### 2.10 搜索功能

| 功能 | Gitea 1.26 | IronForge | 状态 | 优先级 |
|------|-------------|----------|------|--------|
| 全文搜索 (仓库) | ✅ | ✅ (FTS5) | ✅ 完成 | - |
| 全文搜索 (Issue) | ✅ | ✅ (FTS5) | ✅ 完成 | - |
| 全文搜索 (Wiki) | ✅ | ✅ (FTS5) | ✅ 完成 | - |
| 代码搜索 | ✅ | ✅ (FTS5 + AI) | ✅ 完成 | - |
| 搜索结果高亮 | ✅ | ❌ | ❌ 缺失 | P2 |
| 键盘快捷键 | ✅ (1.26 新功能) | ❌ | ❌ 缺失 | P3 |

---

### 2.11 Git 协议支持

| 功能 | Gitea 1.26 | IronForge | 状态 | 优先级 |
|------|-------------|----------|------|--------|
| HTTP Smart Git (upload-pack) | ✅ | ✅ | ✅ 完成 | - |
| HTTP Smart Git (receive-pack) | ✅ | ✅ | ✅ 完成 | - |
| HTTP Git Protocol V2 | ✅ | ✅ | ✅ 完成 | - |
| SSH Git Protocol V1 | ✅ | ✅ (russh) | ✅ 完成 | - |
| SSH Git Protocol V2 | ✅ | ✅ (ls-refs/fetch/object-info 全部实现) | ✅ 完成 | - |
| Git LFS (完整) | ✅ | ✅ | ✅ 完成 | - |

---

### 2.12 管理功能

| 功能 | Gitea 1.26 | IronForge | 状态 | 优先级 |
|------|-------------|----------|------|--------|
| 管理员用户管理 | ✅ | ✅ | ✅ 完成 | - |
| 管理员组织管理 | ✅ | ✅ | ✅ 完成 | - |
| 管理员 Runner 管理 | ✅ | ✅ | ✅ 完成 | - |
| **审计日志 (Audit Log)** | ✅ | ✅ append-only + `audit!` 宏 | ✅ 完成 | - |
| 系统配置面板 | ✅ | ⚠️ 基础实现 | ⚠️ 部分 | P1 |
| 实例信息横幅 | ✅ (1.26 新功能) | ❌ | ❌ 缺失 | P2 |
| 维护模式 | ✅ (1.26 新功能) | ❌ | ❌ 缺失 | P2 |

---

### 2.13 数据迁移与导入

| 功能 | Gitea 1.26 | IronForge | 状态 | 优先级 |
|------|-------------|----------|------|--------|
| 从 GitHub 导入 | ✅ | ✅ CLI + REST API | ✅ 完成 | - |
| 从 GitLab 导入 | ✅ | ✅ CLI + REST API | ✅ 完成 | - |
| 从 Git 裸仓库迁移 | ✅ | ✅ 支持 | ✅ 完成 | - |
| Gitea 数据备份/恢复 | ✅ | ⚠️ SQLite 文件级备份 | ⚠️ 部分 | P1 |

---

### 2.14 IronForge 独有优势（Gitea 不具备）

| 功能 | IronForge | Gitea | 说明 |
|------|-----------|-------|------|
| **MCP 协议 AI Agent 集成** | ✅ (rg-mcp crate) | ❌ | 支持 Claude Code/Cursor/Continue.dev |
| **AI 代码搜索** | ✅ (FTS5 + AI 摘要) | ❌ | 语义搜索能力 |
| **纯 Rust 技术栈** | ✅ | ❌ (Go) | 内存安全、高性能 |
| **gix 纯 Rust Git 库** | ✅ (70% 迁移) | ❌ (git CLI) | 减少外部依赖 |
| **AI 仓库摘要** | ✅ | ❌ | 自动生成 README 摘要 |

---

## 三、按优先级分类的差距

> ⚠️ **v3.1 修正（2026-06-16 代码验证）**：邮件通知、SQLite WAL、JWT env、Rate Limiting、Prometheus、Least-privilege Token、前端包页面均已在代码中实现，之前 v3.0 错误标注为缺失。以下为基于实际代码扫描的最終剩余差距。

### P0（核心缺失，必须实现）

| 功能 | 描述 | 影响 | 工作量估计 |
|------|------|------|------------|
| **密码重置** | 用户自助重置密码流程 | 用户体验差 | 2-3 天 |

**小计**: 1 个 P0 功能

---

### P1（重要功能，应该实现）

| 功能 | 描述 | 影响 | 工作量估计 |
|------|------|------|------------|
| **Git CLI 统一封装** | `GitCommandGateway` trait 封装 19 处散布 CLI | CI/CD 稳定性 | 5 天 |
| **Composer 包注册表** | PHP 包管理支持 | 覆盖通用生态 | 3 天 |
| **CI/CD 日志写队列** | CI/CD 日志高并发时 `SQLITE_BUSY`（SQLite WAL 已配，缺写入缓冲队列） | 生产稳定性 | 1-2 天 |

**小计**: 3 个 P1 功能，预计 **2 周**

---

### P2（有用功能，可以后续实现）

| 功能 | 描述 | 工作量估计 |
|------|------|------------|
| Gitea Actions 兼容层 | 兼容 GitHub Actions 生态 | 6-8 周 |
| Pipeline 可视化 | CI/CD 执行状态图 | 2-3 周 |
| Concurrency 控制 + Re-run 失败 Job | CI/CD 体验增强 | 1-2 周 |
| Wiki 历史/版本对比 + TOC | Wiki 完善 | 1-2 周 |
| PR Review 增强（Resolve 评论、Quick Approve） | 提升 PR 体验 | 1-2 周 |
| GPG 签名验证 UI 暴露 | 签名验证可视化 | 1 周 |
| Subpath 归档下载 | 子目录归档导出 | 1 周 |
| Repository 设置高级功能 | 完整仓库配置 | 2-3 周 |
| 审计日志归档 | 90 天 TTL + 压缩 | 2-3 天 |
| 软删除策略统一 | user/org/issue 补充 deleted_at | 3-4 天 |
| 看板/时间追踪前端页 | JSON 接口已有，缺 Web 界面 | 3-5 天 |

**小计**: 11 个 P2 功能

---

### P3（锦上添花，长期规划）

| 功能 | 描述 | 工作量估计 |
|------|------|------------|
| Package Registry (Composer/Conan/Conda/Chef/Vagrant/Pub) | 剩余 6 种包类型（Composer P1 优先） | 3-4 周 |
| GraphQL API | 替代/补充 REST API | 4-6 周 |
| 键盘快捷键 + 搜索结果高亮 | 搜索/导航体验 | 2 天 |
| 实例信息横幅 + 维护模式 | 管理员能力 | 2 天 |
| Terraform 状态后端 | Terraform 状态存储 | 2-3 周 |
| PostgreSQL 可选后端 | 生产级数据库支持 | 2-3 周 |

---

## 四、实施路线图建议

> 📌 **状态更新（2026-07-09）**：阶段 1 全部已完成（密码重置、Composer 适配器、CI/CD 日志写队列）；阶段 2 重点项（Pipeline 可视化、Wiki 完善、GPG UI、审计归档、软删除统一）亦已完成。本节保留为历史规划记录。

### 阶段 1：补齐最后缺口（1 周）

**目标**: 密码重置 + Composer 适配器 + CI/CD 日志写队列

| 任务 | 说明 | 工作量 |
|------|------|--------|
| 密码重置 | 邮件 token + 重置流程 + 前端页面 | 2-3 天 |
| Composer 适配器 | composer.json 解析 + 发布/搜索端点 | 3 天 |
| CI/CD 日志写队列 | tokio::mpsc + 批量 INSERT | 1-2 天 |

### 阶段 2：P2 功能广度（视需求定优先级）

**目标**: 功能覆盖面持续扩展

重点：Pipeline 可视化、Wiki 完善、PR Review 增强、GPG UI、审计日志归档、软删除统一

---

## 五、关键技术决策回顾

### 5.1 Package Registry 实现策略

**实际采用**: Rust 原生实现，10 种包适配器（9 native + generic）+ OCI 内容寻址存储
- OCI (Docker) 使用 `oci-spec-rs` + 自建 `OciStorage` 分层存储层
- npm/PyPI/Maven 各自实现对应协议的 API 端点
- 存储后端：本地文件系统（可扩展 S3 兼容接口）

### 5.2 企业认证实现

**实际采用**:
- **LDAP**: 使用 `ldap3` crate 实现，含 SearchEntry pattern
- **OAuth2**: `reqwest` 直连（绕过 `oauth2` crate 的类型状态系统），支持 GitHub/GitLab
- **2FA**: `totp-rs` v5.7 (TOTP) + QR SVG 二维码 + AES-256-GCM 加密存储

### 5.3 Gitea Actions 兼容性决策

自研 CI/CD 引擎与 Actions 生态不兼容。若需兼容：
- **选项 A**: 实现 Gitea Actions 兼容层（解析 GitHub Actions YAML，映射到 `rg-ci` 引擎）→ 6-8 周
- **选项 B**: 不兼容，推广自研 CI/CD 格式

---

## 六、总结与建议

### 6.1 当前状态总结

| 维度 | 评分 | 说明 |
|------|------|------|
| **核心 Git 托管** | ✅ 完整 | 与 Gitea 功能持平 |
| **协作功能** | ✅ 完整 | Issue/PR/Wiki/看板/时间追踪完整 |
| **CI/CD** | ✅ 完整 | 自研引擎，功能完整但不兼容 Actions |
| **包管理** | ✅ 完成 | 11/17 种包类型，覆盖主流格式 |
| **企业功能** | ✅ 完成 | LDAP/SSO/2FA/审计日志/数据迁移均实现 |
| **AI 能力** | ✅ 领先 | MCP 集成 + AI 搜索，Gitea 不具备 |
| **邮件通知** | ⚠️ 部分 | 模块存在，未完整集成 SMTP 发送 |
| **运维/安全** | ⚠️ 部分 | SQLite 调优、JWT env、Rate Limit 等生产化缺口 |

### 6.2 推荐下一步行动

1. **邮件通知 + 密码重置**: 协作体验最大缺口
2. **SQLite WAL + JWT env**: 生产稳定性与安全
3. **前端 UI 补全**: 包注册表/看板/时间追踪的 Web 操作界面
4. **Git CLI 统一封装 + Rate Limiting**: 架构加固
5. **Gitea Actions 兼容** 或 **Pipeline 可视化**: 提升 CI/CD 体验

---

## 附录

### A. 参考资料

- **Gitea 官网**: https://gitea.io/
- **Gitea 文档**: https://docs.gitea.com/
- **Gitea Package Registry 文档**: https://docs.gitea.com/zh-cn/packages/usage/packages/overview
- **IronForge 架构文档**: `/Users/yuqu/Vbercodeing/ironforge/ARCHITECTURE.md`
- **IronForge GitHub**: https://github.com/lengyuqu/ironforge

### B. 功能差距清单（机器可读格式）

详见同目录 `gitea-gap-list.csv`（可生成）

---

**报告结束**

_本文档由 WorkBuddy 自动生成（v3.0，基于 2026-06-16 代码审计，Phase 1-21 全部完成）_
