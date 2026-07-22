# IronForge 文档索引

> 单一事实来源的文档导航。当前代码状态以 `CLAUDE.md`「实现现状」与 `architecture/` 系列为准；本报告集为 2026-07-08 文档整理合并后的结构。

---

## 入口文档（先读这些）

| 文件 | 用途 |
|------|------|
| `README.md` | 项目说明、快速开始、REST API 示例 |
| `AGENT.md` | AI 工具轻量统一入口（概览） |
| `CLAUDE.md` | AI 深度协作上下文（踩坑/依赖/现状）⭐ 权威源 |
| `AGENTS.md` | Codex 等工具入口（指针，指向 `CLAUDE.md`） |
| `ARCHITECTURE.md` | 历史架构方案（当前事实以 `architecture/` 为准） |
| `CONTRIBUTING.md` | 开发规范、crate 边界、编码规范 |
| `contributor-quickstart.md` | 贡献者快速上手（5 分钟跑起来 + 提 PR） |

---

## 设计文档（`docs/`）

| 文件 | 内容 |
|------|------|
| `p0-prd.md` | P0 功能 PRD |
| `p0-system-design.md` | P0 系统设计 |
| `p0-completion-plan.md` | P0 完善方案（剩余缺口） |
| `git-protocol.md` | Git 协议实现细节与踩坑 |
| `ai-agent-integration.md` | AI Agent 集成方案（MCP + REST + prompt） |
| `project-audit-2026-06.md` | 项目审计与进度报告（已移至 `testing/`） |

---

## 架构（`ironforge-docs/architecture/`）

| 文件 | 内容 |
|------|------|
| `project-architecture-2026-07.md` | 架构总览（按层：定位/入口/Workspace/数据/HTTP/Git-SSH/安全/CI/部署） |
| `frontend-backend-structure-2026-07.md` | 前后端结构与页面/API 映射 |
| `architecture-followups-2026-07.md` | 已修复项、P2 长期方向、旧口径修正 |
| `system-analysis-by-module-2026-07.md` | 分模块深入分析 + 风险与优先级矩阵 |
| `db-multi-backend-design-2026-07.md` | PostgreSQL 多后端设计 |

---

## 分析与优化（`ironforge-docs/analysis/`）

| 文件 | 内容 |
|------|------|
| `improvement-analysis.md` | 改进与优化整合报告（2026-06-09 规划 + 2026-06-17 落地，含演进时间线与优先级矩阵） |

---

## 对比（`ironforge-docs/comparison/`）

| 文件 | 内容 |
|------|------|
| `mainstream-git-platform-gap-analysis-2026-07-10.md` | vs GitHub/GitLab/Gitea/Forgejo 当前差距、P0 上线阻断项与实施顺序 |
| `gitea-vs-ironforge-2026.md` | vs Gitea 1.26 历史功能对比（v3.1 的 ~85% 旧口径，已由可验证矩阵替代） |
| `gitea-gap-list.csv` | Gitea 1.26.4 加权对齐矩阵（67 项，含代码/测试证据、状态、得分与任务 ID） |
| `gitea-alignment-matrix-2026-07-13.md` | 对齐评分口径、领域得分与基线结论（当前 75.4%） |
| `gitea-detail-alignment-plan-2026-07-13.md` | vs Gitea 1.26.4 细节功能对齐计划、任务 ID、依赖与验收门 |
| `gitea-alignment-progress-2026-07-13.md` | Gitea 1.26.4 对齐进度台账（DONE/DOING/READY/TODO/BLOCKED、证据与变更记录） |

---

## CI/CD（`ironforge-docs/ci/`）

| 文件 | 内容 |
|------|------|
| `ci-runner-architecture.md` | CI Runner 调度架构（Agent 生命周期 / Artifact / Job 调度） |
| `adr-0001-third-party-actions-execution.md` | 第三方 Actions 执行模型、Runner 隔离、网络、Secret、fork 与兼容边界（Accepted） |

## 存储（`ironforge-docs/storage/`）

| 文件 | 内容 |
|------|------|
| `blob-storage-contract-2026-07.md` | BlobStorage 接口、对象键、原子写入、生命周期、legacy 兼容与备份边界 |

## Issue / PR（`ironforge-docs/issues/`）

| 文件 | 内容 |
|------|------|
| `markdown-template-compatibility-2026-07.md` | Gitea 1.26.4 Markdown Issue/PR Template 的发现路径、front matter、chooser config、API、权限与已知边界 |
| `attachment-contract-2026-07.md` | Issue/PR/评论附件的数据归属、Blob key、配额、权限、补偿和 API 契约 |

---

## 测试与审计（`ironforge-docs/testing/`）

| 文件 | 内容 |
|------|------|
| `functional-test-guide-2026-07-03.md` | 功能测试指南 |
| `audit-report-2026-07-03.md` | 前后端缺陷与设计矛盾审计 |
| `project-audit-2026-06.md` | 项目审计与进度报告（2026-06，演进记录） |
| `git-protocol-client-matrix-2026-07.md` | HTTP/SSH、V1/V2 的真实 Git 客户端兼容矩阵、降级语义与运行方法 |

> 两份审计（2026-06 / 2026-07-03）保留以反映演进时间线，不强行合并以消除时序信息。

---

## 归档（`ironforge-docs/archive/`）

过程文档、过时报告、散落文件，**仅供追溯，不作为当前事实来源**：

- `project-architecture-analysis-plan-2026-07.md` / `project-architecture-analysis-notes-2026-07.md` — 架构重盘过程记录（正式内容已沉淀到 `architecture/`）
- `architecture-remediation-plan-2026-07.md` — P0/P1 修复波次执行计划（已完成首轮）
- `gitea-feature-gap-analysis.md` / `gix-migration-feasibility-analysis.md` / `gix-migration-status-report.md` — 过时对比/迁移报告
- `p0-update-2026-06-08.md` — P0 包注册表更新记录
- `frontend-layout-audit.md` / `plan.md` / `defect-report-2026-06-23.md` / `defect-fix-report-2026-06-23.md` — 根目录散落文件归位

---

## 文档维护约定

1. 新增能力先更新 `architecture/project-architecture-2026-07.md` 与 `frontend-backend-structure-2026-07.md`，风险项进入 `architecture-followups-2026-07.md`。
2. 过程性分析草稿完成后归档到 `archive/`，保持活跃文档集精简。
3. 分析报告命名带日期（`*-2026-07-03.md`），便于识别时效。
4. 两份历史审计保留在 `testing/` 以反映演进，不合并。

---

## 相关链接

- [ARCHITECTURE.md](../ARCHITECTURE.md) — 历史架构设计背景
- [CLAUDE.md](../CLAUDE.md) — AI 协作上下文（最完整的踩坑记录和实现现状）
- [AGENT.md](../AGENT.md) — AI 助手统一入口
- [Gitea 1.26 发布说明](https://blog.gitea.com/release-of-1.26.0/)
- [gix (gitoxide) 项目](https://github.com/Byron/gitoxide)
