# IronForge BlobStorage 契约与生命周期

> 状态：当前实现
> 日期：2026-07-14
> 任务：`STORAGE-001`
> 后续：`STORAGE-002`（S3/MinIO、迁移与一致性工具）、`OPS-501`（全实例备份恢复）

## 1. 目标和边界

`rg-core::blob_storage` 是持久二进制对象的统一边界。数据库和业务代码使用稳定 `BlobKey`，不再把本地绝对路径当成对象身份；本地磁盘只是当前 backend。

本任务覆盖：

- LFS、Package、OCI、CI Artifact 和 Release Asset 的新写入；
- 本地 backend 的原子写入、读取、存在性、元数据、删除和前缀盘点；
- 历史绝对路径/旧目录布局的读取与删除兼容；
- 未来 Issue/PR 附件及归档缓存的命名和生命周期入口。

本任务不包含：

- S3/MinIO 实现、签名直传/直下和在线迁移工具，这些属于 `STORAGE-002`；
- Issue/PR 附件 API/UI，这些属于 `ISSUE-103`；
- 仓库源码 Git object database。Git 仓库仍由 Git/gix 管理，不进入 BlobStorage；
- CI Cache。Cache 是可再生加速数据，继续使用独立 retention 路径，不进入完整备份的必需集合；
- 即时生成的仓库 zip/tar.gz。当前 archive handler 不落盘；只有未来启用缓存时才使用预留 namespace。

## 2. 接口契约

`BlobStorage` 是 object-safe、`Send + Sync` 的后端接口：

| 操作 | 语义 |
|---|---|
| `put` | 从内存原子发布完整对象 |
| `put_file` | 从临时文件流式复制并原子发布，供 LFS/OCI 等大对象使用 |
| `get` | 读取完整对象；远程 backend 的流式/签名下载在 `STORAGE-002` 扩展 |
| `metadata` / `exists` | 获取大小、修改时间或检查存在性 |
| `delete` | 幂等删除；不存在返回 `false` |
| `list(prefix)` | 稳定排序的对象盘点，是备份和一致性校验入口 |
| `local_path` | 本地 backend 的可选零复制优化；portable 业务不得假定一定存在 |

本地 backend 的 `put`/`put_file` 在目标目录内写随机临时文件，`flush + sync_all` 后 rename。读取者不会观察到部分内容。所有 key 经过长度、控制字符、绝对路径、空段、`.`/`..`、隐藏段和反斜杠校验；用户输入段先 percent encode。读写删除还会 canonicalize 并拒绝越过 backend root 的 symlink。

## 3. 对象键

键使用 `/` 分段、UTF-8 序列化，最大 1024 bytes。数据库保存下表中的 key 或由数据库字段确定性派生 key：

| 类型 | 新对象键 | DB 记录方式 |
|---|---|---|
| LFS | `lfs/{owner}/{repo}/{oid[0..2]}/{oid}.zst` | 由 repo、OID 和 compression 派生 |
| Package | `packages/{owner}/{repo}/{type}/{name}/{version}/{filename}` | `package_files.storage_path` 保存 key |
| OCI Blob | `oci/{owner}/{repo}/blobs/sha256/{hash[0..2]}/{hash}` | `oci_blob.storage_path` 保存 key |
| OCI Manifest | `oci/{owner}/{repo}/manifests/sha256/{hash}` | 由 repository 和 digest 派生 |
| CI Artifact | `artifacts/jobs/{job_id}/{uuid}-{name}` | `artifacts.file_path` 保存 key；字段名暂不迁移以保持 schema 兼容 |
| Release Asset | `releases/{owner}/{repo}/{release_id}/{asset_id}/{filename}` | 由 release asset 行派生 |
| Issue/PR 附件 | `attachments/{repo_id}/{uuid}/{filename}` | `attachments.blob_key`；UUID 使对象键在 DB 自增 ID 分配前即可原子发布 |
| Archive Cache（预留） | `archives/{repo_id}/{commit_sha}/{format}` | 只在引入持久缓存时使用 |

每个用户可控段独立编码，因此 package scope、空格和 Unicode 不会变成路径分隔符。content-addressed 类型仍在业务层校验 sha256；backend 不替业务猜测 digest。

## 4. 写入、删除与数据库一致性

跨数据库和对象存储没有分布式事务，采用可补偿的写入流程：

1. 验证权限、大小、格式和 digest；
2. 使用随机临时对象/文件接收数据；
3. 原子发布 Blob；
4. 创建或更新 DB 元数据；
5. 第 4 步失败时立即删除刚发布的 Blob；
6. 删除时先删除 Blob，再删除 DB 元数据；不存在视为成功；
7. `list` 与 DB 清单的双向差集用于发现 orphan Blob 和 dangling row。

必须先有 DB ID 才能构造 key 的 Release Asset 会先创建行；Blob 发布失败时补偿删除该行。Package 多文件发布中任一 DB 文件行失败，会删除该版本全部 Blob 和已创建的版本/文件行。

服务崩溃仍可能发生在任意两步之间，所以 `STORAGE-002` 必须提供 dry-run/repair 一致性命令。清理器不得按字符串拼接任意绝对路径删除文件，只能使用已验证 key 或受 root canonicalization 保护的 legacy 路径。

## 5. 临时对象和 Retention

| 临时数据 | 位置/责任 | 清理时机 |
|---|---|---|
| LocalBlobStorage `.*.tmp` | 与目标对象同目录 | 成功 rename；错误立即删除；inventory 忽略 |
| LFS 原始上传与压缩临时文件 | legacy LFS upload staging | Blob 发布后删除；失败删除 |
| OCI chunk upload | `_oci_uploads/oci-uploads/...` | digest 校验并发布后删除；过期 upload 清理由 `STORAGE-002` 补齐 |
| CI Artifact | `artifacts/jobs/...` | repository retention policy 到期后通过 BlobStorage 删除 |
| CI Cache | `_ci_cache` 独立路径 | cache retention policy；可丢弃，不是恢复必需数据 |

LFS、Package、OCI、Release Asset 没有通用时间型 retention；删除由各自业务引用和权限规则驱动。未来配额必须以 DB 所有权为准，不能只按 key 中的 owner 字符串授权。

## 6. 历史数据兼容与迁移

新写入立即使用稳定 key。读取/删除仍兼容：

- LFS：`{repo_root}/{owner}.lfs/{repo}/{prefix}/{oid}[.zst]`；
- Package：`package_files.storage_path` 中的历史绝对路径或相对路径；
- OCI：`{oci_root}/{owner}/{repo}/oci/_blobs|_manifests/...`；
- CI Artifact：`{repo_root}/_artifacts/jobs/...` 下的历史绝对路径；
- Release Asset：`{repo_root}/{owner}/{repo}.releases/assets/...`。

兼容读取不是迁移完成。`STORAGE-002` 需要实现：扫描、copy+校验、DB key 切换、断点续跑、dry-run、双写/维护窗口策略和 legacy 清理。迁移前后必须比较对象数、总字节数和 sha256，并抽样跑 LFS、Package、OCI、Artifact、Release 下载。

## 7. 备份与恢复入口

`OPS-501` 应通过 `BlobStorage::list(None)` 获取必需对象清单，连同 DB snapshot、Git repositories、配置和 key/version manifest 一起备份。恢复顺序是 backend 对象、数据库、Git repositories、配置，随后运行一致性校验和协议抽查。

只有 durable namespace（LFS、Package、OCI、Artifact、Release、未来 Attachment）是完整恢复必需集合；临时 upload、LocalBlobStorage 临时文件和 CI Cache 默认排除。

## 8. 当前限制

- `get` 当前返回完整 bytes；本地 LFS/OCI 使用 `local_path` 保持流式响应，远程 backend 的 streaming/signed URL 由 `STORAGE-002` 增加；
- `oci_storage_path` 非空时仍启用独立 local compatibility backend，并打印 warning；迁移到统一 backend 后应移除该例外；
- OCI upload session 过期清理尚未实现；
- 数据库列名 `file_path`/`storage_path` 暂保留，值语义已从“绝对路径”改为“BlobKey 或 legacy path”；
- 一致性扫描和在线 legacy 迁移工具尚未实现，因此 `STORAGE-001` 的完成验收必须明确这些属于 `STORAGE-002`，不能宣称对象存储已完成。
