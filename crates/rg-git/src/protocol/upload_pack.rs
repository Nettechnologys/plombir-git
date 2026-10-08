//! Git upload-pack protocol implementation (git clone/fetch).
//!
//! Supports split reader/writer after the HTTP advertisement and a single
//! bidirectional SSH stream.

use anyhow::{bail, Context, Result};
use std::path::Path;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tracing;

use super::{pack_stream, ClientRefusal};
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
///
/// Stateless: one request carries the wants and the negotiation so far, and
/// ends either with `done` — answered with the pack — or with a flush, which
/// is answered with acknowledgments alone; the client then sends the next
/// request (see [`negotiate`]).
pub async fn handle_upload_pack_http<R, W>(repo_path: &Path, reader: R, writer: W) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut writer = writer;
    serve_fetch(repo_path, &mut reader, &mut writer, Transport::Stateless).await
}

/// Internal: SSH mode implementation with single stream type.
async fn upload_pack_stream_impl<S>(repo_path: &Path, stream: &mut S) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let advertisement = crate::ref_advertisement::collect_for_clients(repo_path)?;
    let ref_list = build_ref_advertisement_vec(advertisement.refs, advertisement.head_oid);

    // Send ref advertisement
    let ad = build_ref_advertisement(&ref_list, "git-upload-pack");
    for pkt in &ad {
        write_pkt_line(stream, pkt).await?;
    }
    write_flush(stream).await?;

    // The negotiation answers each round as it ends while the client keeps
    // writing, so it reads and writes the one stream at once. The buffered
    // reader lives for the whole exchange: whatever it has read ahead belongs
    // to the next round.
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    serve_fetch(repo_path, &mut reader, &mut write_half, Transport::Stateful).await
}

/// How the client's requests reach the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Transport {
    /// HTTP: every round is its own request, and a request that ends without
    /// `done` is answered with acknowledgments only.
    Stateless,
    /// SSH: one conversation; rounds follow each other on the same stream.
    Stateful,
}

/// The two acknowledgment dialects this server speaks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AckMode {
    /// No `multi_ack` capability: one `ACK` for the first common object, and a
    /// `NAK` at a round's end only while none has been found.
    Plain,
    /// `multi_ack_detailed`: `ACK <oid> common` for every common object and a
    /// `NAK` at the end of every round. A stateless client needs this one to
    /// carry the common objects into its final request; under the plain
    /// dialect that request names none and gets the whole repository.
    Detailed,
}

/// Read the wants, negotiate, and send the pack the negotiation agreed on.
/// A request refused on the client's account reaches it as an `ERR` packet.
async fn serve_fetch<R, W>(
    repo_path: &Path,
    reader: &mut BufReader<R>,
    writer: &mut W,
    transport: Transport,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let outcome = negotiate_and_send(repo_path, reader, writer, transport).await;
    super::tell_client_of_refusal(writer, outcome).await
}

async fn negotiate_and_send<R, W>(
    repo_path: &Path,
    reader: &mut BufReader<R>,
    writer: &mut W,
    transport: Transport,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (wants, client_caps) = read_wants(reader).await?;
    if wants.is_empty() {
        match transport {
            Transport::Stateless => write_flush(writer).await?,
            Transport::Stateful => writer.flush().await?,
        }
        return Ok(());
    }

    // A want the advertisement did not offer is refused before anything else
    // is read: packing it would hand out what the advertisement hides.
    let checked = {
        let (repo_path, wants) = (repo_path.to_path_buf(), wants.clone());
        tokio::task::spawn_blocking(move || {
            crate::ref_advertisement::unadvertised_want(&repo_path, &wants)
        })
        .await
        .context("upload-pack want check did not complete")??
    };
    if let Some(want) = checked {
        return Err(ClientRefusal::new(format!("upload-pack: not our ref {want}")).into());
    }

    let has = |cap: &str| client_caps.iter().any(|offered| offered == cap);
    let mode = if has("multi_ack_detailed") {
        AckMode::Detailed
    } else {
        AckMode::Plain
    };
    let Some(common) = negotiate(repo_path, reader, writer, transport, mode, wants.len()).await?
    else {
        writer.flush().await?;
        return Ok(());
    };

    let options = PackOptions {
        use_sideband: has("side-band-64k") || has("side-band"),
        thin: has("thin-pack"),
        ofs_delta: has("ofs-delta"),
    };
    send_packfile(repo_path, &wants, &common, writer, options).await
}

/// Read the `want` lines up to the flush that ends them, with the client's
/// capabilities from the first one.
async fn read_wants<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
) -> Result<(Vec<String>, Vec<String>)> {
    read_wants_with_limits(
        reader,
        super::MAX_NEGOTIATION_ENTRIES,
        super::MAX_NEGOTIATION_INPUT_BYTES,
    )
    .await
}

async fn read_wants_with_limits<R: AsyncRead + Unpin>(
    reader: &mut BufReader<R>,
    max_entries: usize,
    max_bytes: usize,
) -> Result<(Vec<String>, Vec<String>)> {
    let mut wants = Vec::new();
    let mut capabilities = Vec::new();
    let mut input_bytes = 0_usize;

    loop {
        let raw = match read_pkt_line(reader).await? {
            PktLine::Flush => break,
            PktLine::Data(bytes) => {
                input_bytes = checked_negotiation_bytes(input_bytes, bytes.len(), max_bytes)?;
                bytes
            }
            PktLine::Delim | PktLine::ResponseEnd => continue, // V2-only framing
        };
        let line = String::from_utf8_lossy(&raw);
        let line = line.trim_end_matches('\n');
        if line.is_empty() {
            continue;
        }

        // The first want carries the capability list, either after a NUL or,
        // from clients that saw them advertised without one, after a space:
        //   `want <sha1>\0<cap1> <cap2> ...`
        //   `want <sha1> <cap1> <cap2> ...`
        let (command, caps_part): (&str, Option<&str>) = match line.split_once('\0') {
            Some((command, caps)) => (command, Some(caps)),
            None => match line
                .strip_prefix("want ")
                .and_then(|rest| rest.split_once(' '))
            {
                Some((sha, caps)) => (&line[..5 + sha.len()], Some(caps)),
                None => (line, None),
            },
        };
        if let Some(caps) = caps_part.filter(|caps| !caps.is_empty()) {
            capabilities = caps
                .split([' ', '\0'])
                .filter(|cap| !cap.is_empty())
                .map(str::to_string)
                .collect();
            tracing::debug!(caps = ?capabilities, "Parsed client capabilities");
        }

        if let Some(sha) = command.strip_prefix("want ") {
            if wants.len() >= max_entries {
                return Err(ClientRefusal::new(ENTRY_LIMIT_REFUSAL).into());
            }
            wants.push(validated_oid(sha.trim())?);
        } else {
            // `shallow` / `deepen` are not advertised and not honoured.
            tracing::debug!(line = %command, "Unknown upload-pack request line, ignoring");
        }
    }

    tracing::info!(
        wants = wants.len(),
        caps = capabilities.len(),
        "Upload-pack wants read"
    );
    Ok((wants, capabilities))
}

/// How a round of `have` lines ended.
enum RoundEnd {
    Flush,
    Done,
}

/// Run the `have` / `ACK` / `NAK` exchange (card_ad83ad72d14a).
///
/// Returns the common objects to leave out of the pack once the client says
/// `done`, or `None` when a stateless request ended without it — that request
/// is answered with its acknowledgments and nothing else, and the client
/// sends the next one. Before this the haves were never read at all and every
/// fetch got `pack-objects --all`: the whole repository, objects of hidden
/// refs included, on every poll — and a stateless request that ended in a
/// flush got a pack where its client expected acknowledgments.
///
/// What each dialect writes is what git's own `upload-pack` writes (see
/// [`AckMode`]). The early `ACK <oid> ready` is never sent: it only lets a
/// client stop negotiating sooner, and every client falls back to `done`.
async fn negotiate<R, W>(
    repo_path: &Path,
    reader: &mut BufReader<R>,
    writer: &mut W,
    transport: Transport,
    mode: AckMode,
    wants: usize,
) -> Result<Option<Vec<String>>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut common: Vec<String> = Vec::new();
    let mut entries = wants;
    let mut input_bytes = 0_usize;

    loop {
        let mut round = Vec::new();
        let end = loop {
            match read_pkt_line(reader).await? {
                PktLine::Flush => break RoundEnd::Flush,
                PktLine::Data(bytes) => {
                    input_bytes = checked_negotiation_bytes(
                        input_bytes,
                        bytes.len(),
                        super::MAX_NEGOTIATION_INPUT_BYTES,
                    )?;
                    let line = String::from_utf8_lossy(&bytes);
                    let line = line.trim_end_matches('\n');
                    if line == "done" {
                        break RoundEnd::Done;
                    }
                    if let Some(sha) = line.strip_prefix("have ") {
                        entries += 1;
                        if entries > super::MAX_NEGOTIATION_ENTRIES {
                            return Err(ClientRefusal::new(ENTRY_LIMIT_REFUSAL).into());
                        }
                        round.push(validated_oid(sha.trim())?);
                    }
                }
                PktLine::Delim | PktLine::ResponseEnd => continue,
            }
        };

        // A stateful client flushes only after a round of haves; an empty one
        // is the end of its stream (`read_pkt_line` reads EOF as a flush), and
        // waiting for more would wait forever.
        if transport == Transport::Stateful && matches!(end, RoundEnd::Flush) && round.is_empty() {
            bail!("upload-pack client ended the negotiation without `done`");
        }

        for oid in present_objects(repo_path, round).await? {
            if common.contains(&oid) {
                continue;
            }
            match mode {
                AckMode::Detailed => {
                    write_pkt_line(writer, &PktLine::text(&format!("ACK {oid} common"))).await?;
                }
                AckMode::Plain if common.is_empty() => {
                    write_pkt_line(writer, &PktLine::text(&format!("ACK {oid}"))).await?;
                }
                AckMode::Plain => {}
            }
            common.push(oid);
        }

        match end {
            RoundEnd::Flush => {
                if mode == AckMode::Detailed || common.is_empty() {
                    write_pkt_line(writer, &PktLine::text("NAK")).await?;
                }
                if transport == Transport::Stateless {
                    return Ok(None);
                }
                writer.flush().await?;
            }
            RoundEnd::Done => {
                match (mode, common.last()) {
                    (_, None) => write_pkt_line(writer, &PktLine::text("NAK")).await?,
                    (AckMode::Detailed, Some(last)) => {
                        write_pkt_line(writer, &PktLine::text(&format!("ACK {last}"))).await?
                    }
                    // The plain dialect acknowledged its first common object
                    // the moment it saw it, and says nothing more.
                    (AckMode::Plain, Some(_)) => {}
                }
                return Ok(Some(common));
            }
        }
    }
}

/// The haves of one round that this repository holds, in the client's order.
async fn present_objects(repo_path: &Path, haves: Vec<String>) -> Result<Vec<String>> {
    if haves.is_empty() {
        return Ok(haves);
    }
    let repo_path = repo_path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let repo = crate::repository::open(&repo_path)
            .context("failed to open repository to negotiate a fetch")?;
        let mut present = Vec::with_capacity(haves.len());
        for have in haves {
            let id = gix::ObjectId::from_hex(have.as_bytes())
                .context("upload-pack have is not an object id")?;
            // Three-valued on purpose: an object store that cannot answer is a
            // failure, not an object the client has and we lack.
            if repo
                .try_find_header(id)
                .with_context(|| format!("failed to look up have {have}"))?
                .is_some()
            {
                present.push(have);
            }
        }
        Ok(present)
    })
    .await
    .context("upload-pack negotiation lookup did not complete")?
}

const ENTRY_LIMIT_REFUSAL: &str = "upload-pack negotiation exceeds the configured entry limit";

fn checked_negotiation_bytes(current: usize, frame: usize, max_bytes: usize) -> Result<usize> {
    current
        .checked_add(frame)
        .filter(|size| *size <= max_bytes)
        .ok_or_else(|| {
            ClientRefusal::new("upload-pack negotiation exceeds the configured byte limit").into()
        })
}

/// A want or have names one object by its full SHA-1. Anything else would
/// reach `pack-objects` as a revision expression.
fn validated_oid(sha: &str) -> Result<String> {
    if sha.len() != 40 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(
            ClientRefusal::new("upload-pack object id must be 40 hexadecimal characters").into(),
        );
    }
    Ok(sha.to_ascii_lowercase())
}

/// What the SSH advertisement offers a v0/v1 fetch.
const UPLOAD_PACK_CAPABILITIES: &str =
    "multi_ack_detailed side-band-64k thin-pack ofs-delta agent=plombir-git/0.1";

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
        // - multi_ack_detailed: `ACK <oid> common` per common object, see
        //   `negotiate`. `no-done` is the stateless (HTTP) half and is not
        //   offered on this stream.
        // - side-band-64k: packfile in sideband channel 1, messages in channel 2
        // - thin-pack: deltas against objects the client said it has
        // - ofs-delta: server can send OFS_DELTA objects (smaller packs)
        // - agent: server identification
        let caps = UPLOAD_PACK_CAPABILITIES;
        let line = format!("{} {}\0{}", sha, refname, caps);
        lines.push(PktLine::Data(line.into_bytes()));
    } else {
        // Empty repo — still need capabilities
        let caps = UPLOAD_PACK_CAPABILITIES;
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

/// What the client's capabilities ask of the pack.
#[derive(Clone, Copy, Debug)]
struct PackOptions {
    use_sideband: bool,
    thin: bool,
    ofs_delta: bool,
}

/// Generate and send the pack of `wants` minus everything reachable from
/// `common`.
///
/// The pack is forwarded to `writer` as `git pack-objects` produces it — see
/// [`crate::protocol::pack_stream`] for why it is never collected first. The
/// only trailing work is the band-2 "Done." and the sideband flush, both of
/// which are reached solely on a clean exit: a failed generation returns `Err`
/// from the stream instead, having announced itself on band 3.
///
/// `common` holds only objects the repository has (see [`negotiate`]):
/// `pack-objects` dies on `^<oid>` of an object it cannot find.
///
/// TODO(gix): Replace with gix pack generation when available.
/// Currently using git pack-objects CLI as gix doesn't have a direct replacement.
async fn send_packfile<W: AsyncWrite + Unpin>(
    repo_path: &Path,
    wants: &[String],
    common: &[String],
    writer: &mut W,
    options: PackOptions,
) -> Result<()> {
    let mut revs = String::new();
    for want in wants {
        revs.push_str(want);
        revs.push('\n');
    }
    for have in common {
        revs.push('^');
        revs.push_str(have);
        revs.push('\n');
    }
    let mut args = vec!["pack-objects", "--revs", "--stdout"];
    if options.thin {
        args.push("--thin");
    }
    if options.ofs_delta {
        args.push("--delta-base-offset");
    }

    let mut cmd = crate::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?
        .spawn_async(&args, Some(repo_path))
        .await
        .context("failed to spawn git pack-objects")?;

    // The revision list goes in whole and the pipe is closed, so the child
    // sees EOF and starts packing.
    if let Some(mut stdin) = cmd.stdin.take() {
        stdin
            .write_all(revs.as_bytes())
            .await
            .context("failed to write revs to pack-objects stdin")?;
    }

    let pack_size = pack_stream::stream_pack_objects(cmd, writer, options.use_sideband).await?;

    if options.use_sideband {
        // Send "Done." progress message (band 2)
        sideband::write_sideband_progress(writer, "Done.\n").await?;

        // Send flush to end sideband
        sideband::write_sideband_flush(writer).await?;
    }

    tracing::info!(
        pack_size,
        wants = wants.len(),
        common = common.len(),
        "Upload-pack complete"
    );

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
    async fn upload_pack_does_not_advertise_the_server_private_namespaces() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = crate::test_support::repository_with_server_private_refs(dir.path());
        let (result, output) = run_live_upload_pack(&repo_path).await;
        result.unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("refs/heads/main"), "{output}");
        assert!(
            crate::test_support::server_private_refs_in(&output).is_empty(),
            "{output}"
        );
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
    async fn want_parser_refuses_entries_past_its_ceiling() {
        let mut input = pkt(&format!("want {}\n", "a".repeat(40)));
        input.extend_from_slice(&pkt(&format!("want {}\n", "b".repeat(40))));
        input.extend_from_slice(b"0000");
        let mut reader = BufReader::new(Cursor::new(input));

        let error = super::read_wants_with_limits(&mut reader, 1, 1024)
            .await
            .expect_err("the second retained negotiation entry must be refused");

        assert!(error.to_string().contains("entry limit"), "{error:#}");
    }

    #[tokio::test]
    async fn want_parser_refuses_wire_bytes_past_its_ceiling() {
        let input = pkt(&format!("want {}\n", "a".repeat(40)));
        let mut reader = BufReader::new(Cursor::new(input));

        let error = super::read_wants_with_limits(&mut reader, 10, 8)
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

/// card_ad83ad72d14a: what a v0/v1 fetch is answered with — the negotiation
/// it asked for, and a pack of what it lacks, never the whole repository.
#[cfg(test)]
mod negotiation_tests {
    use std::io::Cursor;
    use std::path::{Path, PathBuf};

    use tokio::io::AsyncReadExt;

    fn git_ok(args: &[&str], cwd: &Path) -> String {
        let out = crate::cli_gateway::global_gateway()
            .as_ref()
            .expect("git gateway")
            .run_with_env(
                args,
                Some(cwd),
                &[
                    ("GIT_AUTHOR_NAME", "fixture"),
                    ("GIT_AUTHOR_EMAIL", "fixture@example.invalid"),
                    ("GIT_COMMITTER_NAME", "fixture"),
                    ("GIT_COMMITTER_EMAIL", "fixture@example.invalid"),
                ],
            )
            .expect("run git");
        out.ensure_success().expect("git succeeded");
        out.stdout_str().trim().to_string()
    }

    fn pkt(payload: &str) -> Vec<u8> {
        let mut encoded = format!("{:04x}", payload.len() + 4).into_bytes();
        encoded.extend_from_slice(payload.as_bytes());
        encoded
    }

    /// `main` at `newer` (child of `older`), and `refs/forks/x` at `hidden`, a
    /// commit no advertised ref reaches.
    struct Served {
        _dir: tempfile::TempDir,
        bare: PathBuf,
        older: String,
        newer: String,
        hidden: String,
    }

    fn served() -> Served {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        git_ok(&["init", "-q", "--initial-branch=main", "."], &work);
        let commit = |file: &str| {
            std::fs::write(work.join(file), format!("{file}\n")).unwrap();
            git_ok(&["add", file], &work);
            git_ok(&["commit", "-q", "-m", file], &work);
            git_ok(&["rev-parse", "HEAD"], &work)
        };
        let older = commit("older.txt");
        let newer = commit("newer.txt");
        git_ok(&["checkout", "-q", "--detach", &older], &work);
        let hidden = commit("hidden-secret.txt");
        // Back on main: a bare clone takes over the source's HEAD, and a HEAD
        // detached at `hidden` would advertise it.
        git_ok(&["checkout", "-q", "main"], &work);
        let bare = dir.path().join("served.git");
        git_ok(
            &[
                "clone",
                "-q",
                "--bare",
                work.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
            dir.path(),
        );
        git_ok(&["fetch", "-q", work.to_str().unwrap(), &hidden], &bare);
        git_ok(&["update-ref", "refs/forks/x", &hidden], &bare);
        Served {
            _dir: dir,
            bare,
            older,
            newer,
            hidden,
        }
    }

    /// The pkt-lines of a response and the raw pack after them, if any.
    fn split_response(response: &[u8]) -> (Vec<String>, Option<Vec<u8>>) {
        let mut lines = Vec::new();
        let mut rest = response;
        loop {
            if rest.is_empty() {
                return (lines, None);
            }
            if rest.starts_with(b"PACK") {
                return (lines, Some(rest.to_vec()));
            }
            let len = usize::from_str_radix(std::str::from_utf8(&rest[..4]).unwrap(), 16).unwrap();
            if len == 0 {
                lines.push("0000".to_string());
                rest = &rest[4..];
                continue;
            }
            lines.push(
                String::from_utf8_lossy(&rest[4..len])
                    .trim_end()
                    .to_string(),
            );
            rest = &rest[len..];
        }
    }

    fn object_count(pack: &[u8]) -> u32 {
        u32::from_be_bytes(pack[8..12].try_into().unwrap())
    }

    fn objects_between(bare: &Path, tip: &str, exclude: Option<&str>) -> u32 {
        let mut args = vec!["rev-list", "--objects", tip];
        if let Some(exclude) = exclude {
            args.extend(["--not", exclude]);
        }
        git_ok(&args, bare).lines().count() as u32
    }

    async fn stateless(bare: &Path, request: Vec<u8>) -> (anyhow::Result<()>, Vec<u8>) {
        let (mut client, mut server) = tokio::io::duplex(1 << 20);
        let repo = bare.to_path_buf();
        let handler = tokio::spawn(async move {
            super::handle_upload_pack_http(&repo, Cursor::new(request), &mut server).await
        });
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();
        (handler.await.unwrap(), response)
    }

    fn request(want_line: &str, haves: &[&str], end: &str) -> Vec<u8> {
        let mut request = pkt(want_line);
        request.extend_from_slice(b"0000");
        for have in haves {
            request.extend_from_slice(&pkt(&format!("have {have}\n")));
        }
        match end {
            "done" => request.extend_from_slice(&pkt("done\n")),
            _ => request.extend_from_slice(b"0000"),
        }
        request
    }

    #[tokio::test]
    async fn an_incremental_fetch_packs_only_what_the_client_lacks() {
        let served = served();
        let unknown = "1".repeat(40);

        for (caps, expected) in [
            // Plain: one ACK for the first common object, nothing at `done`.
            ("ofs-delta", vec![format!("ACK {}", served.older)]),
            (
                "multi_ack_detailed ofs-delta",
                vec![
                    format!("ACK {} common", served.older),
                    format!("ACK {}", served.older),
                ],
            ),
        ] {
            let (result, response) = stateless(
                &served.bare,
                request(
                    &format!("want {}\0{caps}\n", served.newer),
                    &[&unknown, &served.older],
                    "done",
                ),
            )
            .await;
            result.unwrap();
            let (lines, pack) = split_response(&response);
            assert_eq!(lines, expected, "{caps}");
            let pack = pack.expect("a request ending in done gets a pack");
            assert_eq!(
                object_count(&pack),
                objects_between(&served.bare, &served.newer, Some(&served.older)),
                "{caps}: the pack carried objects the client said it has"
            );
        }

        // A clone — no haves — gets the whole of main and nothing of the
        // hidden ref.
        let (result, response) = stateless(
            &served.bare,
            request(&format!("want {}\0ofs-delta\n", served.newer), &[], "done"),
        )
        .await;
        result.unwrap();
        let (lines, pack) = split_response(&response);
        assert_eq!(lines, vec!["NAK".to_string()]);
        assert_eq!(
            object_count(&pack.unwrap()),
            objects_between(&served.bare, &served.newer, None),
            "a clone must get main and only main — not `pack-objects --all`"
        );
    }

    /// A stateless request that ends in a flush is one negotiation round: it is
    /// answered with acknowledgments, and the pack waits for `done`.
    #[tokio::test]
    async fn a_round_without_done_gets_acknowledgments_and_no_pack() {
        let served = served();
        let unknown = "2".repeat(40);

        let (result, response) = stateless(
            &served.bare,
            request(
                &format!("want {}\0multi_ack_detailed side-band-64k\n", served.newer),
                &[&unknown, &served.older],
                "flush",
            ),
        )
        .await;
        result.unwrap();
        assert_eq!(
            split_response(&response),
            (
                vec![format!("ACK {} common", served.older), "NAK".to_string()],
                None
            )
        );

        let (result, response) = stateless(
            &served.bare,
            request(
                &format!("want {}\0side-band-64k\n", served.newer),
                &[&unknown],
                "flush",
            ),
        )
        .await;
        result.unwrap();
        assert_eq!(split_response(&response), (vec!["NAK".to_string()], None));
    }

    /// Only what an advertisement offers may be wanted: the tip of a hidden
    /// ref is refused, a commit an advertised ref reaches is not.
    #[tokio::test]
    async fn a_want_the_advertisement_did_not_offer_is_refused() {
        let served = served();

        let (result, response) = stateless(
            &served.bare,
            request(&format!("want {}\0ofs-delta\n", served.hidden), &[], "done"),
        )
        .await;
        assert!(result.is_err(), "a hidden tip was served");
        let (lines, pack) = split_response(&response);
        assert_eq!(
            lines,
            vec![format!("ERR upload-pack: not our ref {}", served.hidden)]
        );
        assert!(pack.is_none());

        let (result, response) = stateless(
            &served.bare,
            request(&format!("want {}\0ofs-delta\n", served.older), &[], "done"),
        )
        .await;
        result.unwrap();
        assert!(split_response(&response).1.is_some());
    }
}
