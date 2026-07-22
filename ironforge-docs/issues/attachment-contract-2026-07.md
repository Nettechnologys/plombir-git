# Issue / Pull Request / Comment 附件契约

> 对齐任务：ISSUE-103
> 状态：已完成（2026-07-14）

## 数据与存储

`attachments` 使用独立外键表达四种归属：Issue、Pull Request、Issue Comment、Review Comment。每行同时保存 `repo_id`，所有读取、下载和删除均校验“仓库 + 目标 + 附件”三层归属，跨仓库探测统一返回 404。

Blob key 为 `attachments/{repo_id}/{uuid}/{filename}`。上传先原子发布 Blob，再写 DB；DB 失败时删除 Blob。删除时临时读取 Blob，Blob 删除后再删 DB；DB 失败时尝试恢复 Blob。

## 限制

- 单文件最大 100 MiB，与 Gitea 1.26.4 默认值一致；
- 仓库附件默认总配额 1 GiB；
- 默认扩展名白名单与 Gitea 1.26.4 `[attachment].ALLOWED_TYPES` 一致；
- 文件名最长 255 字节，拒绝路径、控制字符和空文件；
- 上传使用 `multipart/form-data` 的 `attachment` 字段，可用 `?name=` 覆盖展示文件名。

## 权限

- 列表和下载沿用仓库读权限；
- 上传和删除允许目标作者或仓库写入者；
- 匿名私有仓库返回 401，已登录无权用户返回 403；
- 附件 ID 与错误仓库/错误 Issue/PR/评论组合返回 404，避免资源枚举。

## API

Issue 使用 Gitea 兼容的 `/repos/{owner}/{repo}/issues/{number}/assets`；Issue 评论使用 `/issues/comments/{id}/assets`。由于 IronForge 的 PR/Review Comment 是独立模型，补充 `/pulls/{number}/assets` 和 `/pulls/comments/{id}/assets`。集合支持 GET/POST，单附件支持 GET/DELETE。

Web 的 Issue、PR 与评论区域使用统一附件面板，支持上传、下载、大小展示和删除。

## 验证

- 核心文件名/类型校验、SQLite fresh migration、公开/私有仓库、四类目标、跨目标/跨仓库 IDOR、配额边界均有自动化回归；
- 上传按 multipart chunk 写入临时文件后调用 `BlobStorage::put_file`，本地下载使用流式响应，删除补偿使用临时文件备份/恢复，不按 100 MiB 上限整块驻留内存；
- 迁移 DDL 在 SQLite、PostgreSQL、MySQL 三种 query builder 上生成通过；实库迁移由现有 CI service-container smoke 持续执行；
- Playwright 在 1440×1000 与 390×844 验证 Issue、Issue Comment、PR、Review Comment 面板，完成上传、下载内容校验、删除，无控制台错误、5xx 或横向溢出。
