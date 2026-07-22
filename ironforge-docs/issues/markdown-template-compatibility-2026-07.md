# Markdown Issue / Pull Request Template 兼容约定

> 基线：Gitea 1.26.4
> 状态：ISSUE-101 已完成

## 1. 发现范围

IronForge 只从仓库的默认分支读取模板，不执行工作区文件，也不跟随符号链接。

Issue Markdown 模板按以下目录顺序发现，且只读取目录直属的 `*.md` 文件：

1. `ISSUE_TEMPLATE` / `issue_template`
2. `.gitea/ISSUE_TEMPLATE` / `.gitea/issue_template`
3. `.github/ISSUE_TEMPLATE` / `.github/issue_template`
4. `.gitlab/ISSUE_TEMPLATE` / `.gitlab/issue_template`

Issue chooser 配置读取 `.gitea` 或 `.github` Issue Template 目录中的 `config.yaml` / `config.yml`。Pull Request Markdown 模板按根目录、`.gitea`、`.github` 的 `PULL_REQUEST_TEMPLATE.md` / `pull_request_template.md` 顺序选择第一个匹配项。

单文件最大 1 MiB，文件必须是 UTF-8；无效文件不会进入选择列表，并在服务日志中记录文件名和错误。空仓库返回空模板列表和默认配置。

## 2. Markdown front matter

模板可以用不少于三个连字符的首尾分隔线声明 YAML front matter：

```markdown
---
name: Bug report
about: Report a reproducible problem
title: "[Bug] "
labels: [bug, triage]
assignees: maintainer
ref: main
---

## Steps to reproduce
```

支持字段：

| 字段 | 行为 |
|------|------|
| `name` | 选择卡标题；缺失时使用文件名 |
| `about` | 选择卡说明；兼容旧字段 `description`；缺失时使用正文摘要 |
| `title` | 新 Issue 标题前缀或默认标题 |
| `labels` | 字符串、逗号分隔字符串或字符串数组 |
| `assignees` | 字符串、逗号分隔字符串或字符串数组；当前返回给客户端，创建接口的多指派由 ISSUE-105 跟踪 |
| `ref` | 兼容元数据；当前返回给客户端，不切换 Issue 所属分支 |

front matter 解析失败时按普通 Markdown 处理，避免因为用户正文中的 YAML 类文本使整个模板列表不可用。

## 3. chooser config

支持 Gitea/GitHub 风格字段：

```yaml
blank_issues_enabled: false
contact_links:
  - name: Support
    url: https://example.com/support
    about: Ask usage questions here
```

联系链接必须有 `name`、`about`，且 URL 必须是绝对 `http` / `https` 地址。前端以新窗口打开链接并设置 `noopener noreferrer`。`blank_issues_enabled: false` 时不显示空白 Issue 入口。

## 4. REST 与 Web 行为

| API | 返回 |
|-----|------|
| `GET /api/v1/repos/{owner}/{repo}/issue_templates` | Markdown Issue 模板数组 |
| `GET /api/v1/repos/{owner}/{repo}/issue_config` | chooser 配置或默认配置 |
| `GET /api/v1/repos/{owner}/{repo}/issue_config/validate` | `{ valid, message }` |
| `GET /api/v1/repos/{owner}/{repo}/pull_request_template` | PR 模板；不存在时 `204` |

公开仓库允许匿名读取；私有仓库沿用统一仓库读权限，匿名访问返回 `401`，无权限用户返回 `403`。Web 的 New Issue 流程先展示模板、空白入口和联系链接；选中模板后预填标题、正文和标签。New Pull Request 首次打开创建表单时预填 PR 正文，不覆盖用户已经输入的内容。

## 5. 验证与边界

自动验证：

- `cargo test -p rg-core issue_template --lib`
- `cargo test -p rg-http --test issue_template_tests`
- `npm run check`（`web/`）
- Playwright：1440×900 Issue chooser/预填、PR 预填，以及 390×844 Issue chooser 交互通过；无框架 overlay、page error 或未解释的网络/控制台错误

尚未纳入 ISSUE-101：

- YAML Issue Form 的 schema、字段渲染、校验和提交正文生成（ISSUE-102）；
- Issue 多 Assignee 写入（ISSUE-105）；
- 模板继承、实例级默认模板或外部模板源；
- 非默认分支上的模板发现。
