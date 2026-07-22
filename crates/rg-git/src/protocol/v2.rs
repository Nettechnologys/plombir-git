//! Git Smart Protocol V2 implementation.
//!
//! Protocol V2 improves upon V1 with:
//! - Stateless-friendly design
//! - On-demand ref fetching (ls-refs command)
//! - Clearer command/capability negotiation
//!
//! Shallow/deepen and partial-clone filters are advertised after end-to-end
//! implementation and real Git client coverage.
//!
//! Reference: <https://git-scm.com/docs/protocol-v2>

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use anyhow::{bail, Context, Result};
use tokio::io::{split, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

use crate::pkt_line::{read_pkt_line, write_flush, write_pkt_line, PktLine};
use crate::sideband;

/// V2 Protocol constants
pub const PROTOCOL_VERSION: &str = "2";

/// Capabilities that IronForge currently implements end to end.
///
/// Keep HTTP and SSH advertisements sourced from this list. Unsupported fetch
/// features must not be appended here until `handle_fetch` implements them.
pub const ADVERTISED_CAPABILITIES: &[&str] = &[
    "agent=ironforge/0.1",
    caps::LS_REFS,
    caps::FETCH_SHALLOW,
    "object-format=sha1",
    caps::SERVER_OPTION,
];

/// V2 Capability names
pub mod caps {
    /// Agent capability - identifies server version
    pub const AGENT: &str = "agent";
    /// Object format (sha1 for now)
    pub const OBJECT_FORMAT: &str = "object-format";
    /// List refs command
    pub const LS_REFS: &str = "ls-refs";
    /// Fetch command
    pub const FETCH: &str = "fetch";
    /// Fetch command with shallow/deepen and partial-clone filter support
    pub const FETCH_SHALLOW: &str = "fetch=shallow filter";
    /// Server option capability
    pub const SERVER_OPTION: &str = "server-option";
    /// Session identifier
    pub const SESSION_ID: &str = "session-id";
    /// Object info command
    pub const OBJECT_INFO: &str = "object-info";
}

/// Sideband channel constants (inherited from V1)
pub mod sideband_channel {
    pub const DATA: u8 = 1;
    pub const PROGRESS: u8 = 2;
    pub const ERROR: u8 = 3;
}

/// Handle Protocol V2 for a single bidirectional stream (SSH mode).
pub async fn handle_v2_stream<S>(repo_path: &Path, stream: &mut S) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // We need to use two separate mutable references, so we use RefCell
    // or we can just use the same impl but duplicated for stream mode
    handle_v2_stream_impl(repo_path, stream).await
}

/// Handle Protocol V2 with separate reader/writer (HTTP mode).
/// Sends capability advertisement first, then processes commands.
/// Use this for SSH mode where the full V2 flow starts from scratch.
pub async fn handle_v2<R, W>(repo_path: &Path, reader: R, writer: W) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    handle_v2_impl(repo_path, reader, writer).await
}

/// Handle Protocol V2 HTTP POST request (command-only, no capability advertisement).
///
/// In Smart HTTP mode, the capability advertisement was already sent in the
/// GET /info/refs response. The POST request only contains the command
/// (ls-refs or fetch), so we skip sending the advertisement and directly
/// process the command.
pub async fn handle_v2_http<R, W>(repo_path: &Path, reader: R, writer: W) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut writer = writer;

    // No capability advertisement — it was sent in the info/refs GET response.
    // Directly enter command processing loop.
    loop {
        match read_command_request(&mut reader).await? {
            CommandRequest::LsRefs {
                ref_patterns,
                peel,
                symrefs,
                unborn,
                server_options,
            } => {
                tracing::debug!(
                    patterns = ?ref_patterns,
                    peel,
                    symrefs,
                    "Processing ls-refs command (HTTP V2)"
                );
                handle_ls_refs(
                    repo_path,
                    &mut writer,
                    &ref_patterns,
                    peel,
                    symrefs,
                    unborn,
                    &server_options,
                )
                .await?;
            }
            CommandRequest::Fetch {
                wants,
                haves,
                shallow,
                filter,
                done,
                client_caps,
            } => {
                tracing::debug!(
                    wants = wants.len(),
                    haves = haves.len(),
                    shallows = shallow.shallows.len(),
                    done,
                    "Processing fetch command (HTTP V2)"
                );
                handle_fetch(
                    repo_path,
                    &mut writer,
                    &wants,
                    &haves,
                    &shallow,
                    &filter,
                    done,
                    &client_caps,
                )
                .await?;
            }
            CommandRequest::ObjectInfo {
                oid,
                server_options,
            } => {
                tracing::debug!(oid = %oid, "Processing object-info command (HTTP V2)");
                handle_object_info(repo_path, &mut writer, &oid, &server_options).await?;
            }
            CommandRequest::Flush => {
                tracing::debug!("Received command flush - closing connection (HTTP V2)");
                break;
            }
            CommandRequest::Unknown(cmd) => {
                tracing::warn!(cmd = %cmd, "Unknown command, skipping");
                skip_until_flush(&mut reader).await?;
                write_flush(&mut writer).await?;
            }
        }
    }

    Ok(())
}

/// Internal: Protocol V2 for single bidirectional stream (SSH mode).
///
/// Uses tokio::io::split to separate the stream into read/write halves,
/// so we can use BufReader on the read half for efficient pkt-line parsing
/// while keeping the write half independent.
async fn handle_v2_stream_impl<S>(repo_path: &Path, stream: &mut S) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // Split the bidirectional stream into independent read/write halves.
    let (read_half, mut write_half) = split(stream);

    // Send capability advertisement on the write half
    send_capability_advertisement(&mut write_half).await?;

    // BufReader on the read half for efficient pkt-line parsing.
    // We reuse the same BufReader across loop iterations to preserve its buffer.
    let mut reader = BufReader::new(read_half);

    // Command processing loop - V2 allows command multiplexing.
    // We read the command first (storing the result), then match on it,
    // so that the mutable borrow of `reader` ends before the match arms execute.
    loop {
        let command = read_command_request(&mut reader).await?;

        match command {
            CommandRequest::LsRefs {
                ref_patterns,
                peel,
                symrefs,
                unborn,
                server_options,
            } => {
                tracing::debug!(
                    patterns = ?ref_patterns,
                    peel,
                    symrefs,
                    "Processing ls-refs command (SSH V2)"
                );
                handle_ls_refs(
                    repo_path,
                    &mut write_half,
                    &ref_patterns,
                    peel,
                    symrefs,
                    unborn,
                    &server_options,
                )
                .await?;
            }
            CommandRequest::Fetch {
                wants,
                haves,
                shallow,
                filter,
                done,
                client_caps,
            } => {
                tracing::debug!(
                    wants = wants.len(),
                    haves = haves.len(),
                    shallows = shallow.shallows.len(),
                    done,
                    "Processing fetch command (SSH V2)"
                );
                handle_fetch(
                    repo_path,
                    &mut write_half,
                    &wants,
                    &haves,
                    &shallow,
                    &filter,
                    done,
                    &client_caps,
                )
                .await?;
            }
            CommandRequest::ObjectInfo {
                oid,
                server_options,
            } => {
                tracing::debug!(oid = %oid, "Processing object-info command (SSH V2)");
                handle_object_info(repo_path, &mut write_half, &oid, &server_options).await?;
            }
            CommandRequest::Flush => {
                // Empty flush packet signals end of commands
                tracing::debug!("Received command flush - closing connection (SSH V2)");
                break;
            }
            CommandRequest::Unknown(cmd) => {
                tracing::warn!(cmd = %cmd, "Unknown command, skipping");
                // Reuse the existing `reader` (BufReader) to skip until flush.
                // The borrow of `reader` for `read_command_request` ended
                // when that function returned, so `reader` is available here.
                skip_until_flush(&mut reader).await?;
                write_flush(&mut write_half).await?;
            }
        }
    }

    Ok(())
}

/// Internal V2 implementation.
async fn handle_v2_impl<R, W>(repo_path: &Path, reader: R, writer: W) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut writer = writer;

    // Send capability advertisement
    send_capability_advertisement(&mut writer).await?;

    // Command processing loop - V2 allows command multiplexing
    loop {
        // Read command request
        match read_command_request(&mut reader).await? {
            CommandRequest::LsRefs {
                ref_patterns,
                peel,
                symrefs,
                unborn,
                server_options,
            } => {
                tracing::debug!(
                    patterns = ?ref_patterns,
                    peel,
                    symrefs,
                    "Processing ls-refs command"
                );
                handle_ls_refs(
                    repo_path,
                    &mut writer,
                    &ref_patterns,
                    peel,
                    symrefs,
                    unborn,
                    &server_options,
                )
                .await?;
            }
            CommandRequest::Fetch {
                wants,
                haves,
                shallow,
                filter,
                done,
                client_caps,
            } => {
                tracing::debug!(
                    wants = wants.len(),
                    haves = haves.len(),
                    shallows = shallow.shallows.len(),
                    done,
                    "Processing fetch command"
                );
                handle_fetch(
                    repo_path,
                    &mut writer,
                    &wants,
                    &haves,
                    &shallow,
                    &filter,
                    done,
                    &client_caps,
                )
                .await?;
            }
            CommandRequest::ObjectInfo {
                oid,
                server_options,
            } => {
                tracing::debug!(oid = %oid, "Processing object-info command");
                handle_object_info(repo_path, &mut writer, &oid, &server_options).await?;
            }
            CommandRequest::Flush => {
                // Empty flush packet signals end of commands
                tracing::debug!("Received command flush - closing connection");
                break;
            }
            CommandRequest::Unknown(cmd) => {
                tracing::warn!(cmd = %cmd, "Unknown command, skipping");
                // Skip until flush
                skip_until_flush(&mut reader).await?;
                write_flush(&mut writer).await?;
            }
        }
    }

    Ok(())
}

/// Send the Protocol V2 capability advertisement.
/// This is the first thing sent after version negotiation.
pub async fn send_capability_advertisement<W: AsyncWrite + Unpin>(writer: &mut W) -> Result<()> {
    // Protocol version line
    write_pkt_line(writer, &PktLine::text("version 2")).await?;

    for capability in ADVERTISED_CAPABILITIES {
        write_pkt_line(writer, &PktLine::text(capability)).await?;
    }

    // End of capabilities
    write_flush(writer).await?;

    tracing::debug!("Sent Protocol V2 capability advertisement");
    Ok(())
}

/// Command request types in Protocol V2
#[derive(Debug)]
pub enum CommandRequest {
    LsRefs {
        ref_patterns: Vec<String>,
        peel: bool,
        symrefs: bool,
        unborn: bool,
        server_options: Vec<String>,
    },
    Fetch {
        wants: Vec<String>,
        haves: Vec<String>,
        shallow: ShallowRequest,
        filter: Option<String>,
        done: bool,
        client_caps: Vec<String>,
    },
    ObjectInfo {
        oid: String,
        server_options: Vec<String>,
    },
    /// Empty flush packet signals end of commands
    Flush,
    /// Unknown command type
    Unknown(String),
}

#[derive(Debug, Default)]
pub struct ShallowRequest {
    shallows: Vec<String>,
    deepen: Option<u32>,
    deepen_relative: bool,
    deepen_since: Option<i64>,
    deepen_not: Vec<String>,
}

/// Read a Protocol V2 command request.
/// Format:
///   command=<cmd>
///   capability=<cap>
///   ...
///   0001 (delimiter)
///   command-args...
///   0000 (flush)
async fn read_command_request<R: AsyncRead + Unpin>(reader: &mut R) -> Result<CommandRequest> {
    let mut command = None;
    let mut capabilities = Vec::new();
    let mut args = Vec::new();
    let mut found_delimiter = false;

    loop {
        let pkt = read_pkt_line(reader).await?;

        match pkt {
            PktLine::Flush => {
                if found_delimiter {
                    // End of request after delimiter
                    break;
                } else {
                    // Empty flush means end of commands
                    return Ok(CommandRequest::Flush);
                }
            }
            PktLine::Delim => {
                found_delimiter = true;
            }
            PktLine::ResponseEnd => {
                // End of stateless response
                return Ok(CommandRequest::Flush);
            }
            PktLine::Data(bytes) => {
                let line = String::from_utf8_lossy(&bytes);
                let line = line.trim_end_matches('\n');

                if !found_delimiter {
                    // Capability negotiation phase
                    if let Some(cmd) = line.strip_prefix("command=") {
                        command = Some(cmd.to_string());
                    } else if !line.is_empty() {
                        capabilities.push(line.to_string());
                    }
                } else {
                    // Command arguments phase
                    args.push(line.to_string());
                }
            }
        }
    }

    let cmd = match command {
        Some(c) => c,
        None => return Ok(CommandRequest::Flush),
    };

    // Parse based on command type
    match cmd.as_str() {
        "ls-refs" => {
            let mut ref_patterns = Vec::new();
            let mut peel = false;
            let mut symrefs = false;
            let mut unborn = false;
            let mut server_options = Vec::new();

            for arg in &args {
                if let Some(pattern) = arg.strip_prefix("ref-prefix ") {
                    ref_patterns.push(pattern.to_string());
                } else if *arg == "peel" {
                    peel = true;
                } else if *arg == "symrefs" {
                    symrefs = true;
                } else if *arg == "unborn" {
                    unborn = true;
                } else if let Some(opt) = arg.strip_prefix("server-option=") {
                    server_options.push(opt.to_string());
                }
            }

            Ok(CommandRequest::LsRefs {
                ref_patterns,
                peel,
                symrefs,
                unborn,
                server_options,
            })
        }
        "fetch" => {
            // Protocol V2 fetch: want/have/done are in the ARGS section (after 0001 delimiter),
            // while capabilities are in the header section (before 0001 delimiter).
            // Bug note: earlier version incorrectly parsed args from `capabilities`.
            let mut wants = Vec::new();
            let mut haves = Vec::new();
            let mut shallows = Vec::new();
            let mut deepen = None;
            let mut deepen_relative = false;
            let mut deepen_since = None;
            let mut deepen_not = Vec::new();
            let mut filter = None;
            let mut done = false;

            for arg in &args {
                if let Some(want) = arg.strip_prefix("want ") {
                    wants.push(want.to_string());
                } else if let Some(have) = arg.strip_prefix("have ") {
                    haves.push(have.to_string());
                } else if let Some(shallow) = arg.strip_prefix("shallow ") {
                    shallows.push(shallow.to_string());
                } else if let Some(d) = arg.strip_prefix("deepen ") {
                    deepen = Some(d.parse().context("invalid Protocol V2 deepen value")?);
                } else if *arg == "deepen-relative" {
                    deepen_relative = true;
                } else if let Some(timestamp) = arg.strip_prefix("deepen-since ") {
                    deepen_since = Some(
                        timestamp
                            .parse()
                            .context("invalid Protocol V2 deepen-since value")?,
                    );
                } else if let Some(revision) = arg.strip_prefix("deepen-not ") {
                    deepen_not.push(revision.to_string());
                } else if let Some(f) = arg.strip_prefix("filter ") {
                    filter = Some(f.to_string());
                } else if *arg == "done" {
                    done = true;
                }
            }

            // capabilities remain in the capabilities list (side-band, ofs-delta, etc.)
            Ok(CommandRequest::Fetch {
                wants,
                haves,
                shallow: ShallowRequest {
                    shallows,
                    deepen,
                    deepen_relative,
                    deepen_since,
                    deepen_not,
                },
                filter,
                done,
                client_caps: capabilities,
            })
        }
        "object-info" => {
            let mut oid = None;
            let mut server_options = Vec::new();

            for arg in &args {
                if let Some(o) = arg.strip_prefix("oid ") {
                    oid = Some(o.to_string());
                } else if let Some(opt) = arg.strip_prefix("server-option=") {
                    server_options.push(opt.to_string());
                }
            }

            match oid {
                Some(o) => Ok(CommandRequest::ObjectInfo {
                    oid: o,
                    server_options,
                }),
                None => Ok(CommandRequest::Unknown(cmd)),
            }
        }
        _ => Ok(CommandRequest::Unknown(cmd)),
    }
}

/// Skip packets until flush (for unknown commands).
///
/// Accepts any `AsyncRead + Unpin` directly.
async fn skip_until_flush<R: AsyncRead + Unpin>(reader: &mut R) -> Result<()> {
    loop {
        let pkt = read_pkt_line(reader).await?;
        if matches!(pkt, PktLine::Flush) {
            break;
        }
    }
    Ok(())
}

/// Handle the ls-refs command.
/// Sends ref advertisements based on client request.
///
/// Protocol V2 ls-refs response format per ref:
///   `<sha> <refname>[ symref-target:<target>][ peeled:<peeled-sha>]`
///
/// Key correctness points:
/// - ref-prefix filters: only send refs whose name starts with a requested prefix
/// - symrefs: HEAD needs `symref-target:refs/heads/<branch>` appended
/// - peel: annotated tags need `peeled:<commit-sha>` appended
/// - unborn: if HEAD points to a non-existent branch, send `unborn HEAD symref-target:<branch>`
/// - No duplicate HEAD: `list_refs` already handles HEAD via symbolic ref resolution,
///   so we don't add a second HEAD entry here
async fn handle_ls_refs<W: AsyncWrite + Unpin>(
    repo_path: &Path,
    writer: &mut W,
    ref_patterns: &[String],
    peel: bool,
    symrefs: bool,
    unborn: bool,
    _server_options: &[String],
) -> Result<()> {
    // CRITICAL: gix::Repository is NOT Send (contains RefCell), so all gix operations
    // MUST complete before any `.await` point. We collect all ref data synchronously first,
    // then do async I/O with the collected data.

    // --- Synchronous gix section (no .await allowed here) ---
    struct RefData {
        entries: Vec<(String, String, Option<String>)>, // (sha, refname, symref_target)
        unborn_line: Option<String>,
    }

    let ref_data: RefData = {
        let repo = gix::open(repo_path).context("failed to open repository for ls-refs")?;

        let mut ref_entries: Vec<(String, String, Option<String>)> = Vec::new();
        let mut unborn_line: Option<String> = None;

        // HEAD first — resolve symref target if client requested symrefs
        let head_ref = repo.head().ok();
        let head_target: Option<String> = if symrefs {
            head_ref.as_ref().and_then(|h| match &h.kind {
                gix::head::Kind::Symbolic(r) => Some(r.name.as_bstr().to_string()),
                gix::head::Kind::Unborn(name) => Some(name.as_bstr().to_string()),
                gix::head::Kind::Detached { .. } => None,
            })
        } else {
            None
        };

        match repo.head_id() {
            Ok(head_id) => {
                ref_entries.push((head_id.to_string(), "HEAD".to_string(), head_target.clone()));
            }
            Err(_) => {
                // HEAD points to unborn branch
                if unborn {
                    if let Some(target) = &head_target {
                        unborn_line = Some(format!("unborn HEAD symref-target:{}", target));
                    }
                }
            }
        }

        // All non-symbolic refs
        let references = repo.references().context("failed to list references")?;
        let all_refs = references.all()?;

        for reference in all_refs {
            let reference = match reference {
                Ok(r) => r,
                Err(_) => continue,
            };
            let refname = reference.name().as_bstr().to_string();

            // Skip HEAD — already handled above
            if refname == "HEAD" {
                continue;
            }

            let target = reference.target();
            match target {
                gix::refs::TargetRef::Object(id) => {
                    ref_entries.push((id.to_string(), refname, None));
                }
                gix::refs::TargetRef::Symbolic(_) => {
                    // Other symbolic refs (rare) — resolve to object
                    if let Ok(mut r) = repo.find_reference(&refname) {
                        if let Ok(id) = r.peel_to_id() {
                            ref_entries.push((id.to_string(), refname, None));
                        }
                    }
                }
            }
        }

        // repo is dropped here — no longer held across .await
        RefData {
            entries: ref_entries,
            unborn_line,
        }
    };
    // --- End synchronous gix section ---

    // Send unborn HEAD if applicable (now safe to .await)
    if let Some(line) = &ref_data.unborn_line {
        write_pkt_line(writer, &PktLine::text(line)).await?;
    }

    // Apply ref-prefix filtering
    let filtered: Vec<_> = if ref_patterns.is_empty() {
        ref_data.entries
    } else {
        ref_data
            .entries
            .into_iter()
            .filter(|(_, refname, _)| {
                ref_patterns
                    .iter()
                    .any(|prefix| refname.starts_with(prefix.as_str()))
            })
            .collect()
    };

    // Send each ref (all async I/O happens here, after gix objects are dropped)
    for (sha, refname, symref_target) in &filtered {
        let mut line = format!("{} {}", sha, refname);

        // Append symref-target if client requested and we have one
        if symrefs {
            if let Some(target) = symref_target {
                line.push_str(&format!(" symref-target:{}", target));
            }
        }

        // Append peeled SHA for annotated tags if client requested
        if peel && refname.starts_with("refs/tags/") {
            if let Some(peeled) = get_tag_peel(repo_path, sha) {
                // Only append if the peeled SHA differs from the tag object SHA
                // (i.e., it's actually an annotated tag pointing to a commit)
                if peeled != sha.as_str() {
                    line.push_str(&format!(" peeled:{}", peeled));
                }
            }
        }

        write_pkt_line(writer, &PktLine::text(&line)).await?;
    }

    // End of refs
    write_flush(writer).await?;

    tracing::debug!(refs = filtered.len(), "Sent ls-refs response (V2)");
    Ok(())
}

/// Handle the fetch command.
/// Negotiates common commits and sends packfile.
///
/// Protocol V2 fetch response format:
///   packfile section with sideband multiplexing:
///   - Band 1: pack data
///   - Band 2: progress messages
///   - Band 3: error messages
#[allow(clippy::too_many_arguments)]
async fn handle_fetch<W: AsyncWrite + Unpin>(
    repo_path: &Path,
    writer: &mut W,
    wants: &[String],
    haves: &[String],
    shallow: &ShallowRequest,
    filter: &Option<String>,
    done: bool,
    _client_caps: &[String],
) -> Result<()> {
    use sideband::{write_sideband_data, write_sideband_flush, write_sideband_progress};

    validate_fetch_features(shallow, filter)?;
    let shallow_update = build_shallow_update(repo_path, wants, shallow)?;

    // Check if client supports sideband (Protocol V2 fetch always uses sideband)
    let use_sideband = true; // V2 fetch always uses sideband per spec

    if wants.is_empty() {
        // Nothing to send
        write_pkt_line(writer, &PktLine::text("packfile")).await?;
        write_flush(writer).await?;
        return Ok(());
    }

    // Protocol V2 fetch response starts with section headers. A request carrying
    // `done` must proceed directly to the packfile section: the client treats an
    // acknowledgments section without `ready` followed by another section as a
    // protocol violation. During negotiation, advertise `ready` inside the
    // acknowledgments section before delimiting the following packfile section.
    if needs_acknowledgments(haves, done) {
        // Check which haves we have — synchronously, before any .await
        // CRITICAL: gix::Repository is !Send (contains RefCell), must not cross .await
        let acked_oids: Vec<String> = {
            let repo = gix::open(repo_path).ok();
            let mut acked = Vec::new();
            for have in haves {
                if let Some(ref r) = repo {
                    if let Ok(oid) = gix::ObjectId::from_hex(have.as_bytes()) {
                        if r.find_object(oid).is_ok() {
                            acked.push(have.clone());
                        }
                    }
                }
            }
            acked
            // repo dropped here
        };

        if !write_acknowledgments(writer, &acked_oids).await? {
            return Ok(());
        }
    }

    if let Some(update) = &shallow_update {
        if !update.response_lines.is_empty() {
            write_pkt_line(writer, &PktLine::text("shallow-info")).await?;
            for line in &update.response_lines {
                write_pkt_line(writer, &PktLine::text(line)).await?;
            }
            write_pkt_line(writer, &PktLine::Delim).await?;
        }
    }

    // Send packfile section header
    write_pkt_line(writer, &PktLine::text("packfile")).await?;

    // Generate packfile for the requested objects, excluding known haves
    let pack_data = generate_packfile(
        repo_path,
        wants,
        haves,
        shallow_update.as_ref(),
        filter.as_deref(),
    )
    .await?;

    if use_sideband {
        // Send progress
        write_sideband_progress(writer, "Enumerating objects: done.\n").await?;

        // Send packfile through sideband channel 1
        write_sideband_data(writer, &pack_data).await?;

        // Send done progress
        write_sideband_progress(writer, "Done.\n").await?;

        // End sideband with flush
        write_sideband_flush(writer).await?;
    } else {
        writer.write_all(&pack_data).await?;
        write_flush(writer).await?;
    }

    tracing::info!(
        pack_size = pack_data.len(),
        wants = wants.len(),
        haves = haves.len(),
        "Sent V2 fetch packfile"
    );
    Ok(())
}

fn needs_acknowledgments(haves: &[String], done: bool) -> bool {
    !haves.is_empty() && !done
}

/// Write a negotiation response and return whether the server is ready to
/// continue with a packfile section in the same response.
async fn write_acknowledgments<W: AsyncWrite + Unpin>(
    writer: &mut W,
    acked_oids: &[String],
) -> Result<bool> {
    write_pkt_line(writer, &PktLine::text("acknowledgments")).await?;
    for have in acked_oids {
        write_pkt_line(writer, &PktLine::text(&format!("ACK {}", have))).await?;
    }

    if !acked_oids.is_empty() {
        // `ready` is part of the acknowledgments section. The delimiter then
        // announces that another section (the packfile) follows.
        write_pkt_line(writer, &PktLine::text("ready")).await?;
        write_pkt_line(writer, &PktLine::Delim).await?;
        Ok(true)
    } else {
        write_pkt_line(writer, &PktLine::text("NAK")).await?;
        write_flush(writer).await?;
        Ok(false)
    }
}

fn validate_fetch_features(shallow: &ShallowRequest, filter: &Option<String>) -> Result<()> {
    if let Some(filter) = filter {
        if filter.is_empty()
            || filter.len() > 1024
            || filter
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            bail!("invalid Protocol V2 partial-clone filter specification");
        }
    }
    if shallow.deepen == Some(0) {
        bail!("Protocol V2 deepen depth must be greater than zero");
    }
    if shallow.deepen_relative && shallow.deepen.is_none() {
        bail!("Protocol V2 deepen-relative requires deepen");
    }
    if shallow.deepen.is_some()
        && (shallow.deepen_since.is_some() || !shallow.deepen_not.is_empty())
    {
        bail!("Protocol V2 deepen cannot be combined with deepen-since or deepen-not");
    }
    Ok(())
}

#[derive(Debug)]
struct ShallowUpdate {
    boundaries: Vec<String>,
    response_lines: Vec<String>,
}

fn build_shallow_update(
    repo_path: &Path,
    wants: &[String],
    request: &ShallowRequest,
) -> Result<Option<ShallowUpdate>> {
    let changes_depth = request.deepen.is_some()
        || request.deepen_since.is_some()
        || !request.deepen_not.is_empty();
    if !changes_depth {
        return Ok(None);
    }

    let boundaries = if let Some(depth) = request.deepen {
        if request.deepen_relative {
            if request.shallows.is_empty() {
                bail!("Protocol V2 deepen-relative requires at least one shallow boundary");
            }
            compute_depth_boundaries(repo_path, &request.shallows, depth, true)?
        } else {
            compute_depth_boundaries(repo_path, wants, depth, false)?
        }
    } else {
        compute_filtered_boundaries(repo_path, wants, request.deepen_since, &request.deepen_not)?
    };

    let old: HashSet<&str> = request.shallows.iter().map(String::as_str).collect();
    let new: HashSet<&str> = boundaries.iter().map(String::as_str).collect();
    let mut response_lines = Vec::new();

    for boundary in &boundaries {
        if !old.contains(boundary.as_str()) {
            response_lines.push(format!("shallow {boundary}"));
        }
    }
    for boundary in &request.shallows {
        if !new.contains(boundary.as_str()) {
            response_lines.push(format!("unshallow {boundary}"));
        }
    }

    Ok(Some(ShallowUpdate {
        boundaries,
        response_lines,
    }))
}

fn compute_depth_boundaries(
    repo_path: &Path,
    starts: &[String],
    depth: u32,
    relative: bool,
) -> Result<Vec<String>> {
    if starts.is_empty() {
        bail!("cannot compute shallow boundaries without a starting commit");
    }

    let graph = load_commit_graph(repo_path, starts, None, &[])?;
    let initial_depth = if relative { 0 } else { 1 };
    let mut queue: VecDeque<(String, u32)> = starts
        .iter()
        .cloned()
        .map(|oid| (oid, initial_depth))
        .collect();
    let mut included: HashMap<String, u32> = HashMap::new();

    while let Some((oid, current_depth)) = queue.pop_front() {
        if current_depth > depth {
            continue;
        }
        if included
            .get(&oid)
            .is_some_and(|known_depth| *known_depth <= current_depth)
        {
            continue;
        }
        included.insert(oid.clone(), current_depth);

        if current_depth < depth {
            if let Some(parents) = graph.get(&oid) {
                queue.extend(
                    parents
                        .iter()
                        .cloned()
                        .map(|parent| (parent, current_depth + 1)),
                );
            }
        }
    }

    Ok(find_boundaries(&graph, &included.keys().cloned().collect()))
}

fn compute_filtered_boundaries(
    repo_path: &Path,
    wants: &[String],
    deepen_since: Option<i64>,
    deepen_not: &[String],
) -> Result<Vec<String>> {
    let graph = load_commit_graph(repo_path, wants, deepen_since, deepen_not)?;
    let included: HashSet<String> = graph.keys().cloned().collect();
    Ok(find_boundaries(&graph, &included))
}

fn load_commit_graph(
    repo_path: &Path,
    starts: &[String],
    max_age: Option<i64>,
    excluded_revisions: &[String],
) -> Result<HashMap<String, Vec<String>>> {
    use crate::cli_gateway::global_gateway;

    let mut args = vec!["rev-list".to_string(), "--parents".to_string()];
    if let Some(timestamp) = max_age {
        args.push(format!("--max-age={timestamp}"));
    }
    args.extend(starts.iter().cloned());
    if !excluded_revisions.is_empty() {
        args.push("--not".to_string());
        args.extend(excluded_revisions.iter().cloned());
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .run(&arg_refs, Some(repo_path))?;
    output.ensure_success()?;

    let mut graph = HashMap::new();
    for line in output.stdout_str().lines() {
        let mut fields = line.split_whitespace();
        if let Some(oid) = fields.next() {
            graph.insert(oid.to_string(), fields.map(str::to_string).collect());
        }
    }
    Ok(graph)
}

fn find_boundaries(
    graph: &HashMap<String, Vec<String>>,
    included: &HashSet<String>,
) -> Vec<String> {
    let mut boundaries: Vec<String> = included
        .iter()
        .filter(|oid| {
            graph
                .get(*oid)
                .is_some_and(|parents| parents.iter().any(|parent| !included.contains(parent)))
        })
        .cloned()
        .collect();
    boundaries.sort();
    boundaries
}

/// Handle the object-info command.
async fn handle_object_info<W: AsyncWrite + Unpin>(
    repo_path: &Path,
    writer: &mut W,
    oid: &str,
    _server_options: &[String],
) -> Result<()> {
    // Get object size
    let size = get_object_size(repo_path, oid)?;

    write_pkt_line(writer, &PktLine::text("size")).await?;
    let mut line = String::with_capacity(oid.len() + 22);
    line.push_str(oid);
    line.push(' ');
    line.push_str(&size.to_string());
    write_pkt_line(writer, &PktLine::text(&line)).await?;
    write_flush(writer).await?;

    Ok(())
}

// ─── Git Operations ───────────────────────────────────────────────────────────

/// Get the peel (dereferenced) SHA of a tag using gix API.
fn get_tag_peel(repo_path: &Path, sha: &str) -> Option<String> {
    let repo = gix::open(repo_path).ok()?;
    let object_id = gix::ObjectId::from_hex(sha.as_bytes()).ok()?;

    // Find the object
    let object = repo.find_object(object_id).ok()?;

    // Check if it's a tag and get the peeled object
    if let Ok(tag) = object.try_into_tag() {
        // The tag points to another object - that's the peeled SHA
        let target_id = tag.target_id().ok()?;
        return Some(target_id.to_string());
    }

    // Not a tag or can't peel, return the original SHA
    Some(sha.to_string())
}

/// Get the size of a git object using gix API.
fn get_object_size(repo_path: &Path, oid: &str) -> Result<u64> {
    let repo = gix::open(repo_path).context("failed to open repository")?;
    let object_id = gix::ObjectId::from_hex(oid.as_bytes())
        .map_err(|e| anyhow::anyhow!("invalid object ID: {}", e))?;

    let object = repo
        .find_object(object_id)
        .map_err(|_| anyhow::anyhow!("object {} not found", oid))?;

    // Get the size of the object data
    let size = object.data.len() as u64;
    Ok(size)
}

/// Generate a packfile for the given wants, excluding known haves.
///
/// Uses `git pack-objects --revs --stdout` which reads revision specs from stdin.
/// Each want is written as `<sha>`, each have as `^<sha>` (exclude).
///
/// TODO(gix): Replace with gix pack generation when available.
/// The `gix` crate does not yet expose a stable pack-objects API,
/// so we fall back to the git CLI for this step.
async fn generate_packfile(
    repo_path: &Path,
    wants: &[String],
    haves: &[String],
    shallow_update: Option<&ShallowUpdate>,
    filter: Option<&str>,
) -> Result<Vec<u8>> {
    use crate::cli_gateway::global_gateway;
    use tokio::io::{AsyncReadExt, AsyncWriteExt as _};

    // Build stdin input. For a depth-changing request, shallow boundaries are
    // passed directly to pack-objects and known objects are intentionally
    // resent: excluding a shallow client's `have` as a normal full-history
    // commit would incorrectly exclude ancestors that the client does not own.
    let mut revs_input = String::new();
    if let Some(update) = shallow_update {
        for boundary in &update.boundaries {
            revs_input.push_str("--shallow ");
            revs_input.push_str(boundary);
            revs_input.push('\n');
        }
    }
    for want in wants {
        revs_input.push_str(want);
        revs_input.push('\n');
    }
    if shallow_update.is_none() {
        for have in haves {
            // Prefix with '^' to exclude commits reachable from haves
            revs_input.push('^');
            revs_input.push_str(have);
            revs_input.push('\n');
        }
    }

    let mut pack_args = vec![
        "pack-objects".to_string(),
        "--revs".to_string(),
        "--stdout".to_string(),
        "--thin".to_string(),
    ];
    if shallow_update.is_some_and(|update| !update.boundaries.is_empty()) {
        pack_args.push("--shallow".to_string());
    }
    if let Some(filter) = filter {
        pack_args.push(format!("--filter={filter}"));
    }
    let pack_arg_refs: Vec<&str> = pack_args.iter().map(String::as_str).collect();
    let mut cmd = global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?
        .spawn_async(&pack_arg_refs, Some(repo_path))
        .await
        .context("failed to spawn git pack-objects")?;

    // Write revision list to stdin, then close it
    if let Some(mut stdin) = cmd.stdin.take() {
        stdin
            .write_all(revs_input.as_bytes())
            .await
            .context("failed to write revs to pack-objects stdin")?;
        // stdin is dropped here, closing the pipe
    }

    let stdout = cmd.stdout.take().context("no stdout from pack-objects")?;
    let mut reader = BufReader::new(stdout);
    let mut pack_data = Vec::new();
    reader
        .read_to_end(&mut pack_data)
        .await
        .context("failed to read packfile from pack-objects")?;

    let status = cmd.wait().await.context("git pack-objects wait failed")?;
    if !status.success() {
        // Read stderr for diagnostics
        let stderr_msg = if let Some(mut se) = cmd.stderr.take() {
            let mut buf = Vec::new();
            se.read_to_end(&mut buf).await.ok();
            String::from_utf8_lossy(&buf).into_owned()
        } else {
            String::new()
        };
        bail!(
            "git pack-objects failed ({}): {}",
            status,
            stderr_msg.trim()
        );
    }

    tracing::debug!(
        pack_bytes = pack_data.len(),
        wants = wants.len(),
        haves = haves.len(),
        "pack-objects complete"
    );

    Ok(pack_data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capability_advertisement_format() {
        assert_eq!(PROTOCOL_VERSION, "2");
        assert_eq!(caps::LS_REFS, "ls-refs");
        assert_eq!(caps::FETCH, "fetch");
        assert!(ADVERTISED_CAPABILITIES.contains(&caps::FETCH_SHALLOW));
        assert!(caps::FETCH_SHALLOW.contains("filter"));
    }

    #[tokio::test]
    async fn serialized_advertisement_includes_only_supported_fetch_features() {
        let mut output = Vec::new();
        send_capability_advertisement(&mut output).await.unwrap();
        let output = String::from_utf8(output).unwrap();

        assert!(output.contains("fetch=shallow filter\n"));
    }

    #[test]
    fn fetch_feature_validation_accepts_shallow_and_rejects_invalid_combinations() {
        let mut request = ShallowRequest::default();
        assert!(validate_fetch_features(&request, &None).is_ok());
        assert!(validate_fetch_features(&request, &Some("blob:none".into())).is_ok());
        assert!(validate_fetch_features(&request, &Some("blob:limit=1 m".into())).is_err());

        request.deepen = Some(0);
        assert!(validate_fetch_features(&request, &None).is_err());
        request.deepen = None;
        request.deepen_relative = true;
        assert!(validate_fetch_features(&request, &None).is_err());
        request.deepen = Some(2);
        assert!(validate_fetch_features(&request, &None).is_ok());
    }

    #[test]
    fn shallow_boundaries_are_commits_with_excluded_parents() {
        let graph = HashMap::from([
            ("a".into(), vec!["b".into()]),
            ("b".into(), vec!["c".into()]),
            ("c".into(), vec![]),
        ]);

        assert_eq!(find_boundaries(&graph, &HashSet::from(["a".into()])), ["a"]);
        assert_eq!(
            find_boundaries(&graph, &HashSet::from(["a".into(), "b".into()])),
            ["b"]
        );
        assert!(
            find_boundaries(&graph, &HashSet::from(["a".into(), "b".into(), "c".into()]))
                .is_empty()
        );
    }

    #[test]
    fn done_request_skips_acknowledgments_section() {
        let haves = vec!["a".repeat(40)];
        assert!(needs_acknowledgments(&haves, false));
        assert!(!needs_acknowledgments(&haves, true));
        assert!(!needs_acknowledgments(&[], false));
    }

    #[tokio::test]
    async fn ready_precedes_the_packfile_section_delimiter() {
        let oid = "a".repeat(40);
        let mut output = Vec::new();

        assert!(
            write_acknowledgments(&mut output, std::slice::from_ref(&oid))
                .await
                .unwrap()
        );

        let serialized = String::from_utf8(output).unwrap();
        let ack = serialized.find(&format!("ACK {oid}\n")).unwrap();
        let ready = serialized.find("ready\n").unwrap();
        assert!(ack < ready);
        assert!(serialized.ends_with("0001"));
    }

    #[tokio::test]
    async fn nak_ends_negotiation_without_a_following_section() {
        let mut output = Vec::new();

        assert!(!write_acknowledgments(&mut output, &[]).await.unwrap());

        let serialized = String::from_utf8(output).unwrap();
        assert!(serialized.contains("NAK\n"));
        assert!(serialized.ends_with("0000"));
        assert!(!serialized.ends_with("0001"));
    }
}
