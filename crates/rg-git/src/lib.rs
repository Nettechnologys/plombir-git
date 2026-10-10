//! `rg-git` — Git Smart Protocol implementation for Plombir Git.
//!
//! This crate handles the server side of Git's smart HTTP and SSH protocols.
//! It speaks pkt-line framing, sideband multiplexing, and implements both
//! Protocol V1 (receive-pack / upload-pack) and Protocol V2 (ls-refs, fetch,
//! object-info).
//!
//! # Architecture
//!
//! - **[`pkt_line`]** — low-level Git pkt-line framing (read/write flush, delim,
//!   data packets).  All Git protocol messages are framed in 4-hex + body
//!   format with `0000` as flush.
//! - **[`sideband`]** — Git sideband (band 1 data / band 2 progress / band 3
//!   error) for multiplexing pack data and status messages.
//! - **[`protocol`]** — server-side protocol handlers:
//!   - `receive_pack` — handles `git push` (receive pack + thin pack indexing)
//!   - `upload_pack` — handles `git fetch/clone` (ref advertisement + pack
//!     generation)
//!   - `v2` — Protocol V2 handlers (ls-refs, fetch command, object-info)
//!
//! # Key design decisions
//!
//! - **Bare repositories only.**  Plombir Git repos live as `{owner}/{repo}.git`
//!   bare repos on disk.
//! - **Hybrid gix + git CLI.**  Uses `gix` 0.83 for ref traversal, object
//!   reading, and reference updates.  Falls back to `git` CLI for pack
//!   generation (`git pack-objects`), thin-pack indexing
//!   (`git index-pack --fix-thin`), and Protocol V2 packfile streaming.
//!   See Project Memory for migration status.
//! - **The outgoing pack is streamed, never collected.**  `git pack-objects`
//!   produces the clone/fetch pack incrementally, and
//!   `protocol::pack_stream` is the single place that reads it — one chunk at
//!   a time, so the memory a clone costs is a working window rather than the
//!   size of the repository.
//! - **Thin pack handling.**  Clients may send thin (delta-only) packs; they
//!   MUST be completed with `--fix-thin` before the ref is updated.
//!
//! # Usage
//!
//! ```rust,ignore
//! use rg_git::protocol;
//!
//! // Resolve refs for upload-pack advertisement
//! let refs = protocol::upload_pack::list_refs(repo_path)?;
//!
//! // Handle a receive-pack push
//! let updates = protocol::receive_pack::process_pack(repo_path, pack_data)?;
//! ```
//!
//! # Caveats
//!
//! - **Protocol V2** is complete in HTTP mode but has gaps in SSH mode
//!   (`handle_v2_stream_impl` is a stub).
//! - Pack generation and thin-pack indexing use `git` CLI — not pure Rust.
//!   These are the remaining migration items tracked in Project Memory.

pub mod cli_gateway;
pub mod credentials;
pub mod invocation;
pub mod io_timeout;
pub mod maintenance;
pub mod pkt_line;
pub mod protocol;
pub mod ref_advertisement;
pub mod refname;
pub mod repository;
pub mod sideband;
pub mod signatures;

#[cfg(test)]
pub(crate) mod test_support; // Fixtures shared by unit tests of several protocol modules
