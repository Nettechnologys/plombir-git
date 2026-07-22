# 已归档分析报告

以下报告因内容过时（反映项目早期阶段状态）、已被新版替代，或属于过程 / 临时记录而归档。
**仅供追溯，不作为当前事实来源**；当前事实以 `CLAUDE.md` / `AGENT.md` / `architecture/` 系列为准。

| 文件 | 原生成时间 | 主题与内容摘要 | 与当前的关系 |
|------|-----------|---------------|-------------|
| `gitea-feature-gap-analysis.md` | 2026-05-10 | vs Gitea 1.26 功能差距分析（完成度 40-50% 的早期快照） | 已被 `comparison/gitea-vs-ironforge-2026.md`（v3.1）替代 |
| `gix-migration-feasibility-analysis.md` | 2026-05-09 | Git CLI → gix 库替换的可行性评估 | 使命已完成，gix 迁移已在推进（见 `CLAUDE.md` 实现现状） |
| `gix-migration-status-report.md` | 2026-05-10 | gix 迁移进度报告（~60% / 13 处 CLI） | 数据已过时，最新进度见 `CLAUDE.md`（~70% / 16 处经网关） |
| `p0-update-2026-06-08.md` | 2026-06-08 | P0 包注册表完善记录（PyPI / Maven 适配器实现细节、PEP 503 等） | 已实现并沉淀到代码，属功能落地记录 |
| `ironforge-improvement-analysis-2026-06-09.md` | 2026-06-09 | 全方位改进空间分析（8 维度缺口：SQLite 瓶颈 / JWT 明文 / git CLI fallback / 前端缺口 / 可观测性盲区等） | 已与 `source-optimization-analysis` 融合为 `analysis/improvement-analysis.md` |
| `source-optimization-analysis-2026-06-17.md` | 2026-06-17 | 源码优化空间分析（16 项代码优化，自动化扫描 + 3 Agent 审查） | 已与上文融合为 `analysis/improvement-analysis.md` |
| `project-architecture-analysis-plan-2026-07.md` | 2026-07-05 | 架构重盘的分析原则与步骤计划（从代码 / 配置 / 路由 / 前端 / 运行入口盘点现状） | 过程文档，正式结论已沉淀到 `architecture/` 系列 |
| `project-architecture-analysis-notes-2026-07.md` | 2026-07-05 | 架构重盘逐轮分析过程记录（含逐文件盘点细节） | 过程文档（内容量大），正式结论见 `architecture/` |
| `architecture-remediation-plan-2026-07.md` | 2026-07-05 | 把架构重盘 P0/P1 缺口转成可拆分、可验证的代码修复蓝图（按安全 / 权限 / 部署 / 回归排序） | 首轮修复已执行，相关更新已落到 `CLAUDE.md` / `architecture/` |
| `plan.md` | 2026-07（修订） | Git CLI 统一 & gix 迁移技术债修复计划（交给开发智能体执行，Phase 1+2 已完成，Phase 3 登记待办） | 执行状态 2026-07-04 已标注完成 |
| `defect-report-2026-06-23.md` | 2026-06-23 | 全仓库静态分析 + 安全审计的缺陷汇总（按严重等级统计） | 检查记录，对应修复见 `defect-fix-report` |
| `defect-fix-report-2026-06-23.md` | 2026-06-23 | 已修复缺陷明细（C-1 路径遍历、C-3 类型不匹配等 Critical / High，含文件与修复方式） | 修复落地记录 |
| `frontend-layout-audit.md` | 2026-06-18/19 | 前端布局合理性检查与修复（23 页面样式去重、响应式断点统一、ARIA 可访问性、组件复用） | 前端质量记录，修复已落地 |

> 如需查阅原始内容，直接打开对应文件即可。归档文件内的链接（含原 `Desktop/帮我做个方案` 旧路径）已全部修正为当前仓库位置（`/Users/yuqu/Vbercodeing/ironforge/`）。
