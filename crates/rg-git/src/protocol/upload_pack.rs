//! Git upload-pack protocol implementation (git clone/fetch).
//!
//! Supports split reader/writer after the HTTP advertisement and a single
//! bidirectional SSH stream.

use anyhow::{bail, Context, Result};
use std::path::Path;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tracing;

use super::pack_stream;
use crate::pkt_line::{read_pkt_line, write_flush, write_pkt_line, PktLine};
use crate::sideband;

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

/// Internal: SSH mode implementation with single stream type.
async fn upload_pack_stream_impl<S>(repo_path: &Path, stream: &mut S) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let advertisement = crate::ref_advertisement::collect(repo_path)?;
    let ref_list = build_ref_advertisement_vec(advertisement.refs, advertisement.head_oid);

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
    read_want_have_impl_with_limits(
        reader,
        super::MAX_NEGOTIATION_ENTRIES,
        super::MAX_NEGOTIATION_INPUT_BYTES,
    )
    .await
}

async fn read_want_have_impl_with_limits<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
    max_entries: usize,
    max_bytes: usize,
) -> Result<(Vec<String>, Vec<String>, Vec<String>)> {
    let mut wants = Vec::new();
    let mut haves = Vec::new();
    let mut capabilities = Vec::new();
    let mut input_bytes = 0_usize;

    loop {
        let pkt = read_pkt_line(reader).await?;

        // Flush packet ("0000") or EOF → end of negotiation
        // Delim/ResponseEnd are V2-only and shouldn't appear in V1 protocol
        let raw = match pkt {
            PktLine::Flush => break,
            PktLine::Data(bytes) => {
                input_bytes = input_bytes
                    .checked_add(bytes.len())
                    .filter(|size| *size <= max_bytes)
                    .context("upload-pack negotiation exceeds the configured byte limit")?;
                bytes
            }
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
            if wants.len() + haves.len() >= max_entries {
                bail!("upload-pack negotiation exceeds the configured entry limit");
            }
            let sha = sha.trim().to_string();
            tracing::debug!(sha = %sha, "Client wants");
            wants.push(sha);
        } else if let Some(sha) = command.strip_prefix("have ") {
            if wants.len() + haves.len() >= max_entries {
                bail!("upload-pack negotiation exceeds the configured entry limit");
            }
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
        let caps = "side-band-64k ofs-delta agent=plombir-git/0.1";
        let line = format!("{} {}\0{}", sha, refname, caps);
        lines.push(PktLine::Data(line.into_bytes()));
    } else {
        // Empty repo — still need capabilities
        let caps = "side-band-64k ofs-delta agent=plombir-git/0.1";
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
///
/// The pack is forwarded to `writer` as `git pack-objects` produces it — see
/// [`crate::protocol::pack_stream`] for why it is never collected first. The
/// only trailing work is the band-2 "Done." and the sideband flush, both of
/// which are reached solely on a clean exit: a failed generation returns `Err`
/// from the stream instead, having announced itself on band 3.
///
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
    // Pitfall: Stdio::piped() creates a pipe but git pack-objects blocks reading stdin
    // until EOF; must take and drop stdin to signal EOF.
    {
        let stdin = cmd.stdin.take();
        drop(stdin); // Close stdin pipe → child sees EOF
    }

    let pack_size = pack_stream::stream_pack_objects(cmd, writer, use_sideband).await?;

    if use_sideband {
        // Send "Done." progress message (band 2)
        sideband::write_sideband_progress(writer, "Done.\n").await?;

        // Send flush to end sideband
        sideband::write_sideband_flush(writer).await?;
    }

    tracing::info!(pack_size, objects = wants.len(), "Upload-pack complete");

    Ok(())
}

#[cfg(test)]
mod ref_advertisement_tests {
    use std::io::Cursor;
    use std::path::Path;

    use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};

    fn pkt(payload: &str) -> Vec<u8> {
        let mut encoded = format!("{:04x}", payload.len() + 4).into_bytes();
        encoded.extend_from_slice(payload.as_bytes());
        encoded
    }

    async fn run_live_upload_pack(repo_path: &Path) -> (anyhow::Result<()>, Vec<u8>) {
        let (mut client, mut server) = tokio::io::duplex(64 * 1024);
        client.shutdown().await.unwrap();
        let repo_path = repo_path.to_path_buf();
        let handler =
            tokio::spawn(
                async move { super::handle_upload_pack_stream(&repo_path, &mut server).await },
            );

        let mut output = Vec::new();
        client.read_to_end(&mut output).await.unwrap();
        (handler.await.unwrap(), output)
    }

    #[tokio::test]
    async fn upload_pack_advertises_an_unborn_repository_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("unborn.git");
        gix::init_bare(&repo_path).unwrap();
        let (result, output) = run_live_upload_pack(&repo_path).await;
        result.unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output
            .contains("0000000000000000000000000000000000000000 capabilities^git-upload-pack"));
    }

    #[tokio::test]
    async fn upload_pack_rejects_an_unreadable_ref_before_advertising() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("broken-ref.git");
        gix::init_bare(&repo_path).unwrap();
        std::fs::create_dir_all(repo_path.join("refs/heads")).unwrap();
        std::fs::write(repo_path.join("refs/heads/broken"), "not-an-object-id\n").unwrap();
        let (result, output) = run_live_upload_pack(&repo_path).await;
        let error = result.unwrap_err();

        assert!(output.is_empty(), "no partial advertisement may be written");
        assert!(
            format!("{error:#}").contains("failed to read a reference"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn want_have_parser_refuses_entries_past_its_ceiling() {
        let mut input = pkt(&format!("want {}\n", "a".repeat(40)));
        input.extend_from_slice(&pkt(&format!("have {}\n", "b".repeat(40))));
        input.extend_from_slice(b"0000");
        let mut reader = BufReader::new(Cursor::new(input));

        let error = super::read_want_have_impl_with_limits(&mut reader, 1, 1024)
            .await
            .expect_err("the second retained negotiation entry must be refused");

        assert!(error.to_string().contains("entry limit"), "{error:#}");
    }

    #[tokio::test]
    async fn want_have_parser_refuses_wire_bytes_past_its_ceiling() {
        let input = pkt(&format!("want {}\n", "a".repeat(40)));
        let mut reader = BufReader::new(Cursor::new(input));

        let error = super::read_want_have_impl_with_limits(&mut reader, 10, 8)
            .await
            .expect_err("a negotiation frame above the byte ceiling must be refused");

        assert!(error.to_string().contains("byte limit"), "{error:#}");
    }
}

/// Live streaming coverage for both upload-pack dialects.
///
/// Lives beside V1 rather than being split across the two protocol modules
/// because the claim is one claim about one mechanism: the pack leaves
/// `git pack-objects` in chunks (`super::pack_stream`) and reaches the client
/// whole, whichever dialect framed it (`card_73f02e2a97ad`).
#[cfg(test)]
mod pack_streaming_tests {
    use std::io::Cursor;
    use std::path::{Path, PathBuf};

    use tokio::io::{AsyncReadExt, BufReader};

    use crate::pkt_line::{read_pkt_line, PktLine};
    use crate::sideband::SIDEBAND_MAX;

    /// The window the HTTP transport puts between the protocol handler and the
    /// response stream. The pack these tests ask for is many times larger, so a
    /// handler that collected it first would have to hold the whole thing —
    /// and one that wrote it without a concurrent reader would deadlock here.
    const HTTP_WINDOW_BYTES: usize = 64 * 1024;

    fn git_ok(args: &[&str], cwd: &Path) -> String {
        let gateway = crate::cli_gateway::global_gateway()
            .as_ref()
            .expect("git gateway");
        let out = gateway.run(args, Some(cwd)).expect("run git");
        out.ensure_success().expect("git succeeded");
        out.stdout_str().trim().to_string()
    }

    fn pkt(payload: &str) -> Vec<u8> {
        let mut encoded = format!("{:04x}", payload.len() + 4).into_bytes();
        encoded.extend_from_slice(payload.as_bytes());
        encoded
    }

    /// Bytes zlib cannot shrink, so the pack really is as large as the blob and
    /// the test is not silently exercising a 200-byte transfer.
    fn incompressible(len: usize) -> Vec<u8> {
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect()
    }

    /// A bare repository holding one commit far larger than every window on the
    /// path, plus its HEAD.
    fn seed_large_repo(tmp: &Path) -> (PathBuf, String) {
        let work = tmp.join("work");
        std::fs::create_dir_all(&work).unwrap();
        git_ok(&["init", "-q", "--initial-branch=main", "."], &work);
        git_ok(&["config", "user.email", "pack@example.com"], &work);
        git_ok(&["config", "user.name", "Pack Streaming"], &work);
        std::fs::write(work.join("payload.bin"), incompressible(768 * 1024)).unwrap();
        git_ok(&["add", "."], &work);
        git_ok(&["commit", "-q", "-m", "large"], &work);
        let head = git_ok(&["rev-parse", "HEAD"], &work);

        let bare = tmp.join("serve.git");
        git_ok(
            &[
                "clone",
                "-q",
                "--bare",
                work.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
            tmp,
        );
        (bare, head)
    }

    /// What the client sees once the sideband is unwrapped.
    struct SidebandResponse {
        pack: Vec<u8>,
        /// Payload size of each band-1 packet, in order.
        data_payloads: Vec<usize>,
        saw_done: bool,
    }

    /// Read pkt-lines until the sideband flush, splitting band 1 from band 2.
    ///
    /// `expect_text` is the plain (non-sideband) header the dialect writes
    /// first — `NAK` for V1, `packfile` for V2 — and is asserted rather than
    /// skipped, so a response that lost its framing cannot pass as a pack.
    async fn read_sideband_response(response: Vec<u8>, expect_text: &str) -> SidebandResponse {
        let mut reader = BufReader::new(Cursor::new(response));
        match read_pkt_line(&mut reader).await.unwrap() {
            PktLine::Data(line) => assert_eq!(
                String::from_utf8_lossy(&line).trim_end(),
                expect_text,
                "unexpected response header"
            ),
            other => panic!("expected the {expect_text} header, got {other:?}"),
        }

        let mut out = SidebandResponse {
            pack: Vec::new(),
            data_payloads: Vec::new(),
            saw_done: false,
        };
        loop {
            match read_pkt_line(&mut reader).await.unwrap() {
                PktLine::Data(payload) => match payload[0] {
                    1 => {
                        out.data_payloads.push(payload.len() - 1);
                        out.pack.extend_from_slice(&payload[1..]);
                    }
                    2 => out.saw_done |= payload[1..].starts_with(b"Done."),
                    band => panic!("unexpected sideband {band}"),
                },
                PktLine::Flush => break,
                other => panic!("unexpected pkt-line {other:?}"),
            }
        }
        out
    }

    /// The pack arrived whole, and the chunked read did not change the wire.
    ///
    /// `git index-pack` verifies the object count and the trailing checksum,
    /// which a short transfer cannot satisfy — that is what proves a streamed
    /// pack is not a truncated one. The framing assertion is the second half:
    /// every band-1 packet but the last carries a full `SIDEBAND_MAX` payload,
    /// the same shape a single `write_sideband_data` over a complete buffer
    /// produced, rather than following however the kernel split the pipe.
    fn assert_pack_is_whole(response: &SidebandResponse, bare: &Path, tmp: &Path, label: &str) {
        assert!(
            response.saw_done,
            "{label}: the pack must be followed by Done."
        );
        assert!(
            response.pack.len() > HTTP_WINDOW_BYTES * 4,
            "{label}: the test pack must dwarf the transport windows, got {} bytes",
            response.pack.len()
        );
        assert!(
            response.data_payloads.len() > 1,
            "{label}: a pack this size must arrive in several band-1 packets"
        );
        let (last, full) = response.data_payloads.split_last().unwrap();
        assert!(
            full.iter().all(|len| *len == SIDEBAND_MAX),
            "{label}: every band-1 packet but the last must be full: {full:?}"
        );
        assert!(*last <= SIDEBAND_MAX);

        let pack_path = tmp.join(format!("{label}.pack"));
        std::fs::write(&pack_path, &response.pack).unwrap();
        git_ok(
            &["index-pack", "--strict", pack_path.to_str().unwrap()],
            bare,
        );
    }

    #[tokio::test]
    async fn v1_streams_a_pack_larger_than_every_internal_window() {
        if crate::cli_gateway::global_gateway().is_err() {
            eprintln!("skipping V1 large-pack streaming test: git not available");
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let (bare, head) = seed_large_repo(tmp.path());

        let mut request = pkt(&format!("want {head}\0side-band-64k\n"));
        request.extend_from_slice(b"0000");
        request.extend_from_slice(&pkt("done\n"));

        // The bounded duplex is the point: the handler can only make progress
        // because the response is drained concurrently, chunk by chunk.
        let (mut client, mut server) = tokio::io::duplex(HTTP_WINDOW_BYTES);
        let repo_path = bare.clone();
        let handler = tokio::spawn(async move {
            super::handle_upload_pack_http(&repo_path, Cursor::new(request), &mut server).await
        });

        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        handler.await.unwrap().expect("upload-pack succeeded");

        let response = read_sideband_response(response, "NAK").await;
        assert_pack_is_whole(&response, &bare, tmp.path(), "v1");
    }

    #[tokio::test]
    async fn v2_fetch_streams_a_pack_larger_than_every_internal_window() {
        if crate::cli_gateway::global_gateway().is_err() {
            eprintln!("skipping V2 large-pack streaming test: git not available");
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let (bare, head) = seed_large_repo(tmp.path());

        let mut request = pkt("command=fetch\n");
        request.extend_from_slice(&pkt("object-format=sha1\n"));
        request.extend_from_slice(b"0001");
        request.extend_from_slice(&pkt(&format!("want {head}\n")));
        request.extend_from_slice(&pkt("done\n"));
        request.extend_from_slice(b"0000");

        let (mut client, mut server) = tokio::io::duplex(HTTP_WINDOW_BYTES);
        let repo_path = bare.clone();
        let handler = tokio::spawn(async move {
            crate::protocol::v2::handle_v2_http(&repo_path, Cursor::new(request), &mut server).await
        });

        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        handler.await.unwrap().expect("v2 fetch succeeded");

        let response = read_sideband_response(response, "packfile").await;
        assert_pack_is_whole(&response, &bare, tmp.path(), "v2");
    }
}
