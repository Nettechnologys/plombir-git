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
use tokio::io::{split, AsyncRead, AsyncWrite, BufReader};

use super::{pack_stream, ClientRefusal};
use crate::pkt_line::{read_pkt_line, write_flush, write_pkt_line, PktLine};
use crate::sideband;

/// V2 Protocol constants
pub const PROTOCOL_VERSION: &str = "2";

/// Capabilities that Plombir Git currently implements end to end.
///
/// Keep HTTP and SSH advertisements sourced from this list. Unsupported fetch
/// features must not be appended here until `handle_fetch` implements them.
pub const ADVERTISED_CAPABILITIES: &[&str] = &[
    "agent=plombir-git/0.1",
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
        let command = read_command_request(&mut reader).await;
        match super::tell_client_of_refusal(&mut writer, command).await? {
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
                pack,
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
                    pack,
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
        let command = read_command_request(&mut reader).await;
        let command = super::tell_client_of_refusal(&mut write_half, command).await?;

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
                pack,
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
                    pack,
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

/// The pack-shaping arguments of a v2 `fetch`. Both default to off: in v2
/// they are things the client asks for, not things the server assumes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FetchPackOptions {
    /// `thin-pack`: deltas may name bases the client already has.
    pub thin: bool,
    /// `ofs-delta`: deltas may address their base by pack offset.
    pub ofs_delta: bool,
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
        /// What the client's `thin-pack` / `ofs-delta` arguments allow the
        /// pack to contain.
        pack: FetchPackOptions,
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
    let (command, args) = match read_command_frames(reader).await? {
        CommandFrames::Flush => return Ok(CommandRequest::Flush),
        CommandFrames::Command { command, args } => (command, args),
    };

    let cmd = match command {
        Some(c) => c,
        None => return Ok(CommandRequest::Flush),
    };

    // Parse based on command type
    match cmd.as_str() {
        "ls-refs" => Ok(parse_ls_refs_args(&args)),
        "fetch" => parse_fetch_args(&args),
        "object-info" => Ok(parse_object_info_args(&args, cmd)),
        _ => Ok(CommandRequest::Unknown(cmd)),
    }
}

/// Outcome of reading the framed header + args of a Protocol V2 command.
#[derive(Debug)]
enum CommandFrames {
    /// An empty flush or response-end — the caller should return `Flush`.
    Flush,
    /// The parsed frames: the `command=` line and the args section (after the
    /// `0001` delimiter). The header's capability lines (`agent=`,
    /// `object-format=`, ...) count toward the limits but steer nothing: every
    /// option a command can ask for is one of its arguments.
    Command {
        command: Option<String>,
        args: Vec<String>,
    },
}

/// Read the pkt-line frames of one command request, splitting the header
/// (command + capabilities) from the args section at the `0001` delimiter.
async fn read_command_frames<R: AsyncRead + Unpin>(reader: &mut R) -> Result<CommandFrames> {
    read_command_frames_with_limits(
        reader,
        super::MAX_NEGOTIATION_ENTRIES,
        super::MAX_NEGOTIATION_INPUT_BYTES,
    )
    .await
}

async fn read_command_frames_with_limits<R: AsyncRead + Unpin>(
    reader: &mut R,
    max_entries: usize,
    max_bytes: usize,
) -> Result<CommandFrames> {
    let mut command = None;
    let mut args = Vec::new();
    let mut found_delimiter = false;
    let mut data_frames = 0_usize;
    let mut input_bytes = 0_usize;

    loop {
        let pkt = read_pkt_line(reader).await?;

        match pkt {
            PktLine::Flush => {
                if found_delimiter {
                    // End of request after delimiter
                    break;
                } else {
                    // Empty flush means end of commands
                    return Ok(CommandFrames::Flush);
                }
            }
            PktLine::Delim => {
                found_delimiter = true;
            }
            PktLine::ResponseEnd => {
                // End of stateless response
                return Ok(CommandFrames::Flush);
            }
            PktLine::Data(bytes) => {
                if data_frames >= max_entries {
                    return Err(ClientRefusal::new(
                        "protocol v2 negotiation exceeds the configured entry limit",
                    )
                    .into());
                }
                data_frames += 1;
                input_bytes = input_bytes
                    .checked_add(bytes.len())
                    .filter(|size| *size <= max_bytes)
                    .ok_or_else(|| {
                        ClientRefusal::new(
                            "protocol v2 negotiation exceeds the configured byte limit",
                        )
                    })?;
                let line = String::from_utf8_lossy(&bytes);
                let line = line.trim_end_matches('\n');

                if !found_delimiter {
                    // Capability negotiation phase
                    if let Some(cmd) = line.strip_prefix("command=") {
                        command = Some(cmd.to_string());
                    }
                } else {
                    // Command arguments phase
                    args.push(line.to_string());
                }
            }
        }
    }

    Ok(CommandFrames::Command { command, args })
}

/// Parse the args section of an `ls-refs` command.
fn parse_ls_refs_args(args: &[String]) -> CommandRequest {
    let mut ref_patterns = Vec::new();
    let mut peel = false;
    let mut symrefs = false;
    let mut unborn = false;
    let mut server_options = Vec::new();

    for arg in args {
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

    CommandRequest::LsRefs {
        ref_patterns,
        peel,
        symrefs,
        unborn,
        server_options,
    }
}

/// Parse the args section of a `fetch` command.
///
/// Protocol V2 fetch: want/have/done are in the ARGS section (after 0001 delimiter),
/// while capabilities are in the header section (before 0001 delimiter).
/// Bug note: earlier version incorrectly parsed args from `capabilities`.
fn parse_fetch_args(args: &[String]) -> Result<CommandRequest> {
    let mut wants = Vec::new();
    let mut haves = Vec::new();
    let mut shallows = Vec::new();
    let mut deepen = None;
    let mut deepen_relative = false;
    let mut deepen_since = None;
    let mut deepen_not = Vec::new();
    let mut filter = None;
    let mut done = false;
    let mut pack = FetchPackOptions::default();

    for arg in args {
        if let Some(want) = arg.strip_prefix("want ") {
            wants.push(want.to_string());
        } else if let Some(have) = arg.strip_prefix("have ") {
            haves.push(have.to_string());
        } else if let Some(shallow) = arg.strip_prefix("shallow ") {
            shallows.push(shallow.to_string());
        } else if let Some(d) = arg.strip_prefix("deepen ") {
            deepen = Some(
                d.parse()
                    .map_err(|_| ClientRefusal::new("invalid Protocol V2 deepen value"))?,
            );
        } else if *arg == "deepen-relative" {
            deepen_relative = true;
        } else if let Some(timestamp) = arg.strip_prefix("deepen-since ") {
            deepen_since = Some(
                timestamp
                    .parse()
                    .map_err(|_| ClientRefusal::new("invalid Protocol V2 deepen-since value"))?,
            );
        } else if let Some(revision) = arg.strip_prefix("deepen-not ") {
            deepen_not.push(revision.to_string());
        } else if let Some(f) = arg.strip_prefix("filter ") {
            filter = Some(f.to_string());
        } else if *arg == "done" {
            done = true;
        } else if *arg == "thin-pack" {
            pack.thin = true;
        } else if *arg == "ofs-delta" {
            pack.ofs_delta = true;
        }
    }

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
        pack,
    })
}

/// Parse the args section of an `object-info` command.
fn parse_object_info_args(args: &[String], cmd: String) -> CommandRequest {
    let mut oid = None;
    let mut server_options = Vec::new();

    for arg in args {
        if let Some(o) = arg.strip_prefix("oid ") {
            oid = Some(o.to_string());
        } else if let Some(opt) = arg.strip_prefix("server-option=") {
            server_options.push(opt.to_string());
        }
    }

    match oid {
        Some(o) => CommandRequest::ObjectInfo {
            oid: o,
            server_options,
        },
        None => CommandRequest::Unknown(cmd),
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
        let advertisement = crate::ref_advertisement::collect_for_clients(repo_path)
            .context("failed to collect references for ls-refs")?;

        let mut ref_entries: Vec<(String, String, Option<String>)> = Vec::new();
        let head_target = if symrefs {
            advertisement.head_target.clone()
        } else {
            None
        };

        let unborn_line = match advertisement.head_oid {
            Some(head_oid) => {
                ref_entries.push((head_oid, "HEAD".to_string(), head_target.clone()));
                None
            }
            None if unborn => head_target
                .as_ref()
                .map(|target| format!("unborn HEAD symref-target:{target}")),
            None => None,
        };

        for (oid, refname) in advertisement.refs {
            ref_entries.push((oid, refname, None));
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
            let peeled = get_tag_peel(repo_path, sha)
                .with_context(|| format!("failed to peel advertised tag `{refname}`"))?;
            // Only append if the peeled SHA differs from the tag object SHA
            // (i.e., it's actually an annotated tag pointing to a commit)
            if peeled != sha.as_str() {
                line.push_str(&format!(" peeled:{}", peeled));
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
///
/// A request refused on the client's account reaches it as an `ERR` packet.
#[allow(clippy::too_many_arguments)]
async fn handle_fetch<W: AsyncWrite + Unpin>(
    repo_path: &Path,
    writer: &mut W,
    wants: &[String],
    haves: &[String],
    shallow: &ShallowRequest,
    filter: &Option<String>,
    done: bool,
    pack: FetchPackOptions,
) -> Result<()> {
    let outcome =
        fetch_response(repo_path, writer, wants, haves, shallow, filter, done, pack).await;
    super::tell_client_of_refusal(writer, outcome).await
}

#[allow(clippy::too_many_arguments)]
async fn fetch_response<W: AsyncWrite + Unpin>(
    repo_path: &Path,
    writer: &mut W,
    wants: &[String],
    haves: &[String],
    shallow: &ShallowRequest,
    filter: &Option<String>,
    done: bool,
    pack: FetchPackOptions,
) -> Result<()> {
    use sideband::{write_sideband_flush, write_sideband_progress};

    validate_fetch_features(shallow, filter)?;
    // A want the advertisement did not offer is refused, as over v0/v1: the
    // tip of a server-private ref, or a commit a force push left behind, is
    // not the client's to fetch by SHA (card_ad83ad72d14a).
    let checked = {
        let (repo_path, wants) = (repo_path.to_path_buf(), wants.to_vec());
        tokio::task::spawn_blocking(move || {
            crate::ref_advertisement::unadvertised_want(&repo_path, &wants)
        })
        .await
        .context("V2 fetch want check did not complete")??
    };
    if let Some(want) = checked {
        return Err(ClientRefusal::new(format!("upload-pack: not our ref {want}")).into());
    }
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
    // Only the haves this repository holds may reach `pack-objects`: it dies
    // on `^<oid>` of an object it cannot find, so a client with commits of its
    // own — the ordinary case — failed its fetch the moment a common one
    // arrived in the same round and the server answered `ready`.
    // Checked synchronously, before any .await: gix::Repository is !Send.
    let common = acknowledged_haves(repo_path, haves);
    if needs_acknowledgments(haves, done) && !write_acknowledgments(writer, &common).await? {
        return Ok(());
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

    // The progress line moves ahead of generation: the pack is now streamed as
    // git produces it, so there is no "after the pack, before the pack was
    // sent" moment left to write it in.
    if use_sideband {
        write_sideband_progress(writer, "Enumerating objects: done.\n").await?;
    }

    // Generate packfile for the requested objects, excluding known haves, and
    // forward it band-1 chunk by band-1 chunk.
    let pack_size = stream_packfile(
        repo_path,
        writer,
        wants,
        &common,
        shallow_update.as_ref(),
        filter.as_deref(),
        use_sideband,
        pack,
    )
    .await?;

    if use_sideband {
        // Send done progress
        write_sideband_progress(writer, "Done.\n").await?;

        // End sideband with flush
        write_sideband_flush(writer).await?;
    } else {
        write_flush(writer).await?;
    }

    tracing::info!(
        pack_size,
        wants = wants.len(),
        haves = haves.len(),
        "Sent V2 fetch packfile"
    );
    Ok(())
}

/// Return the client's `have` objects that are present in the repository.
///
/// A repository that cannot be opened cannot answer the common-object question.
/// Keep the existing best-effort fetch behavior, but make that degraded answer
/// visible to operators instead of silently treating every `have` as absent.
///
/// The same distinction holds per object: `try_find_object` separates "the odb
/// says this object is not here" from "the odb could not be read". Only the
/// first is an honest absence; the second stays unacknowledged *and* logged, so
/// a corrupt pack degrades the fetch to a full transfer with an operator trail
/// instead of masquerading as a client that shares nothing with us.
fn acknowledged_haves(repo_path: &Path, haves: &[String]) -> Vec<String> {
    let repo = match crate::repository::open(repo_path) {
        Ok(repo) => repo,
        Err(error) => {
            tracing::warn!(
                repo = %repo_path.display(),
                error = %format!("{error:#}"),
                "cannot open repository while negotiating V2 fetch acknowledgments"
            );
            return Vec::new();
        }
    };

    let mut acked = Vec::new();
    for have in haves {
        let Ok(oid) = gix::ObjectId::from_hex(have.as_bytes()) else {
            continue;
        };
        match repo.try_find_object(oid) {
            Ok(Some(_)) => acked.push(have.clone()),
            Ok(None) => {}
            Err(error) => tracing::warn!(
                repo = %repo_path.display(),
                object = %have,
                error = %format!("{error:#}"),
                "cannot read object while negotiating V2 fetch acknowledgments"
            ),
        }
    }
    acked
    // `repo` is dropped here, before the caller awaits.
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
            return Err(ClientRefusal::new(
                "invalid Protocol V2 partial-clone filter specification",
            )
            .into());
        }
    }
    if shallow.deepen == Some(0) {
        return Err(
            ClientRefusal::new("Protocol V2 deepen depth must be greater than zero").into(),
        );
    }
    if shallow.deepen_relative && shallow.deepen.is_none() {
        return Err(ClientRefusal::new("Protocol V2 deepen-relative requires deepen").into());
    }
    if shallow.deepen.is_some()
        && (shallow.deepen_since.is_some() || !shallow.deepen_not.is_empty())
    {
        return Err(ClientRefusal::new(
            "Protocol V2 deepen cannot be combined with deepen-since or deepen-not",
        )
        .into());
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
                return Err(ClientRefusal::new(
                    "Protocol V2 deepen-relative requires at least one shallow boundary",
                )
                .into());
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
fn get_tag_peel(repo_path: &Path, sha: &str) -> Result<String> {
    let repo = crate::repository::open(repo_path)
        .context("failed to open repository while peeling a tag")?;
    let object_id = gix::ObjectId::from_hex(sha.as_bytes())
        .context("advertised tag has an invalid object id")?;

    // Find the object
    let object = repo
        .find_object(object_id)
        .context("failed to read advertised tag object")?;

    // Check if it's a tag and get the peeled object
    if object.kind == gix::object::Kind::Tag {
        let tag = object
            .try_into_tag()
            .context("advertised tag object could not be decoded")?;
        // The tag points to another object - that's the peeled SHA
        let target_id = tag
            .target_id()
            .context("advertised tag target could not be read")?;
        return Ok(target_id.to_string());
    }

    // A lightweight tag already points straight at its target.
    Ok(sha.to_string())
}

/// Get the size of a git object using gix API.
///
/// The two ways this can fail are not the same answer, so they must not share a
/// message: an object the repository does not carry is `object ... not found`,
/// while an odb that refused to answer keeps its own cause chain. Collapsing
/// both into "not found" told the operator a healthy-but-unreadable pack was a
/// client asking for something that never existed.
fn get_object_size(repo_path: &Path, oid: &str) -> Result<u64> {
    let repo = crate::repository::open(repo_path).context("failed to open repository")?;
    let object_id = gix::ObjectId::from_hex(oid.as_bytes())
        .map_err(|e| anyhow::anyhow!("invalid object ID: {}", e))?;

    let object = repo
        .try_find_object(object_id)
        .with_context(|| format!("failed to read object {oid}"))?
        .ok_or_else(|| anyhow::anyhow!("object {} not found", oid))?;

    // Get the size of the object data
    let size = object.data.len() as u64;
    Ok(size)
}

/// Generate a packfile for the given wants and stream it to `writer`.
///
/// Uses `git pack-objects --revs --stdout` which reads revision specs from stdin.
/// Each want is written as `<sha>`, each have as `^<sha>` (exclude).
///
/// The pack is forwarded as git produces it rather than returned as a `Vec`:
/// the V2 fetch response is the largest thing this server sends, and holding it
/// whole made one clone cost a repository of memory
/// (see [`crate::protocol::pack_stream`]). Returns how many pack bytes went out.
///
/// TODO(gix): Replace with gix pack generation when available.
/// The `gix` crate does not yet expose a stable pack-objects API,
/// so we fall back to the git CLI for this step.
#[allow(clippy::too_many_arguments)]
async fn stream_packfile<W: AsyncWrite + Unpin>(
    repo_path: &Path,
    writer: &mut W,
    wants: &[String],
    haves: &[String],
    shallow_update: Option<&ShallowUpdate>,
    filter: Option<&str>,
    use_sideband: bool,
    pack: FetchPackOptions,
) -> Result<u64> {
    use crate::cli_gateway::global_gateway;
    use tokio::io::AsyncWriteExt as _;

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
    ];
    // A thin pack deltas against objects the client claims to have and leaves
    // them out; a client that did not ask for one cannot resolve those deltas
    // and fails on `missing delta base`. Same rule as v0/v1 (`PackOptions`).
    if pack.thin {
        pack_args.push("--thin".to_string());
    }
    if pack.ofs_delta {
        pack_args.push("--delta-base-offset".to_string());
    }
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

    let pack_bytes = pack_stream::stream_pack_objects(cmd, writer, use_sideband).await?;

    tracing::debug!(
        pack_bytes,
        wants = wants.len(),
        haves = haves.len(),
        "pack-objects complete"
    );

    Ok(pack_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::CapturedLogs;

    #[tokio::test]
    async fn ls_refs_does_not_list_the_server_private_namespaces() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = crate::test_support::repository_with_server_private_refs(dir.path());
        let mut output = Vec::new();

        handle_ls_refs(&repo_path, &mut output, &[], false, true, true, &[])
            .await
            .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("refs/heads/main"), "{output}");
        assert!(
            crate::test_support::server_private_refs_in(&output).is_empty(),
            "{output}"
        );
    }

    #[tokio::test]
    async fn ls_refs_advertises_only_an_explicitly_unborn_head_as_unborn() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("unborn.git");
        gix::init_bare(&repo_path).unwrap();
        let mut output = Vec::new();

        handle_ls_refs(&repo_path, &mut output, &[], false, true, true, &[])
            .await
            .unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("unborn HEAD symref-target:refs/heads/"));
    }

    #[tokio::test]
    async fn ls_refs_rejects_a_malformed_head_instead_of_calling_it_unborn() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("broken-head.git");
        gix::init_bare(&repo_path).unwrap();
        std::fs::write(repo_path.join("HEAD"), "not a ref at all\n").unwrap();
        let mut output = Vec::new();

        let error = handle_ls_refs(&repo_path, &mut output, &[], false, true, true, &[])
            .await
            .unwrap_err();

        assert!(
            output.is_empty(),
            "no partial ls-refs response may be written"
        );
        assert!(format!("{error:#}").contains("failed to read HEAD"));
    }

    #[test]
    fn requested_tag_peel_propagates_a_missing_object() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("unborn.git");
        gix::init_bare(&repo_path).unwrap();

        let error =
            get_tag_peel(&repo_path, "0000000000000000000000000000000000000000").unwrap_err();

        assert!(format!("{error:#}").contains("failed to read advertised tag object"));
    }

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

    fn repository_with_commit() -> (tempfile::TempDir, std::path::PathBuf, String) {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        let git = crate::cli_gateway::GitCommandGateway::new().unwrap();
        git.run_or_bail(&["init", "-q", repo_path.to_str().unwrap()], None)
            .unwrap();
        for args in [
            vec!["config", "user.email", "fetch@example.com"],
            vec!["config", "user.name", "Fetch"],
            vec!["commit", "--allow-empty", "-qm", "fixture"],
        ] {
            git.run_or_bail(&args, Some(&repo_path)).unwrap();
        }
        let have = git
            .run(&["rev-parse", "HEAD"], Some(&repo_path))
            .unwrap()
            .stdout_str()
            .trim()
            .to_string();

        (dir, repo_path, have)
    }

    #[test]
    fn acknowledged_haves_preserves_a_common_commit() {
        let (_dir, repo_path, have) = repository_with_commit();

        assert_eq!(
            acknowledged_haves(&repo_path, std::slice::from_ref(&have)),
            vec![have]
        );
    }

    #[test]
    fn acknowledged_haves_returns_empty_when_repository_config_is_malformed() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("broken.git");
        gix::init_bare(&repo_path).unwrap();
        std::fs::write(repo_path.join("config"), "[core\n").unwrap();
        assert!(gix::open(&repo_path).is_err());

        assert!(acknowledged_haves(&repo_path, &["a".repeat(40)]).is_empty());
    }

    /// A `have` the server genuinely never had is the ordinary negotiation
    /// outcome: it stays unacknowledged and must NOT look like a storage
    /// incident in the log.
    #[test]
    fn acknowledged_haves_silently_skips_an_object_the_server_never_had() {
        let (_dir, repo_path, _have) = repository_with_commit();
        let (logs, _guard) = CapturedLogs::capture();
        let absent = "b".repeat(40);

        assert!(acknowledged_haves(&repo_path, std::slice::from_ref(&absent)).is_empty());
        assert_eq!(logs.rendered(), "", "an absent object is not a failure");
    }

    /// The distinction the card is about: the odb refusing to read an object is
    /// not the same answer as the object not being there. It stays
    /// unacknowledged either way, but only the failure gets an operator trail.
    #[test]
    fn acknowledged_haves_reports_an_unreadable_object_instead_of_calling_it_absent() {
        let (_dir, repo_path, have) = repository_with_commit();
        let loose = repo_path
            .join(".git/objects")
            .join(&have[..2])
            .join(&have[2..]);
        assert!(loose.is_file(), "fixture must keep the commit loose");
        // Not deleted — present but undecodable, which is exactly what a
        // corrupt odb looks like and what `Ok(None)` would misreport. Loose
        // objects land read-only, so replace the file rather than write over it.
        std::fs::remove_file(&loose).unwrap();
        std::fs::write(&loose, b"not a zlib stream").unwrap();

        let (logs, _guard) = CapturedLogs::capture();
        let acked = acknowledged_haves(&repo_path, std::slice::from_ref(&have));

        assert!(acked.is_empty(), "an unreadable object must not be acked");
        let rendered = logs.rendered();
        assert!(rendered.contains(&have), "{rendered}");
        assert!(
            rendered.contains("cannot read object while negotiating"),
            "{rendered}"
        );
    }

    /// `object-info` returns an error either way, but the operator reads the
    /// message: an absent object is the client's question, an unreadable one is
    /// our storage. The old `map_err(|_| "not found")` printed the first for
    /// both and dropped the real cause.
    #[test]
    fn object_size_separates_an_absent_object_from_an_unreadable_one() {
        let (_dir, repo_path, have) = repository_with_commit();

        let absent = get_object_size(&repo_path, &"b".repeat(40)).unwrap_err();
        assert!(format!("{absent:#}").contains("not found"), "{absent:#}");

        let loose = repo_path
            .join(".git/objects")
            .join(&have[..2])
            .join(&have[2..]);
        std::fs::remove_file(&loose).unwrap();
        std::fs::write(&loose, b"not a zlib stream").unwrap();

        let unreadable = get_object_size(&repo_path, &have).unwrap_err();
        let rendered = format!("{unreadable:#}");
        assert!(rendered.contains("failed to read object"), "{rendered}");
        assert!(
            !rendered.contains("not found"),
            "a storage failure must not be reported as a missing object: {rendered}"
        );
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

    // --- Protocol V2 command-request negotiation parsing ---

    /// Encode one pkt-line (`<4-hex-len><payload>`) for building request streams.
    fn pkt_bytes(data: &[u8]) -> Vec<u8> {
        let mut out = format!("{:04x}", data.len() + 4).into_bytes();
        out.extend_from_slice(data);
        out
    }

    #[tokio::test]
    async fn read_command_request_parses_ls_refs() {
        use std::io::Cursor;
        let mut buf = Vec::new();
        buf.extend_from_slice(&pkt_bytes(b"command=ls-refs\n"));
        buf.extend_from_slice(&pkt_bytes(b"agent=git/2.40\n"));
        buf.extend_from_slice(b"0001"); // delimiter → args section
        buf.extend_from_slice(&pkt_bytes(b"ref-prefix refs/heads/\n"));
        buf.extend_from_slice(&pkt_bytes(b"peel\n"));
        buf.extend_from_slice(&pkt_bytes(b"symrefs\n"));
        buf.extend_from_slice(b"0000"); // flush → end of request

        let mut reader = Cursor::new(buf);
        match read_command_request(&mut reader).await.unwrap() {
            CommandRequest::LsRefs {
                ref_patterns,
                peel,
                symrefs,
                unborn,
                ..
            } => {
                assert_eq!(ref_patterns, vec!["refs/heads/".to_string()]);
                assert!(peel);
                assert!(symrefs);
                assert!(!unborn);
            }
            other => panic!("expected LsRefs, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn read_command_request_parses_fetch_wants_and_haves() {
        use std::io::Cursor;
        let want = "a".repeat(40);
        let have = "b".repeat(40);
        let mut buf = Vec::new();
        buf.extend_from_slice(&pkt_bytes(b"command=fetch\n"));
        buf.extend_from_slice(&pkt_bytes(b"agent=git/2.40\n"));
        buf.extend_from_slice(b"0001");
        buf.extend_from_slice(&pkt_bytes(format!("want {want}\n").as_bytes()));
        buf.extend_from_slice(&pkt_bytes(format!("have {have}\n").as_bytes()));
        buf.extend_from_slice(&pkt_bytes(b"done\n"));
        buf.extend_from_slice(b"0000");

        let mut reader = Cursor::new(buf);
        match read_command_request(&mut reader).await.unwrap() {
            CommandRequest::Fetch {
                wants,
                haves,
                done,
                pack,
                ..
            } => {
                assert_eq!(wants, vec![want]);
                assert_eq!(haves, vec![have]);
                assert!(done);
                // Nothing asked for, nothing assumed.
                assert_eq!(pack, FetchPackOptions::default());
            }
            other => panic!("expected Fetch, got {other:?}"),
        }
    }

    /// card_54503ff49d6b: `thin-pack` and `ofs-delta` are fetch *arguments* in
    /// v2 — after the delimiter, where stock git sends them.
    #[tokio::test]
    async fn read_command_request_reads_pack_arguments() {
        use std::io::Cursor;
        let mut buf = Vec::new();
        buf.extend_from_slice(&pkt_bytes(b"command=fetch\n"));
        buf.extend_from_slice(b"0001");
        buf.extend_from_slice(&pkt_bytes(b"thin-pack\n"));
        buf.extend_from_slice(&pkt_bytes(b"ofs-delta\n"));
        buf.extend_from_slice(&pkt_bytes(format!("want {}\n", "a".repeat(40)).as_bytes()));
        buf.extend_from_slice(&pkt_bytes(b"done\n"));
        buf.extend_from_slice(b"0000");

        match read_command_request(&mut Cursor::new(buf)).await.unwrap() {
            CommandRequest::Fetch { pack, .. } => assert_eq!(
                pack,
                FetchPackOptions {
                    thin: true,
                    ofs_delta: true
                }
            ),
            other => panic!("expected Fetch, got {other:?}"),
        }
    }

    /// A repository whose `newer` commit rewrites one line of a file `older`
    /// already holds — the shape `pack-objects --thin` deltas against the
    /// excluded base.
    fn repository_with_a_deltifiable_change(
    ) -> (tempfile::TempDir, std::path::PathBuf, String, String) {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("repo");
        let git = crate::cli_gateway::GitCommandGateway::new().unwrap();
        git.run_or_bail(&["init", "-q", repo_path.to_str().unwrap()], None)
            .unwrap();
        for args in [
            vec!["config", "user.email", "fetch@example.com"],
            vec!["config", "user.name", "Fetch"],
        ] {
            git.run_or_bail(&args, Some(&repo_path)).unwrap();
        }
        let body: String = (0..400)
            .map(|n| format!("line {n} of a file long enough to be worth a delta\n"))
            .collect();
        let commit = |text: &str, message: &str| {
            std::fs::write(repo_path.join("big.txt"), text).unwrap();
            git.run_or_bail(&["add", "big.txt"], Some(&repo_path))
                .unwrap();
            git.run_or_bail(&["commit", "-qm", message], Some(&repo_path))
                .unwrap();
            git.run(&["rev-parse", "HEAD"], Some(&repo_path))
                .unwrap()
                .stdout_str()
                .trim()
                .to_string()
        };
        let older = commit(&body, "older");
        let newer = commit(&body.replace("line 200 ", "line 200 changed "), "newer");
        (dir, repo_path, older, newer)
    }

    /// Fetch `want` with `have` over v2, and return the band-1 pack bytes.
    async fn v2_fetch_pack(repo_path: &Path, args: &[&str]) -> Vec<u8> {
        use std::io::Cursor;
        let mut request = pkt_bytes(b"command=fetch\n");
        request.extend_from_slice(b"0001");
        for arg in args {
            request.extend_from_slice(&pkt_bytes(format!("{arg}\n").as_bytes()));
        }
        request.extend_from_slice(&pkt_bytes(b"done\n"));
        request.extend_from_slice(b"0000");

        let mut response = Vec::new();
        handle_v2_http(repo_path, Cursor::new(request), &mut response)
            .await
            .expect("v2 fetch succeeded");

        let mut reader = Cursor::new(response);
        let mut pack = Vec::new();
        loop {
            match read_pkt_line(&mut reader).await.unwrap() {
                PktLine::Data(payload) if payload.first() == Some(&1) => {
                    pack.extend_from_slice(&payload[1..]);
                }
                PktLine::Data(_) | PktLine::Delim => {}
                PktLine::Flush | PktLine::ResponseEnd => break,
            }
        }
        assert!(pack.starts_with(b"PACK"), "no pack in the response");
        pack
    }

    /// Run `git index-pack --strict` without `--fix-thin` on `pack`, inside the
    /// repository the client is fetching into — here the served one, which
    /// has every base. `--strict` wants the commit's parent to exist; only
    /// `--fix-thin` would let a delta borrow its base from the repository, so
    /// a thin pack fails on its unresolved deltas.
    fn index_pack(repo_path: &Path, pack: &[u8]) -> crate::cli_gateway::GitOutput {
        let dir = tempfile::tempdir().unwrap();
        let pack_path = dir.path().join("fetched.pack");
        std::fs::write(&pack_path, pack).unwrap();
        crate::cli_gateway::GitCommandGateway::new()
            .unwrap()
            .run(
                &["index-pack", "--strict", pack_path.to_str().unwrap()],
                Some(repo_path),
            )
            .unwrap()
    }

    /// A partial clone fetches the blobs its filter left out by id, through no
    /// advertisement. One the public history reaches is served; one only a
    /// server-private ref reaches is not (card_ad83ad72d14a).
    #[tokio::test]
    async fn a_partial_clone_gets_public_blobs_by_id_and_never_private_ones() {
        if crate::cli_gateway::global_gateway().is_err() {
            eprintln!("skipping v2 blob-want test: git not available");
            return;
        }
        let (_dir, repo_path, older, _newer) = repository_with_a_deltifiable_change();
        let git = crate::cli_gateway::GitCommandGateway::new().unwrap();
        let rev = |spec: &str| {
            git.run(&["rev-parse", spec], Some(&repo_path))
                .unwrap()
                .stdout_str()
                .trim()
                .to_string()
        };
        let public_blob = rev(&format!("{older}:big.txt"));

        // A commit only `refs/forks/x` holds, carrying a blob nothing else has.
        let private_blob = {
            std::fs::write(repo_path.join("secret.txt"), "fork-only content\n").unwrap();
            git.run_or_bail(&["add", "secret.txt"], Some(&repo_path))
                .unwrap();
            git.run_or_bail(&["commit", "-qm", "fork"], Some(&repo_path))
                .unwrap();
            let fork = rev("HEAD");
            git.run_or_bail(&["update-ref", "refs/forks/x", &fork], Some(&repo_path))
                .unwrap();
            git.run_or_bail(&["reset", "-q", "--hard", "HEAD~1"], Some(&repo_path))
                .unwrap();
            rev("refs/forks/x:secret.txt")
        };

        let pack = v2_fetch_pack(&repo_path, &[&format!("want {public_blob}")]).await;
        assert!(pack.len() > 32, "the public blob is packed");

        let mut request = pkt_bytes(b"command=fetch\n");
        request.extend_from_slice(b"0001");
        request.extend_from_slice(&pkt_bytes(format!("want {private_blob}\n").as_bytes()));
        request.extend_from_slice(&pkt_bytes(b"done\n"));
        request.extend_from_slice(b"0000");
        let mut response = Vec::new();
        let refused =
            handle_v2_http(&repo_path, std::io::Cursor::new(request), &mut response).await;
        let error = refused.expect_err("a blob only refs/forks reaches must not be served");
        assert!(format!("{error:#}").contains("not our ref"), "{error:#}");
        // The client hears why, as over v0/v1, and the transports can tell the
        // refusal from a failure of ours (card_bd1b7010d482).
        assert!(super::super::client_refusal(&error).is_some(), "{error:#}");
        assert_eq!(
            response,
            pkt_bytes(format!("ERR upload-pack: not our ref {private_blob}\n").as_bytes())
        );
    }

    /// card_54503ff49d6b: a v2 client that did not send `thin-pack` gets a
    /// self-contained pack; one that did gets a thin one.
    #[tokio::test]
    async fn fetch_sends_a_thin_pack_only_when_the_client_asks_for_one() {
        if crate::cli_gateway::global_gateway().is_err() {
            eprintln!("skipping v2 thin-pack test: git not available");
            return;
        }
        let (_dir, repo_path, older, newer) = repository_with_a_deltifiable_change();
        let want = format!("want {newer}");
        let have = format!("have {older}");

        let full = v2_fetch_pack(&repo_path, &[&want, &have]).await;
        let indexed = index_pack(&repo_path, &full);
        assert!(
            indexed.status.success(),
            "a pack the client did not ask to be thin must resolve on its own: {}",
            indexed.stderr_str()
        );

        let thin = v2_fetch_pack(&repo_path, &["thin-pack", &want, &have]).await;
        let indexed = index_pack(&repo_path, &thin);
        assert!(
            !indexed.status.success(),
            "a requested thin pack must delta against the client's base"
        );
        assert!(
            thin.len() < full.len(),
            "the thin pack must be the smaller one"
        );
    }

    #[tokio::test]
    async fn read_command_request_empty_flush_yields_flush() {
        use std::io::Cursor;
        let mut reader = Cursor::new(Vec::from(b"0000".as_slice()));
        assert!(matches!(
            read_command_request(&mut reader).await.unwrap(),
            CommandRequest::Flush
        ));
    }

    #[tokio::test]
    async fn read_command_request_errors_on_garbage_header() {
        // A non-hex pkt-line header on the negotiation stream must error, not panic.
        use std::io::Cursor;
        let mut reader = Cursor::new(Vec::from(b"zzzz".as_slice()));
        assert!(read_command_request(&mut reader).await.is_err());
    }

    #[tokio::test]
    async fn read_command_request_errors_on_non_numeric_deepen() {
        // A hostile `deepen <non-number>` arg must be rejected as Err (the
        // parse().context path), never unwrapped into a panic.
        use std::io::Cursor;
        let mut buf = Vec::new();
        buf.extend_from_slice(&pkt_bytes(b"command=fetch\n"));
        buf.extend_from_slice(b"0001");
        buf.extend_from_slice(&pkt_bytes(b"deepen notanumber\n"));
        buf.extend_from_slice(b"0000");

        let mut reader = Cursor::new(buf);
        assert!(read_command_request(&mut reader).await.is_err());
    }

    #[tokio::test]
    async fn command_frames_refuse_entries_past_the_ceiling() {
        use std::io::Cursor;
        let mut buf = pkt_bytes(b"command=fetch\n");
        buf.extend_from_slice(&pkt_bytes(b"agent=git/2.40\n"));
        let mut reader = Cursor::new(buf);

        let error = read_command_frames_with_limits(&mut reader, 1, 1024)
            .await
            .expect_err("the second retained frame must be refused");

        assert!(error.to_string().contains("entry limit"), "{error:#}");
    }

    #[tokio::test]
    async fn command_frames_refuse_wire_bytes_past_the_ceiling() {
        use std::io::Cursor;
        let mut reader = Cursor::new(pkt_bytes(b"command=fetch\n"));

        let error = read_command_frames_with_limits(&mut reader, 10, 4)
            .await
            .expect_err("a frame above the byte ceiling must be refused");

        assert!(error.to_string().contains("byte limit"), "{error:#}");
    }
}
