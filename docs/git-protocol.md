# Git Protocol Implementation Notes

> This document records the key technical details, protocol specifics, and hard-won
> lessons from implementing the Git Smart Protocol (V1 + V2) in ForgeKeep.
> Intended for developers who maintain or extend the `rg-git` crate.

---

## Table of Contents

1. [pkt-line format](#1-pkt-line-format)
2. [Sideband-64k multiplexing](#2-sideband-64k-multiplexing)
3. [git-upload-pack (clone/fetch)](#3-git-upload-pack-clonefetch)
4. [git-receive-pack (push)](#4-git-receive-pack-push)
5. [SSH transport (russh)](#5-ssh-transport-russh)
6. [HTTP transport (Axum)](#6-http-transport-axum)
7. [Pitfalls log](#7-pitfalls-log)
8. [Git Smart Protocol V2](#8-git-smart-protocol-v2)

---

## 1. pkt-line format

### Packet types added in V2

Protocol V2 introduces two new special packets:

| Packet | Hex | Description |
|--------|-----|-------------|
| Flush | `0000` | End of the current phase (same as V1) |
| Delimiter | `0001` | Separates sections of a message (new in V2) |
| Response-End | `0002` | End of a response over a stateless connection (new in V2) |

### V1 pkt-line format

```
<4 hex digits of total length><payload>
```

- The 4-byte length header is hex and **includes its own 4 bytes**.
- So the maximum payload length is `0xffff - 4 = 65531` bytes (in practice usually capped at 65516).
- `0000` is the flush packet, signalling the end of the current "phase".

### Example

```
# "hello\n" (6-byte payload) -> total length 10
000ahello\n

# Flush packet
0000

# Delimiter packet (V2)
0001

# Response-End packet (V2)
0002
```

pkt-line is the basic transport unit of the Git protocol.

### Implementation location

`crates/rg-git/src/pkt_line.rs`

Key functions:
- `read_pkt_line(reader: &mut BufReader<R>)` — read one pkt-line
- `write_pkt_line(writer, pkt)` — write one pkt-line
- `write_flush(writer)` — write `0000`

### ⚠️ Common mistake

**Never use `read_line()` to read a pkt-line.**

`BufReader::read_line()` reads the 4-byte length header (e.g. `004a` in `004ahello\n`) as if it were part of the text content. When it later hits the binary bytes of a packfile it produces a UTF-8 decode error: `stream did not contain valid UTF-8`.

The correct approach:
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

## 2. Sideband-64k multiplexing

### Concept

sideband-64k lets the server send several kinds of data over a single connection:

| Band | Description |
|------|-------------|
| Band 1 (`\x01`) | Main data stream (packfile or report-status) |
| Band 2 (`\x02`) | Progress messages (shown on the client's stderr) |
| Band 3 (`\x03`) | Error messages (fatal error) |

### Format

```
<pkt-line containing a 1-byte band prefix + payload>
```

For example, sending 10 bytes of band-1 data:

```
# total = 4 (header) + 1 (band) + 10 (data) = 15 = 0x0f
000f\x01<10 bytes data>
```

### Flush semantics

A sideband flush (`0000`) signals the end of the entire sideband stream. Once the client
receives it, it stops reading sideband data.

### Implementation location

`crates/rg-git/src/sideband.rs`

Key functions:
- `write_sideband_data(writer, data)` — send band-1 data (auto-chunked)
- `write_sideband_progress(writer, message)` — send band-2 progress
- `write_sideband_error(writer, message)` — send band-3 error
- `write_sideband_flush(writer)` — send the sideband flush `0000`

---

## 3. git-upload-pack (clone/fetch)

### Protocol flow (Smart Protocol V1)

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

In SSH mode the `GET /info/refs` step is skipped; a bidirectional stream is established
directly via `exec_request`.

### Ref advertisement format

```
<pkt-line: "sha1 refname\0caps\n">  <- first line carries capabilities
<pkt-line: "sha2 refs/heads/main\n">
...
<flush: 0000>
```

### Capabilities (what we advertise)

```
side-band-64k ofs-delta agent=forgekeep/0.1
```

> Note: we do **not** advertise `multi_ack` / `multi_ack_detailed` / `no-done`, because
> our negotiation loop only implements the simple NAK -> packfile flow.
>
> Built in `build_ref_advertisement()` in `crates/rg-git/src/protocol/upload_pack.rs`.
> The HTTP `info/refs` path (`build_info_refs()` in `crates/rg-http/src/lib.rs`) advertises a
> wider set — `multi_ack_detailed no-done side-band-64k thin-pack ofs-delta agent=forgekeep/0.1`.

### The two forms of want/have

In practice the macOS git client sends two forms, both of which must be supported:

**Form A** (NUL-separated capabilities):
```
want <sha1>\0side-band-64k ofs-delta\n
```

**Form B** (space-separated, common with macOS git):
```
want <sha1> side-band-64k ofs-delta\n
```

Implementation: first check whether a `\0` is present — if so it is Form A; otherwise check
position 46 (`"want " + 40-char SHA + space`).

### Pack generation

```bash
git -C <repo_path> pack-objects --all --stdout
```

Standard input accepts object SHAs (`--all` means pack every object).

> Note: V1 upload-pack does not prune objects precisely against `wants`/`haves` — it packs
> everything with `--all`. This is simple but bandwidth-unfriendly for large repos; replacing
> it with gix-native pack generation is tracked as a `TODO(gix)` in the code.

---

## 4. git-receive-pack (push)

### Protocol flow

```
Client                          Server
  |                               |
  | <ref advertisement>           |
  | <flush>                       |
  |<------------------------------|
  |                               |
  | old_sha new_sha refname\0caps | <- update commands
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

### Update command format

```
<old_sha> <new_sha> <refname>\0<capabilities>\n   <- first entry carries capabilities
<old_sha> <new_sha> <refname>\n                   <- subsequent entries
<flush: 0000>
<packfile binary data>
```

Special SHAs:
- `old_sha` all zeros: create a new ref
- `new_sha` all zeros: delete a ref (not yet supported in Phase 1)

### Thin pack handling

The client sends a **thin pack** (whose delta bases are not necessarily contained in the pack).
It must be converted to a complete pack with `--fix-thin`:

```bash
git -C <repo_path> index-pack --fix-thin --stdin
```

**Experimental native path (opt-in, default off).** Setting
`FORGEKEEP_NATIVE_INDEX_PACK=1` (`true`/`yes`/`on`) makes receive-pack index the
incoming pack in-process via `gix_pack::Bundle::write_to_directory` instead of
the `git index-pack` subprocess. The repository is passed as the thin-pack
base-object lookup — the native equivalent of `--fix-thin` — and the unpack is
interrupt-driven (a tripped wall-clock/idle watchdog aborts it). It is a PoC:
parity-tested against `git index-pack --fix-thin` on real thin packs, but it does
**not** remove the git dependency (pack generation, verify-commit, archive, etc.
still shell out), so it stays off by default.

### Report-status response format (critical!)

This is the most error-prone part. The correct format, **verified by capturing real
git-receive-pack traffic with `GIT_TRACE_PACKET=1`**:

```
# One sideband band-1 pkt-line whose payload is a sequence of report-status pkt-lines
<pkt-line: \x01 + "000eunpack ok\n" + "0017ok refs/heads/main\n" + "0000">
# sideband flush
0000
```

The wrong approach (**do not do this**):
```
# X send the sideband flush first, then plain pkt-lines
0000                    <- the client thinks the sideband is over
000eunpack ok\n         <- the client never reads this!
...
```

The correct implementation (see `send_response()` in `receive_pack.rs`):

```rust
// 1. Write the report-status sequence into an in-memory buffer
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

// 2. Emit the whole buffer as band-1 sideband data
sideband::write_sideband_data(writer, &report_buf).await?;

// 3. sideband flush terminates the entire sideband stream
sideband::write_sideband_flush(writer).await?;
```

### Capabilities (what we advertise)

```
report-status report-status-v2 side-band-64k agent=forgekeep/0.1
```

---

## 5. SSH transport (russh)

### Session lifecycle

```
russh::server::Server::new_client()  -> create SshHandler
SshHandler::channel_open_session()   -> store the Channel
SshHandler::exec_request()           -> parse the git command, tokio::spawn to handle it
  |-- handle_upload_pack_stream() or handle_receive_pack_stream()
    |-- exit_status_request()        -> send exit code
    |-- stream.shutdown()            -> send SSH EOF
    |-- stream drop                  -> channel close
```

### Ordering of exit_status and stream.shutdown()

**The following order must be followed strictly:**

```rust
// (1) Send exit-status first (the channel is still alive and can accept the request)
handle.exit_status_request(channel_id, exit_code).await?;

// (2) Then shut down the stream (send SSH EOF)
// This guarantees all data written to the stream has been flushed to the client
stream.shutdown().await?;

// (3) stream drop -> channel close (happens automatically)
```

If `exit_status` is not sent before `shutdown()`, the client may block waiting for the exit code.
If `shutdown()` is not called, data buffered inside russh may be lost when the stream is dropped.

### exec_request command format

The git client sends:
```
git-upload-pack '/owner/repo'
git-receive-pack '/owner/repo.git'
```

Handling rules:
1. Split on the first space: service / path
2. Strip leading and trailing quotes from the path (single or double)
3. Strip the leading `/` from the path
4. Look up the full path first, then try appending a `.git` suffix

### ChannelStream behavior notes

russh's `ChannelStream` implements `AsyncRead + AsyncWrite`, but has a few special behaviors:

1. **Every `write_all` call immediately emits an SSH channel data packet** (there is no internal
   buffer), so writing many small fragments produces many small packets. This is normal — russh
   coalesces or reframes them internally.

2. **`flush()` is a no-op** on `ChannelStream` (it has no real effect). Data reliability is
   guaranteed by `shutdown()`.

3. **When wrapping the stream in a `BufReader` to read pkt-lines**, the `BufReader`'s internal
   read-ahead buffer must be dropped before `process_push` returns, otherwise the stream cannot
   be reused to write the response:

```rust
// OK: scope the BufReader's lifetime with a block
let results = {
    let mut reader = BufReader::new(&mut *stream);
    process_push(repo_path, &mut reader).await?
};  // BufReader dropped here
send_response(stream, &results).await?;
```

---

## 6. HTTP transport (Axum)

### Routing structure

```
Router::nest("/git", ...)
├── GET  /{owner}/{repo}/info/refs
├── POST /{owner}/{repo}/git-upload-pack
└── POST /{owner}/{repo}/git-receive-pack

GET /health
```

> **Note**: the full URL is `/git/<owner>/<repo>/...`, not `/<owner>/<repo>/...`.

### Pipe bridging pattern

The HTTP request body (`Bytes`) is synchronous, but `handle_upload_pack_http` and friends expect
an `AsyncRead`. Bridge them with a `tokio::io::duplex` pipe:

```rust
// Write the request body into a pipe
let (pipe_read, mut pipe_write) = tokio::io::duplex(body.len() + 1024);
tokio::spawn(async move {
    let _ = pipe_write.write_all(&body).await;
});

// Write the handler output into another pipe, then read it back as the response body
let (mut buf_reader, mut buf_writer) = tokio::io::duplex(64 * 1024);
handle_upload_pack_http(&repo_path, pipe_read, &mut buf_writer).await?;
buf_writer.flush().await?;
drop(buf_writer);  // <- must drop so that read_to_end can terminate!
let mut output = Vec::new();
buf_reader.read_to_end(&mut output).await?;
```

### info/refs response format

The info/refs response is **not identical** to the SSH ref advertisement — it needs an extra wrapper:

```
# Service header (one pkt-line)
<pkt-line: "# service=git-upload-pack\n">
# Flush
0000
# then the ref advertisement (same as SSH)
<ref advertisement pkt-lines>
0000
```

The `Content-Type` response header must be set correctly, and it depends on the service:
- `git-upload-pack`  -> `application/x-git-upload-pack-advertisement` (for `info/refs`),
  `application/x-git-upload-pack-result` (for the POST result)
- `git-receive-pack` -> `application/x-git-receive-pack-advertisement` /
  `application/x-git-receive-pack-result`

A wrong or missing `Content-Type` makes git clients fall back to the "dumb" protocol or reject
the response outright, so this is easy to get wrong (see pitfall #3 for a related failure mode).

---

## 7. Pitfalls log

The following are pitfalls actually hit during Phase 1 development. **Each one took a while to track down.**

### Pitfall 1: using read_line to read a pkt-line causes a UTF-8 error

**Symptom**: `stream did not contain valid UTF-8`
**Cause**: `BufReader::read_line()` reads the 4-byte pkt-line length header (e.g. `004a`) as text; it crashes outright when it reaches binary packfile data.
**Fix**: use `read_pkt_line()`, which parses the 4-byte length header correctly and returns only the payload.

### Pitfall 2: receive-pack response `bad band #110`

**Symptom**: `error: remote unpack failed: bad band #110`
**Cause**: `#110` is `0x6e`, i.e. the letter `n` — from the first byte of "unpack ok". The server sent `unpack ok` as a plain pkt-line, but in sideband mode the client read `u` (`0x75`) as the band number.
**Fix**: wrap the whole report-status in a sideband band-1 packet.

### Pitfall 3: receive-pack response `bad line length character: unpa`

**Symptom**: after receiving the sideband flush `0000` the client stops reading; the plain pkt-lines sent afterwards are ignored, causing `fatal: the remote end hung up unexpectedly`.
**Cause**: the mistaken belief that the correct format is "send the sideband flush first, then plain pkt-lines" — in reality the client considers the response finished once it receives the sideband flush.
**Fix**: by capturing real git-receive-pack traffic with `GIT_TRACE_PACKET=1`, we found that report-status must be sent as band-1 **before** the sideband flush.

### Pitfall 4: SSH data loss (timing of russh stream.shutdown())

**Symptom**: the server log shows "Receive-pack response sent", but the client only receives the sideband flush `0000` and the trailing data is lost.
**Cause**: the stream is dropped when the task ends, but russh may discard not-yet-sent buffered data when the channel closes.
**Fix**: call `stream.shutdown().await` explicitly before the stream is dropped, so all data is flushed before the SSH EOF is sent.

### Pitfall 5: thin pack makes index-pack fail

**Symptom**: `git index-pack failed: error: pack has X unresolved deltas`
**Cause**: the client sends a thin pack (delta bases may be existing objects not present in the pack); without `--fix-thin` they cannot be resolved.
**Fix**: use `git index-pack --fix-thin --stdin`.

### Pitfall 6: git rev-parse HEAD returns the literal "HEAD" on an empty repo

**Symptom**: in an empty bare repo, `git rev-parse HEAD` does not return an error code but instead outputs the literal string `"HEAD"`.
**Cause**: `HEAD` points at `refs/heads/main`, but the main branch does not exist yet. rev-parse returns a success status code but emits the unresolved symbolic ref.
**Fix**: validate the result as a 40-char hex SHA and otherwise ignore it:

```rust
if sha.len() == 40 && sha.chars().all(|c| c.is_ascii_hexdigit()) {
    Some(sha)
} else {
    None
}
```

### Pitfall 7: argon2 0.5's SaltString::generate needs CryptoRngCore

**Symptom**: `the trait bound ThreadRng: CryptoRngCore is not satisfied`
**Cause**: rand 0.9's `rng()` returns `ThreadRng`, which does not satisfy `password_hash::rand_core::CryptoRngCore`.
**Fix**:

```rust
use password_hash::rand_core::OsRng;
let salt = SaltString::generate(&mut OsRng);
```

### Pitfall 8: Command::arg() does not allow NUL bytes

**Symptom**: `nul byte found in provided data`
**Cause**: the first line of a git update command is `old_sha new_sha refname\0capabilities`; passing the whole line to `Command::arg()` is rejected by the OS (NUL terminates a C string).
**Fix**: split on `\0` and keep only the refname part:

```rust
let clean_line = if line.contains('\0') {
    line.split('\0').next().unwrap_or(line)
} else {
    line
};
```

---

## 8. Git Smart Protocol V2

Protocol V2 is a major improvement over V1, offering a cleaner command architecture and better performance.

### Main differences from V1

| Feature | V1 | V2 |
|---------|----|----|
| Ref advertisement | Everything pushed on the first request | On-demand (ls-refs command) |
| Command reuse | None | Multiple commands over one connection |
| Protocol format | Fixed service name | Parameterized command line |
| Stateless support | Poor | Native |
| Shallow clone | Needs extra negotiation | Built-in |

### V2 protocol flow

```
Client                          Server
  |                               |
  | GET /info/refs?service=       |
  |   Git-Protocol: version=2     |
  |------------------------------>|
  |                               |
  | version 2                     | <- version declaration
  | ls-refs                       | <- supported capabilities
  | fetch=shallow                 |
  | object-format=sha1            |
  | 0000                          |
  |<------------------------------|
  |                               |
  | POST /git-upload-pack         |
  | command=ls-refs               | <- command request
  | 0001                          | <- delimiter
  | ref-prefix refs/heads/        |
  | peel                          |
  | 0000                          |
  |------------------------------>|
  |                               |
  | <ref advertisement>           |
  | 0000                          |
  |<------------------------------|
  |                               |
  | POST /git-upload-pack         |
  | command=fetch                 | <- fetch command
  | want <sha>                    |
  | have <sha>                    |
  | 0001                          |
  | done                          |
  | 0000                          |
  |------------------------------>|
  |                               |
  | ACK <sha>                     | <- acknowledgement
  | <packfile in sideband>        |
  | 0000                          |
  |<------------------------------|
```

### Activating V2 over HTTP

The client activates V2 via an HTTP header:

```http
GET /git/owner/repo/info/refs?service=git-upload-pack HTTP/1.1
Git-Protocol: version=2
```

The server responds with the V2 capability advertisement, then the client sends commands.

### Capability advertisement format

```
<length>version 2\n
<length>agent=forgekeep/0.1\n
<length>ls-refs\n
<length>fetch=shallow\n
<length>object-format=sha1\n
<length>server-option\n
0000
```

> The advertised set is sourced from `ADVERTISED_CAPABILITIES` in
> `crates/rg-git/src/protocol/v2.rs`. Shallow/deepen and partial-clone filters are only
> advertised once they are implemented end to end and covered against real git clients.

### V2 command format

**ls-refs command**:
```
command=ls-refs\n
0001
ref-prefix <prefix>\n
peel\n
symrefs\n
0000
```

**fetch command**:
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

### Implementation location

`crates/rg-git/src/protocol/v2.rs`

Key functions:
- `handle_v2()` — HTTP-mode V2 handler
- `handle_v2_stream()` — SSH-mode V2 handler
- `send_capability_advertisement()` — send the capability declaration
- `read_command_request()` — parse a command request
- `handle_ls_refs()` — handle the ls-refs command
- `handle_fetch()` — handle the fetch command

### Pitfalls log

#### Pitfall 9: V2 must handle Delim/ResponseEnd packets

**Symptom**: `read_pkt_line()` returns new enum variants when it receives `0001` or `0002`.
**Cause**: V2 introduced new packet types.
**Fix**: handle `PktLine::Delim` and `PktLine::ResponseEnd` at every pkt-line read site.

---

## References

- [Git Protocol V2 Reference](https://git-scm.com/docs/protocol-v2)
- [Git Pack Protocol Reference](https://git-scm.com/docs/pack-protocol)
- [Git HTTP Backend](https://git-scm.com/docs/git-http-backend)
- [Git Smart HTTP Transfer Protocols](https://git-scm.com/docs/http-protocol)
- [russh documentation](https://docs.rs/russh/)
- [gitoxide (gix)](https://github.com/Byron/gitoxide) — candidate library for replacing shell-out git commands
