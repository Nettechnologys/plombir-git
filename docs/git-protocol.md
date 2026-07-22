# Git 协议实现细节

> 本文档记录 IronForge 实现 Git Smart Protocol V1 过程中的关键技术细节、协议规范和踩坑经验。
> 适合需要维护或扩展 `rg-git` crate 的开发者阅读。

---

## 目录

1. [pkt-line 协议格式](#1-pkt-line-协议格式)
2. [Sideband-64k 多路复用](#2-sideband-64k-多路复用)
3. [git-upload-pack（clone/fetch）](#3-git-upload-packclonefetch)
4. [git-receive-pack（push）](#4-git-receive-packpush)
5. [SSH 传输层（russh）](#5-ssh-传输层russh)
6. [HTTP 传输层（Axum）](#6-http-传输层axum)
7. [踩坑记录](#7-踩坑记录)

---

## 1. pkt-line 协议格式

### V2 新增的 Packet 类型

Protocol V2 引入了两种新的特殊 packet：

| Packet | 十六进制 | 说明 |
|--------|----------|------|
| Flush | `0000` | 结束当前阶段（与 V1 相同） |
| Delimiter | `0001` | 分隔消息的不同部分（V2 新增） |
| Response-End | `0002` | 无状态连接中响应的结束（V2 新增） |

### V1 pkt-line 格式

```
<4 hex digits of total length><payload>
```

- 4 字节长度头用十六进制表示，**包含自身 4 字节**
- 所以 payload 最大长度为 `0xffff - 4 = 65531` 字节
- `0000` 是 flush packet

### 示例

```
# "hello\n"（6字节 payload）→ 总长 10
000ahello\n


# Flush packet
0000

# Delimiter packet (V2)
0001

# Response-End packet (V2)
0002
```

pkt-line 是 Git 协议的基础传输单元。

### 格式

```
<4 hex digits of total length><payload>
```

- 4 字节长度头用十六进制表示，**包含自身 4 字节**
- 所以 payload 最大长度为 `0xffff - 4 = 65531` 字节（实践中通常限制到 65516）
- `0000` 是 flush packet，表示当前"阶段"结束

### 示例

```
# "hello\n"（6字节 payload）→ 总长 10
000ahello\n

# Flush packet
0000
```

### 实现位置

`crates/rg-git/src/pkt_line.rs`

关键函数：
- `read_pkt_line(reader: &mut BufReader<R>)` — 读一个 pkt-line
- `write_pkt_line(writer, pkt)` — 写一个 pkt-line
- `write_flush(writer)` — 写 `0000`

### ⚠️ 常见错误

**绝对不能用 `read_line()` 读 pkt-line。**

`BufReader::read_line()` 会把 `004ahello\n` 中的 `004a` 当成文本内容一起读出来。当遇到 packfile 的二进制数据时，会产生 UTF-8 解析错误：`stream did not contain valid UTF-8`。

正确做法：
```rust
let mut reader = BufReader::new(stream);
loop {
    match read_pkt_line(&mut reader).await? {
        PktLine::Flush => break,
        PktLine::Data(bytes) => { /* ... */ }
    }
}
```

---

## 2. Sideband-64k 多路复用

### 概念

sideband-64k 允许服务端在同一个连接上发送多种类型的数据：

| Band | 说明 |
|------|------|
| Band 1 (`\x01`) | 主数据流（packfile 或 report-status） |
| Band 2 (`\x02`) | 进度消息（显示在客户端 stderr） |
| Band 3 (`\x03`) | 错误消息（fatal error） |

### 格式

```
<pkt-line 包含 1字节 band prefix + payload>
```

例如，发送 10 字节的 band-1 数据：

```
# total = 4(header) + 1(band) + 10(data) = 15 = 0x0f
000f\x01<10 bytes data>
```

### Flush 语义

sideband flush (`0000`) 表示整个 sideband 流结束。客户端收到后停止读取 sideband 数据。

### 实现位置

`crates/rg-git/src/sideband.rs`

关键函数：
- `write_sideband_data(writer, data)` — 发 band-1 数据（自动分块）
- `write_sideband_progress(writer, message)` — 发 band-2 进度
- `write_sideband_error(writer, message)` — 发 band-3 错误
- `write_sideband_flush(writer)` — 发 sideband flush `0000`

---

## 3. git-upload-pack（clone/fetch）

### 协议流程（Smart Protocol V1）

```
Client                          Server
  |                               |
  |  GET /info/refs?service=      |
  |  git-upload-pack              |
  |------------------------------>|
  |                               |
  |  <service header pkt-line>    |
  |  <ref advertisement>          |
  |  <flush>                      |
  |<------------------------------|
  |                               |
  | POST /git-upload-pack         |
  | want <sha1>\0<capabilities>   |
  | want <sha2>                   |
  | <flush>                       |
  | done                          |
  |------------------------------>|
  |                               |
  | NAK                           |
  | <packfile in sideband-64k>    |
  | <sideband flush>              |
  |<------------------------------|
```

SSH 模式省略 `GET /info/refs` 步骤，直接通过 exec_request 建立双向 stream。

### 引用广告格式

```
<pkt-line: "sha1 refname\0caps\n">  ← 第一行附带 capabilities
<pkt-line: "sha2 refs/heads/main\n">
...
<flush: 0000>
```

### Capabilities（我们广告的）

```
side-band-64k ofs-delta agent=ironforge/0.1
```

> 注意：我们**不广告** `multi_ack` / `multi_ack_detailed` / `no-done`，因为目前只实现了简单的 NAK → packfile 流程。

### want/have 格式的两种形式

实践中 macOS git 客户端发送两种形式，都必须支持：

**Form A**（NUL 分隔 capabilities）：
```
want <sha1>\0side-band-64k ofs-delta\n
```

**Form B**（空格分隔，macOS git 常见）：
```
want <sha1> side-band-64k ofs-delta\n
```

实现：先检查是否含 `\0`，有则 Form A；否则检查第 46 位（`"want " + 40字符SHA + 空格`）。

### Pack 生成

```bash
git -C <repo_path> pack-objects --all --stdout
```

标准输入接受 object SHA（`--all` 表示打包所有对象）。

---

## 4. git-receive-pack（push）

### 协议流程

```
Client                          Server
  |                               |
  | <ref advertisement>           |
  | <flush>                       |
  |<------------------------------|
  |                               |
  | old_sha new_sha refname\0caps | ← update commands
  | old_sha new_sha refname       |
  | <flush>                       |
  | <packfile (raw bytes)>        |
  |------------------------------>|
  |                               |
  | <sideband band-1:             |
  |   report-status pkt-lines>    |
  | <sideband flush>              |
  |<------------------------------|
```

### Update Command 格式

```
<old_sha> <new_sha> <refname>\0<capabilities>\n   ← 第一条包含 capabilities
<old_sha> <new_sha> <refname>\n                   ← 后续条目
<flush: 0000>
<packfile binary data>
```

特殊 SHA：
- `old_sha` 全为 0：创建新 ref
- `new_sha` 全为 0：删除 ref（IronForge Phase 1 暂不支持）

### Thin Pack 处理

客户端发送的是 **thin pack**（只包含 delta base 不一定在 pack 里的对象）。
必须用 `--fix-thin` 转换为完整 pack：

```bash
git -C <repo_path> index-pack --fix-thin --stdin
```

### Report-Status 响应格式（关键！）

这是最容易出错的地方。**经过 `GIT_TRACE_PACKET=1` 对真实 git-receive-pack 抓包验证**的正确格式：

```
# 一个 sideband band-1 pkt-line，payload 是 report-status pkt-lines 序列
<pkt-line: \x01 + "000eunpack ok\n" + "0017ok refs/heads/main\n" + "0000">
# sideband flush
0000
```

错误的做法（**不要这样做**）：
```
# ❌ 先发 sideband flush，再发 plain pkt-lines
0000                    ← 客户端以为 sideband 结束了
000eunpack ok\n         ← 客户端不会读这里！
...
```

正确的实现（见 `receive_pack.rs` 的 `send_response()`）：

```rust
// 1. 把 report-status 序列写入内存 buf
let mut report_buf: Vec<u8> = Vec::new();
write_pkt_line(&mut report_buf, &PktLine::text("unpack ok")).await?;
for result in results {
    if result.status == "ok" {
        write_pkt_line(&mut report_buf, &PktLine::text(&format!("ok {}", result.refname))).await?;
    } else {
        write_pkt_line(&mut report_buf, &PktLine::text(&format!("ng {} {}", result.refname, result.message))).await?;
    }
}
write_flush(&mut report_buf).await?;

// 2. 整体作为 band-1 sideband 发出
sideband::write_sideband_data(writer, &report_buf).await?;

// 3. sideband flush 结束整个 sideband 流
sideband::write_sideband_flush(writer).await?;
```

### Capabilities（我们广告的）

```
report-status report-status-v2 side-band-64k agent=ironforge/0.1
```

---

## 5. SSH 传输层（russh）

### 会话生命周期

```
russh::server::Server::new_client()  → 创建 SshHandler
SshHandler::channel_open_session()   → 存储 Channel
SshHandler::exec_request()           → 解析 git 命令，tokio::spawn 处理
  └── handle_upload_pack_stream() 或 handle_receive_pack_stream()
    └── exit_status_request()        → 发退出码
    └── stream.shutdown()            → 发 SSH EOF
    └── stream drop                  → channel close
```

### exit_status 和 stream.shutdown() 的顺序

**必须严格按以下顺序执行**：

```rust
// ① 先发 exit-status（此时 channel 还活着，可以接受请求）
handle.exit_status_request(channel_id, exit_code).await?;

// ② 再 shutdown stream（发 SSH EOF）
// 这确保所有写入 stream 的数据都已发送到客户端
stream.shutdown().await?;

// ③ stream drop → channel close（自动发生）
```

如果在 `shutdown()` 之前不发 `exit_status`，客户端可能在等待 exit code 时被阻塞。
如果不调用 `shutdown()`，russh 内部缓冲区中的数据可能在 stream drop 时丢失。

### exec_request 的命令格式

git 客户端发送：
```
git-upload-pack '/owner/repo'
git-receive-pack '/owner/repo.git'
```

处理规则：
1. 按第一个空格分割：service / path
2. 去除 path 的首尾引号（单引号或双引号）
3. 去除 path 开头的 `/`
4. 先查完整路径，再尝试加 `.git` 后缀

### ChannelStream 行为注意事项

russh 的 `ChannelStream` 实现了 `AsyncRead + AsyncWrite`，但有几个特殊行为：

1. **每次 `write_all` 调用立即发出 SSH channel data packet**（没有内部 buffer），所以小碎片写入会产生很多小 packet。这是正常的，russh 内部会合并或分帧。

2. **`flush()` 是 no-op**（在 `ChannelStream` 上没有实际效果）。数据可靠性由 `shutdown()` 保证。

3. **在 `BufReader` 中包装 stream 读 pkt-line 时**，BufReader 内部预读缓冲必须在 `process_push` 返回前 drop，否则 stream 无法继续用于写响应：

```rust
// ✅ 正确：用块限制 BufReader 生命周期
let results = {
    let mut reader = BufReader::new(&mut *stream);
    process_push(repo_path, &mut reader).await?
};  // BufReader drop 在这里
send_response(stream, &results).await?;
```

---

## 6. HTTP 传输层（Axum）

### 路由结构

```
Router::nest("/git", ...)
├── GET  /{owner}/{repo}/info/refs
├── POST /{owner}/{repo}/git-upload-pack
└── POST /{owner}/{repo}/git-receive-pack

GET /health
```

> **注意**：完整 URL 是 `/git/<owner>/<repo>/...`，不是 `/<owner>/<repo>/...`

### Pipe 桥接模式

HTTP 请求体（`Bytes`）是同步的，但 `handle_upload_pack_http` 等函数期望 `AsyncRead`。
用 `tokio::io::duplex` 管道桥接：

```rust
// 把 request body 写入 pipe
let (pipe_read, mut pipe_write) = tokio::io::duplex(body.len() + 1024);
tokio::spawn(async move {
    let _ = pipe_write.write_all(&body).await;
});

// 把 handler 输出写入另一个 pipe，再读回来作为 response body
let (mut buf_reader, mut buf_writer) = tokio::io::duplex(64 * 1024);
handle_upload_pack_http(&repo_path, pipe_read, &mut buf_writer).await?;
buf_writer.flush().await?;
drop(buf_writer);  // ← 必须 drop 才能让 read_to_end 结束！
let mut output = Vec::new();
buf_reader.read_to_end(&mut output).await?;
```

### info/refs 响应格式

info/refs 的响应**不完全等同于** SSH 的 ref advertisement，需要额外包一层：

```
# Service header（一个 pkt-line）
<pkt-line: "# service=git-upload-pack\n">
# Flush
0000
# 然后是 ref advertisement（同 SSH）
<ref advertisement pkt-lines>
0000
```

Content-Type 响应头必须正确设置（见 CLAUDE.md 踩坑 #6）。

---

## 7. 踩坑记录

以下是 Phase 1 开发过程中实际踩过的坑，**每个都花费了较长时间排查**。

### 坑 1：用 read_line 读 pkt-line 导致 UTF-8 错误

**现象**：`stream did not contain valid UTF-8`  
**原因**：`BufReader::read_line()` 把 pkt-line 的 4 字节长度头（如 `004a`）当成文本内容读了进来，碰到 packfile 二进制数据更是直接崩溃。  
**修复**：改用 `read_pkt_line()`，正确解析 4 字节长度头后只返回 payload。

### 坑 2：receive-pack 响应 `bad band #110`

**现象**：`error: remote unpack failed: bad band #110`  
**原因**：`#110` 是 `0x6e` 即字母 `n`，来自 "unpack ok" 的第一个字节。服务端把 `unpack ok` 作为 plain pkt-line 发送，但客户端在 sideband 模式下把 `u` (`0x75`) 当成 band number 读取。  
**修复**：把 report-status 整体用 sideband band-1 包装发送。

### 坑 3：receive-pack 响应 `bad line length character: unpa`

**现象**：客户端收到 sideband flush `0000` 后停止读取，后续发送的 plain pkt-lines 被客户端忽略，导致 `fatal: the remote end hung up unexpectedly`。  
**原因**：误以为正确格式是"先发 sideband flush，再发 plain pkt-lines"——实际上客户端在收到 sideband flush 后就认为响应结束了。  
**修复**：通过 `GIT_TRACE_PACKET=1` 对真实 git-receive-pack 抓包，发现 report-status 必须整体在 sideband flush **之前**以 band-1 发送。

### 坑 4：SSH 数据丢失（russh stream.shutdown() 时机）

**现象**：服务端日志显示 "Receive-pack response sent"，但客户端只收到 sideband flush `0000`，后续数据丢失。  
**原因**：stream 在 task 结束时被 drop，但 russh 可能在 channel close 时丢弃尚未发出的缓冲数据。  
**修复**：在 stream drop 之前显式调用 `stream.shutdown().await`，确保所有数据都被发出后再发 SSH EOF。

### 坑 5：thin pack 导致 index-pack 失败

**现象**：`git index-pack failed: error: pack has X unresolved deltas`  
**原因**：客户端发送 thin pack（delta base 可能是已有对象，不在 pack 中），不加 `--fix-thin` 无法解析。  
**修复**：使用 `git index-pack --fix-thin --stdin`。

### 坑 6：空 repo 的 git rev-parse HEAD 返回字面 "HEAD"

**现象**：在空的 bare repo 中 `git rev-parse HEAD` 不返回错误码，而是输出字面字符串 `"HEAD"`。  
**原因**：`HEAD` 指向 `refs/heads/main`，但 main 分支还不存在。rev-parse 返回成功状态码但输出未解析的符号引用。  
**修复**：解析结果后需校验是否为 40 位 hex SHA，否则忽略：

```rust
if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
    Some(sha)
} else {
    None
}
```

### 坑 7：argon2 0.5 的 SaltString::generate 需要 CryptoRngCore

**现象**：`the trait bound ThreadRng: CryptoRngCore is not satisfied`  
**原因**：rand 0.9 的 `rng()` 返回 `ThreadRng`，不满足 `password_hash::rand_core::CryptoRngCore`。  
**修复**：

```rust
use password_hash::rand_core::OsRng;
let salt = SaltString::generate(&mut OsRng);
```

### 坑 8：Command::arg() 不允许 NUL 字节

**现象**：`nul byte found in provided data`  
**原因**：git update command 第一行格式是 `old_sha new_sha refname\0capabilities`，把整行传给 `Command::arg()` 会被 OS 拒绝（NUL 在 C 字符串中表示结束）。  
**修复**：在 `\0` 处分割，只取 refname 部分：

```rust
let clean_line = if line.contains('\0') {
    line.split('\0').next().unwrap_or(line)
} else {
    line
};
```

---

## 8. Git Smart Protocol V2

Protocol V2 是对 V1 的重大改进，提供了更清晰的命令架构和更好的性能。

### 与 V1 的主要区别

| 特性 | V1 | V2 |
|------|----|----|
| Ref 广告方式 | 首次请求全部推送 | 按需请求（ls-refs 命令） |
| 命令复用 | 无 | 支持多命令复用同一连接 |
| 协议格式 | 固定服务名 | 命令行参数化 |
| 无状态支持 | 差 | 原生支持 |
| 浅克隆 | 需额外协商 | 内置支持 |

### V2 协议流程

```
Client                          Server
  |                               |
  | GET /info/refs?service=       |
  |   Git-Protocol: version=2      |
  |------------------------------>|
  |                               |
  | version 2                     | ← 版本声明
  | ls-refs                       | ← 支持的能力
  | fetch=shallow                 |
  | object-format=sha1            |
  | 0000                          |
  |<------------------------------|
  |                               |
  | POST /git-upload-pack         |
  | command=ls-refs               | ← 命令请求
  | 0001                          | ← 分隔符
  | ref-prefix refs/heads/         |
  | peel                           |
  | 0000                           |
  |------------------------------>|
  |                               |
  | <ref advertisement>           |
  | 0000                           |
  |<------------------------------|
  |                               |
  | POST /git-upload-pack         |
  | command=fetch                  | ← fetch 命令
  | want <sha>                     |
  | have <sha>                    |
  | 0001                          |
  | done                          |
  | 0000                           |
  |------------------------------>|
  |                               |
  | ACK <sha>                      | ← 确认
  | <packfile in sideband>        |
  | 0000                           |
  |<------------------------------|
```

### HTTP 模式下的 V2 激活

客户端通过 HTTP 头激活 V2：

```http
GET /git/owner/repo/info/refs?service=git-upload-pack HTTP/1.1
Git-Protocol: version=2
```

服务端响应 V2 capability advertisement，然后客户端发送命令。

### Capability Advertisement 格式

```
<length>version 2\n
<length>agent=ironforge/0.1\n
<length>ls-refs\n
<length>fetch=shallow\n
<length>object-format=sha1\n
<length>server-option\n
0000
```

### V2 命令格式

**ls-refs 命令**：
```
command=ls-refs\n
0001
ref-prefix <prefix>\n
peel\n
symrefs\n
0000
```

**fetch 命令**：
```
command=fetch\n
want <sha>\n
have <sha>\n
0001
filter blob:none\n
deepen 10\n
done\n
0000
```

### 实现位置

`crates/rg-git/src/protocol/v2.rs`

关键函数：
- `handle_v2()` — HTTP 模式 V2 处理器
- `handle_v2_stream()` — SSH 模式 V2 处理器
- `send_capability_advertisement()` — 发送能力声明
- `read_command_request()` — 解析命令请求
- `handle_ls_refs()` — 处理 ls-refs 命令
- `handle_fetch()` — 处理 fetch 命令

### 踩坑记录

#### 坑 9：V2 需要处理 Delim/ResponseEnd packet

**现象**：`read_pkt_line()` 在收到 `0001` 或 `0002` 时返回新的枚举变体。  
**原因**：V2 引入了新的 packet 类型。  
**修复**：在所有 pkt-line 读取处处理 `PktLine::Delim` 和 `PktLine::ResponseEnd`。

---

## 参考资料

- [Git Protocol V2 Reference](https://git-scm.com/docs/protocol-v2)
- [Git Pack Protocol Reference](https://git-scm.com/docs/pack-protocol)
- [Git HTTP Backend](https://git-scm.com/docs/git-http-backend)
- [Git Smart HTTP Transfer Protocols](https://git-scm.com/docs/http-protocol)
- [russh 文档](https://docs.rs/russh/)
- [gitoxide (gix)](https://github.com/Byron/gitoxide) — 未来替换系统 git 命令的候选库
