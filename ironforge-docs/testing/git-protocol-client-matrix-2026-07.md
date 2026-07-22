# Git 真实客户端协议兼容矩阵

> 建立日期：2026-07-14
> 对齐任务：`GIT-003`、`GIT-004`、`GIT-002`
> 自动化入口：`scripts/git-protocol-e2e.sh`
> CI 入口：`.github/workflows/regression.yml` 的 `git-protocol` job

## 1. 验收范围

矩阵启动一个临时 IronForge 实例，使用真实 `git`、`ssh` 和 `curl` 完成用户注册、私有仓库创建、SSH 公钥注册及 Git 数据往返。每次运行使用临时 SQLite 数据库、仓库存储和动态 HTTP/SSH 端口，结束后自动清理。

| Transport | 协议 | clone | fetch | push | 协商证据 |
|---|---|---:|---:|---:|---|
| HTTP | V1 | ✅ | ✅ | ✅ | `protocol.version=1`，packet trace 不含 `version 2` |
| HTTP | V2 | ✅ | ✅ | 复用 V1 receive-pack | `protocol.version=2`，packet trace 必须含 `version 2` |
| SSH | V1 | ✅ | ✅ | ✅ | `protocol.version=1`，packet trace 不含 `version 2` |
| SSH | V2 | ✅ | ✅ | 复用 V1 receive-pack | `protocol.version=2`，packet trace 必须含 `version 2` |

Git receive-pack 没有独立的 Protocol V2 命令，因此 V2 行的 push 不重复标成 V2 能力；HTTP/SSH 的 push 路径由 V1 receive-pack 场景覆盖。

## 2. Shallow 与 partial clone 正向语义

| 场景 | 当前预期 | 自动化断言 |
|---|---|---|
| HTTP V2 `clone --depth=1`、`fetch --deepen=2`、`fetch --unshallow` | 深度依次为 1、3、完整历史，shallow 文件正确创建和移除 | 提交数、`.git/shallow` 和 V2 trace 必须一致 |
| SSH V2 `clone --depth=2` | 仅取得两层提交并记录 shallow boundary | 提交数为 2，`.git/shallow` 非空，V2 trace 成功 |
| HTTP V2 `clone --shallow-exclude=<tag>` | 排除指定 tag 可达历史并在截断提交处建立边界 | 提交数比完整线性历史少 1，`.git/shallow` 非空 |
| HTTP V2 `clone --shallow-since=<time>` | 只取得指定提交时间之后的历史并建立边界 | 固定日期数据集只返回预期的 2 个提交 |
| HTTP V2 `clone --filter=blob:none --no-checkout` | 初始 pack 省略 blob，checkout 时按需取回当前工作树对象 | promisor 配置存在；禁用 lazy fetch 时有缺失对象；checkout trace 请求目标 blob |
| SSH V2 `clone --filter=tree:0 --no-checkout` | 初始 pack 省略 tree/blob，checkout 时按需取回当前工作树对象 | trace 含 `filter tree:0`；checkout trace 发出按需 `want`，根 tree 与工作树文件可用 |

shallow/deepen 的正向回归由 `GIT-004` 提供；partial-clone filter 的正向回归由 `GIT-002` 提供。当前矩阵同时验证传输协商、promisor 状态和按需对象取回。

## 3. 本地运行

先构建服务端，再运行矩阵：

```bash
cargo build --release -p rg-cli
scripts/git-protocol-e2e.sh
```

调试失败时可保留临时目录和服务端日志：

```bash
IRONFORGE_E2E_KEEP_TMP=1 scripts/git-protocol-e2e.sh
```

也可通过 `IRONFORGE_BIN=/absolute/path/to/ironforge` 指定其他构建产物。

## 4. 2026-07-14 首次运行结论

- GIT-003 建立时的 10 个场景全部通过；GIT-004 将矩阵扩展为 12 个场景，GIT-002 将最后一项升级为 HTTP/SSH filter 正向验收；
- 首次运行发现 HTTP V2 增量 fetch 的 acknowledgments section 顺序错误；
- 修复后，携带 `done` 的请求直接进入 packfile section，协商中的 `ready` 位于 section delimiter 之前；
- 对应 7 个 `rg-git` Protocol V2 单元测试和 1 个 `rg-http` capability 测试通过，真实 HTTP/SSH V2 clone/fetch/shallow/filter 均通过。
