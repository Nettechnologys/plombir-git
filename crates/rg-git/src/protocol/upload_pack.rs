//! Git upload-pack protocol implementation (git clone/fetch).
//!
//! Supports two modes:
//! 1. Split reader/writer (HTTP mode) — via `handle_upload_pack`
//! 2. Single bidirectional stream (SSH mode) — via `handle_upload_pack_stream`

use anyhow::{bail, Context, Result};
use std::path::Path;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tracing;

use crate::pkt_line::{read_pkt_line, write_flush, write_pkt_line, PktLine};
use crate::sideband;

/// Handle upload-pack with separate reader and writer (HTTP mode).
/// This sends the ref advertisement, negotiates, and sends the packfile.
pub async fn handle_upload_pack<R, W>(repo_path: &Path, reader: R, writer: W) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = reader;
    let mut writer = writer;
    upload_pack_refs_and_negotiate(repo_path, &mut reader, &mut writer).await
}

/// Handle upload-pack with a single bidirectional stream (SSH mode).
/// Takes a mutable reference so the caller can send exit-status before dropping the stream.
pub async fn handle_upload_pack_stream<S>(repo_path: &Path, stream: &mut S) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    upload_pack_stream_impl(repo_path, stream).await
}

/// Handle upload-pack for HTTP mode where ref advertisement is already sent.
pub async fn handle_upload_pack_http<R, W>(repo_path: &Path, reader: R, writer: W) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut writer = writer;

    // Read client request and negotiate
    let (wants, haves, client_caps) = read_want_have_split(&mut reader).await?;

    if wants.is_empty() {
        write_flush(&mut writer).await?;
        return Ok(());
    }

    // Send NAK
    write_pkt_line(&mut writer, &PktLine::data(b"NAK")).await?;

    // Send packfile
    let use_sideband = client_caps.contains(&"side-band-64k".to_string())
        || client_caps.contains(&"side-band".to_string());
    send_packfile(repo_path, &wants, &haves, &mut writer, use_sideband).await
}

/// Internal: send ref advertisement and negotiate with separate reader/writer.
async fn upload_pack_refs_and_negotiate<R, W>(
    repo_path: &Path,
    reader: &mut R,
    writer: &mut W,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let refs = list_refs(repo_path)?;
    let head_sha = crate::resolve_head_sha(repo_path);
    let ref_list = build_ref_advertisement_vec(refs, head_sha);

    // Send ref advertisement
    let ad = build_ref_advertisement(&ref_list, "git-upload-pack");
    for pkt in &ad {
        write_pkt_line(writer, pkt).await?;
    }
    write_flush(writer).await?;

    // Read client negotiation
    let (wants, haves, client_caps) = read_want_have_split(&mut BufReader::new(reader)).await?;

    if wants.is_empty() {
        write_flush(writer).await?;
        return Ok(());
    }

    // Send NAK
    write_pkt_line(writer, &PktLine::data(b"NAK")).await?;

    // Send packfile
    let use_sideband = client_caps.contains(&"side-band-64k".to_string())
        || client_caps.contains(&"side-band".to_string());
    send_packfile(repo_path, &wants, &haves, writer, use_sideband).await
}

/// Internal: SSH mode implementation with single stream type.
async fn upload_pack_stream_impl<S>(repo_path: &Path, stream: &mut S) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let refs = list_refs(repo_path)?;
    let head_sha = crate::resolve_head_sha(repo_path);
    let ref_list = build_ref_advertisement_vec(refs, head_sha);

    // Send ref advertisement
    let ad = build_ref_advertisement(&ref_list, "git-upload-pack");
    for pkt in &ad {
        write_pkt_line(stream, pkt).await?;
    }
    write_flush(stream).await?;

    // Negotiation + packfile (single stream type)
    negotiate_and_send_pack_single(repo_path, stream).await
}

/// Internal: negotiate and send pack with single stream type (SSH mode).
async fn negotiate_and_send_pack_single<S>(repo_path: &Path, stream: &mut S) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (wants, haves, client_caps) = read_want_have_stream(stream).await?;

    let use_sideband = client_caps.contains(&"side-band-64k".to_string())
        || client_caps.contains(&"side-band".to_string());

    if wants.is_empty() {
        stream.flush().await?;
        return Ok(());
    }

    write_pkt_line(stream, &PktLine::data(b"NAK")).await?;
    send_packfile(repo_path, &wants, &haves, stream, use_sideband).await
}

/// Read want/have lines from separate reader (HTTP mode).
async fn read_want_have_split<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<(Vec<String>, Vec<String>, Vec<String>)> {
    read_want_have_impl(reader).await
}

/// Read want/have lines from single stream (SSH mode).
/// Wraps stream in a BufReader temporarily, then passes it straight to impl.
async fn read_want_have_stream<S: AsyncRead + Unpin>(
    stream: &mut S,
) -> Result<(Vec<String>, Vec<String>, Vec<String>)> {
    // Safety: BufReader here only pre-reads the negotiation phase.
    // After returning, any remaining bytes in the BufReader internal buffer
    // would be lost, but for pkt-line protocol each read_pkt_line consumes
    // exactly the announced bytes, so there should be no unconsumed buffered data.
    let mut reader = BufReader::new(stream);
    read_want_have_impl(&mut reader).await
}

/// Internal: parse want/have negotiation from a BufReader using proper pkt-line parsing.
///
/// Each pkt-line on the wire is:
///   `<4-hex-length><payload>`
/// where the 4-byte length includes itself. `read_pkt_line` handles this and
/// returns only the payload bytes (or `PktLine::Flush` for "0000").
async fn read_want_have_impl<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<(Vec<String>, Vec<String>, Vec<String>)> {
    let mut wants = Vec::new();
    let mut haves = Vec::new();
    let mut capabilities = Vec::new();

    loop {
        let pkt = read_pkt_line(reader).await?;

        // Flush packet ("0000") or EOF → end of negotiation
        // Delim/ResponseEnd are V2-only and shouldn't appear in V1 protocol
        let raw = match pkt {
            PktLine::Flush => break,
            PktLine::Data(bytes) => bytes,
            PktLine::Delim | PktLine::ResponseEnd => continue, // Skip in V1 context
        };

        // Convert bytes to string (pkt-line payload, no length prefix)
        let line = String::from_utf8_lossy(&raw);
        let line = line.trim_end_matches('\n');

        if line.is_empty() {
            continue;
        }

        // Git want/have lines come in two forms:
        //
        //   Form A (first want line, v1 protocol):
        //     `want <sha1>\0<cap1> <cap2> ...`
        //     NUL separates sha+command from capability list.
        //
        //   Form B (git client sends capabilities space-separated after the SHA,
        //     without a NUL, when the server did NOT advertise them with NUL):
        //     `want <sha1> <cap1> <cap2> ...`
        //
        // In practice the macOS git client sends Form B (space-separated after sha).
        // We handle both by first checking for NUL, then splitting on the second space
        // for commands that start with "want " or "have ".

        let (command, caps_part): (String, Option<&str>) = if line.contains('\0') {
            // Form A: NUL-separated capabilities
            let mut parts = line.splitn(2, '\0');
            let cmd = parts.next().unwrap_or("");
            let caps = parts.next().unwrap_or("");
            (
                cmd.to_string(),
                if caps.is_empty() { None } else { Some(caps) },
            )
        } else if let Some(after_want) = line.strip_prefix("want ") {
            // Form B: `want <sha1> [cap1 cap2 ...]` — space after sha1
            if let Some((sha, caps)) = after_want.split_once(' ') {
                let command = format!("want {sha}");
                (command, if caps.is_empty() { None } else { Some(caps) })
            } else {
                (line.to_string(), None)
            }
        } else {
            (line.to_string(), None)
        };

        if let Some(caps) = caps_part {
            // Parse space-separated capabilities
            capabilities = caps
                .split([' ', '\0'])
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
                .collect();
            tracing::debug!(caps = ?capabilities, "Parsed client capabilities");
        }

        if let Some(sha) = command.strip_prefix("want ") {
            let sha = sha.trim().to_string();
            tracing::debug!(sha = %sha, "Client wants");
            wants.push(sha);
        } else if let Some(sha) = command.strip_prefix("have ") {
            let sha = sha.trim().to_string();
            haves.push(sha);
        } else if command == "done" {
            break;
        } else {
            tracing::debug!(line = %command, "Unknown want/have line, ignoring");
        }
    }

    tracing::info!(
        wants = wants.len(),
        haves = haves.len(),
        caps = capabilities.len(),
        "Want/have negotiation complete"
    );

    Ok((wants, haves, capabilities))
}

/// List refs in a bare git repository using gix API.
fn list_refs(repo_path: &Path) -> Result<Vec<(String, String)>> {
    let repo = gix::open(repo_path).context("failed to open repository")?;
    let mut refs = Vec::new();

    let references = repo.references().context("failed to list references")?;
    let all_refs = references.all()?;

    for reference in all_refs {
        let reference = match reference {
            Ok(r) => r,
            Err(_) => continue,
        };
        let refname = reference.name().as_bstr().to_string();
        let target = reference.target();

        match target {
            gix::refs::TargetRef::Object(id) => {
                refs.push((id.to_string(), refname));
            }
            gix::refs::TargetRef::Symbolic(_) => {
                // For symbolic refs like HEAD, try to resolve to the actual object
                if refname == "HEAD" {
                    if let Ok(head_id) = repo.head_id() {
                        refs.push((head_id.to_string(), refname));
                    }
                }
            }
        }
    }

    Ok(refs)
}

/// Build ref advertisement from ref list.
fn build_ref_advertisement_vec(
    refs: Vec<(String, String)>,
    head_sha: Option<String>,
) -> Vec<(String, String)> {
    let mut ref_list = Vec::new();

    // Add HEAD first if we have it
    if let Some(sha) = &head_sha {
        ref_list.push((sha.clone(), "HEAD".to_string()));
    }

    // Add all refs
    for (sha, refname) in refs {
        ref_list.push((sha, refname));
    }

    ref_list
}

/// Build ref advertisement pkt-lines.
fn build_ref_advertisement(ref_list: &[(String, String)], service: &str) -> Vec<PktLine> {
    let mut lines = Vec::new();

    // First line includes service announcement and capabilities
    if let Some((sha, refname)) = ref_list.first() {
        // Capabilities: advertise only what we implement.
        // - side-band-64k: packfile in sideband channel 1, messages in channel 2
        // - ofs-delta: server can send OFS_DELTA objects (smaller packs)
        // - agent: server identification
        // NOTE: We do NOT advertise multi_ack / multi_ack_detailed / no-done because
        // our negotiation loop only handles the simple NAK→packfile flow.
        let caps = "side-band-64k ofs-delta agent=ironforge/0.1";
        let line = format!("{} {}\0{}", sha, refname, caps);
        lines.push(PktLine::Data(line.into_bytes()));
    } else {
        // Empty repo — still need capabilities
        let caps = "side-band-64k ofs-delta agent=ironforge/0.1";
        let line = format!(
            "0000000000000000000000000000000000000000 capabilities^{}\0{}",
            service, caps
        );
        lines.push(PktLine::Data(line.into_bytes()));
    }

    // Remaining refs
    for (sha, refname) in ref_list.iter().skip(1) {
        let line = format!("{} {}", sha, refname);
        lines.push(PktLine::Data(line.into_bytes()));
    }

    lines
}

/// Generate and send the packfile.
/// TODO(gix): Replace with gix pack generation when available.
/// Currently using git pack-objects CLI as gix doesn't have a direct replacement.
async fn send_packfile<W: AsyncWrite + Unpin>(
    repo_path: &Path,
    wants: &[String],
    _haves: &[String],
    writer: &mut W,
    use_sideband: bool,
) -> Result<()> {
    // Use git pack-objects to generate the packfile via gateway
    let mut cmd = crate::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?
        .spawn_async(&["pack-objects", "--all", "--stdout"], Some(repo_path))
        .await
        .context("failed to spawn git pack-objects")?;

    // Close stdin immediately — `--all` packs all objects without stdin input,
    // but the piped stdin keeps the child waiting for EOF if we don't close it.
    // 踩坑: Stdio::piped() creates a pipe but git pack-objects blocks reading stdin
    // until EOF; must take and drop stdin to signal EOF.
    {
        let stdin = cmd.stdin.take();
        drop(stdin); // Close stdin pipe → child sees EOF
    }

    let stdout = cmd.stdout.take().context("no stdout")?;
    let mut pack_reader = BufReader::new(stdout);

    let mut pack_data = Vec::new();
    pack_reader
        .read_to_end(&mut pack_data)
        .await
        .context("failed to read packfile")?;

    let status = cmd.wait().await?;
    if !status.success() {
        let stderr = cmd.stderr.take();
        if let Some(mut stderr) = stderr {
            let mut err_msg = Vec::new();
            stderr.read_to_end(&mut err_msg).await?;
            bail!(
                "git pack-objects failed: {}",
                String::from_utf8_lossy(&err_msg)
            );
        }
        bail!("git pack-objects failed with status {}", status);
    }

    let pack_size = pack_data.len();
    tracing::info!(pack_size, "Packfile generated successfully");

    if use_sideband {
        // Send packfile data through sideband-64k (band 1)
        sideband::write_sideband_data(writer, &pack_data).await?;

        // Send "Done." progress message (band 2)
        sideband::write_sideband_progress(writer, "Done.\n").await?;

        // Send flush to end sideband
        sideband::write_sideband_flush(writer).await?;
    } else {
        // Send raw packfile without sideband
        writer.write_all(&pack_data).await?;
        writer.flush().await?;
    }

    tracing::info!(pack_size, objects = wants.len(), "Upload-pack complete");

    Ok(())
}
