//! Git receive-pack protocol implementation (git push).
//!
//! Supports two modes:
//! 1. Split reader/writer (HTTP mode) — via `handle_receive_pack`
//! 2. Single bidirectional stream (SSH mode) — via `handle_receive_pack_stream`

use std::path::Path;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::pkt_line::{read_pkt_line, write_flush, write_pkt_line, PktLine};
use crate::sideband;

/// Result of processing a push for a single ref update.
#[derive(Clone, Debug)]
pub struct RefUpdate {
    pub old_sha: String,
    pub new_sha: String,
    pub refname: String,
    pub status: String,
    pub message: String,
}

/// Handle receive-pack with separate reader and writer (HTTP mode).
/// Returns the list of ref updates that were processed.
pub async fn handle_receive_pack<R, W>(
    repo_path: &Path,
    reader: R,
    writer: W,
) -> Result<Vec<RefUpdate>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(reader);
    let mut writer = writer;

    // Send ref advertisement
    let ref_list = build_ref_list(repo_path);
    let ad = build_ref_advertisement(&ref_list, "git-receive-pack");
    for pkt in &ad {
        write_pkt_line(&mut writer, pkt).await?;
    }
    write_flush(&mut writer).await?;

    // Process the push
    let results = process_push(repo_path, &mut reader).await?;

    // Send response
    send_response(&mut writer, &results).await?;
    Ok(results)
}

/// Handle receive-pack with a single bidirectional stream (SSH mode).
/// Takes a mutable reference so the caller can send exit-status before dropping the stream.
/// Returns the list of ref updates that were processed.
pub async fn handle_receive_pack_stream<S>(
    repo_path: &Path,
    stream: &mut S,
) -> Result<Vec<RefUpdate>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    do_receive_pack_stream(repo_path, stream).await
}

/// Handle receive-pack with a caller-provided pre-receive validator.
///
/// The validator receives the parsed ref update commands before pack indexing
/// and before any ref is written. It can mark individual updates as `error`
/// while leaving allowed updates as `ok`.
pub async fn handle_receive_pack_stream_with_rejections<S>(
    repo_path: &Path,
    stream: &mut S,
    rejected_refs: Vec<(String, String)>,
    require_signed_refs: Vec<String>,
) -> Result<Vec<RefUpdate>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    do_receive_pack_stream_with_rejections(repo_path, stream, rejected_refs, require_signed_refs)
        .await
}

/// Handle receive-pack for HTTP mode where ref advertisement is already sent.
/// Returns the list of ref updates that were processed.
pub async fn handle_receive_pack_http<R, W>(
    repo_path: &Path,
    reader: R,
    mut writer: W,
) -> Result<Vec<RefUpdate>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(reader);

    let results = process_push(repo_path, &mut reader).await?;
    send_response(&mut writer, &results).await?;
    Ok(results)
}

/// Handle receive-pack for HTTP mode with a caller-provided pre-receive validator.
pub async fn handle_receive_pack_http_with_rejections<R, W>(
    repo_path: &Path,
    reader: R,
    mut writer: W,
    rejected_refs: Vec<(String, String)>,
    require_signed_refs: Vec<String>,
) -> Result<Vec<RefUpdate>>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(reader);

    let results =
        process_push_with_rejections(repo_path, &mut reader, &rejected_refs, &require_signed_refs)
            .await?;
    send_response(&mut writer, &results).await?;
    Ok(results)
}

/// Internal: SSH mode implementation with single stream type.
async fn do_receive_pack_stream<S>(repo_path: &Path, stream: &mut S) -> Result<Vec<RefUpdate>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let ref_list = build_ref_list(repo_path);
    let ad = build_ref_advertisement(&ref_list, "git-receive-pack");
    for pkt in &ad {
        write_pkt_line(stream, pkt).await?;
    }
    write_flush(stream).await?;

    // Phase 1: Read push data (wrapped in BufReader for line-reading)
    let results = {
        let mut reader = BufReader::new(&mut *stream);
        process_push(repo_path, &mut reader).await?
    };

    // Phase 2: Write response (BufReader is dropped, stream is available again)
    send_response(stream, &results).await?;
    Ok(results)
}

async fn do_receive_pack_stream_with_rejections<S>(
    repo_path: &Path,
    stream: &mut S,
    rejected_refs: Vec<(String, String)>,
    require_signed_refs: Vec<String>,
) -> Result<Vec<RefUpdate>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let ref_list = build_ref_list(repo_path);
    let ad = build_ref_advertisement(&ref_list, "git-receive-pack");
    for pkt in &ad {
        write_pkt_line(stream, pkt).await?;
    }
    write_flush(stream).await?;

    let results = {
        let mut reader = BufReader::new(&mut *stream);
        process_push_with_rejections(repo_path, &mut reader, &rejected_refs, &require_signed_refs)
            .await?
    };

    send_response(stream, &results).await?;
    Ok(results)
}

/// Build the list of refs with their SHAs for advertisement.
fn build_ref_list(repo_path: &Path) -> Vec<(String, String)> {
    let mut refs = Vec::new();

    // Get all refs using gix API
    if let Ok(repo) = gix::open(repo_path) {
        if let Ok(references) = repo.references() {
            if let Ok(all_refs) = references.all() {
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
            }
        }

        // Also add HEAD if we have it
        if let Ok(head_id) = repo.head_id() {
            let head_entry = refs.iter().find(|(_, name)| name == "HEAD");
            if head_entry.is_none() {
                refs.push((head_id.to_string(), "HEAD".to_string()));
            }
        }
    }

    if refs.is_empty() {
        // Empty repo — add a null ref
        refs.push((
            "0000000000000000000000000000000000000000".to_string(),
            "capabilities^{}".to_string(),
        ));
    }

    refs
}

/// Build ref advertisement pkt-lines for receive-pack.
fn build_ref_advertisement(ref_list: &[(String, String)], _service: &str) -> Vec<PktLine> {
    let mut lines = Vec::new();

    // Capabilities for receive-pack:
    // - report-status: server will send ref update status after receiving the push
    // - report-status-v2: extended status format (we respond in v1-compatible way)
    // - side-band-64k: server can send progress/error on sideband during pack receipt
    // - agent: server identification
    // NOTE: We do NOT advertise atomic (all-or-nothing ref updates) because
    // we process refs sequentially.
    let caps = "report-status report-status-v2 side-band-64k agent=ironforge/0.1";

    if let Some((sha, refname)) = ref_list.first() {
        let line = format!("{} {}\0{}", sha, refname, caps);
        lines.push(PktLine::Data(line.into_bytes()));
    }

    for (sha, refname) in ref_list.iter().skip(1) {
        let line = format!("{} {}", sha, refname);
        lines.push(PktLine::Data(line.into_bytes()));
    }

    lines
}

/// Process the push: read update commands, packfile, and update refs.
async fn process_push<R: AsyncRead + Unpin>(
    repo_path: &Path,
    reader: &mut BufReader<R>,
) -> Result<Vec<RefUpdate>> {
    process_push_with_rejections(repo_path, reader, &[], &[]).await
}

async fn process_push_with_rejections<R>(
    repo_path: &Path,
    reader: &mut BufReader<R>,
    rejected_refs: &[(String, String)],
    require_signed_refs: &[String],
) -> Result<Vec<RefUpdate>>
where
    R: AsyncRead + Unpin,
{
    let mut updates = Vec::new();

    // Read update commands using proper pkt-line parsing.
    // Each line is: `old_sha new_sha refname[\0capabilities]`
    // Terminated by a flush packet ("0000").
    loop {
        let pkt = read_pkt_line(reader).await?;

        // Flush packet or EOF → end of update commands
        // Delim/ResponseEnd are V2-only and shouldn't appear in V1 protocol
        match pkt {
            PktLine::Flush => break,
            PktLine::Delim | PktLine::ResponseEnd => continue,
            PktLine::Data(bytes) => {
                let line = String::from_utf8_lossy(&bytes);
                let line = line.trim_end_matches('\n');

                if line.is_empty() {
                    continue;
                }

                // First update line may include capabilities after NUL
                let clean_line = if line.contains('\0') {
                    line.split('\0').next().unwrap_or(line)
                } else {
                    line
                };

                let parts: Vec<&str> = clean_line.split_whitespace().collect();
                if parts.len() < 3 {
                    continue;
                }

                let old_sha = parts[0].to_string();
                let new_sha = parts[1].to_string();
                let refname = parts[2].to_string();

                tracing::info!(
                    old = %old_sha,
                    new = %new_sha,
                    refname = %refname,
                    "Receive-pack: update command"
                );

                // Skip null SHA (delete) for now
                if new_sha.starts_with("0000000") {
                    updates.push(RefUpdate {
                        old_sha,
                        new_sha,
                        refname,
                        status: "error".to_string(),
                        message: "deletion not supported".to_string(),
                    });
                    continue;
                }

                updates.push(RefUpdate {
                    old_sha: old_sha.clone(),
                    new_sha: new_sha.clone(),
                    refname: refname.clone(),
                    status: "ok".to_string(),
                    message: String::new(),
                });
            }
        }
    }

    if updates.is_empty() {
        return Ok(updates);
    }

    for update in &mut updates {
        if update.status != "ok" {
            continue;
        }

        if let Some((_, message)) = rejected_refs
            .iter()
            .find(|(pattern, _)| ref_matches_rejection_pattern(&update.refname, pattern))
        {
            update.status = "error".to_string();
            update.message = message.clone();
        }
    }

    if !updates.iter().any(|update| update.status == "ok") {
        drain_pack(reader).await?;
        return Ok(updates);
    }

    // Receive pack data and pipe to git index-pack
    // TODO(gix): Replace with gix pack indexing when available.
    //
    // CRITICAL: --fix-thin is REQUIRED (踩坑经验 #4)
    //
    // Thin packs reference base objects NOT in the pack.
    // Without --fix-thin, git index-pack fails with "pack has delta resolution error".
    // With --fix-thin, missing bases are resolved from the repo before indexing.
    // TODO(gix): Replace with gix pack indexing when available.
    // Currently using git index-pack CLI as gix doesn't have a direct replacement.
    //
    // CRITICAL: --fix-thin is REQUIRED (踩坑经验 #4)
    //
    // A "thin pack" is a packfile that references base objects NOT included in
    // the pack. Git clients send thin packs during push to reduce network traffic.
    //
    // Without --fix-thin:
    //   git index-pack will fail with "pack has delta resolution error"
    //   or "missing delta base object"
    //
    // With --fix-thin:
    //   git index-pack resolves missing bases from the repository, adds them
    //   to the pack, making it "non-thin" before indexing.
    //
    // This is a common gotcha when implementing receive-pack. Always use
    // --fix-thin unless you're absolutely sure the client sends full packs.
    let mut index_pack = crate::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?
        .spawn_async(&["index-pack", "--fix-thin", "--stdin"], Some(repo_path))
        .await
        .context("failed to spawn git index-pack")?;

    let stdin = index_pack.stdin.as_mut().context("no stdin")?;

    // Read and forward pack data
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        stdin.write_all(&buf[..n]).await?;
    }
    // stdin is automatically closed when dropped (end of scope)

    let status = index_pack.wait().await?;
    if !status.success() {
        let stderr = index_pack.stderr.take();
        if let Some(mut stderr) = stderr {
            let mut err_msg = Vec::new();
            stderr.read_to_end(&mut err_msg).await?;
            bail!(
                "git index-pack failed: {}",
                String::from_utf8_lossy(&err_msg)
            );
        }
        bail!("git index-pack failed with status {}", status);
    }

    enforce_signed_commit_policies(repo_path, &mut updates, require_signed_refs);

    // Update the refs
    for update in &mut updates {
        if update.status != "ok" {
            continue;
        }
        match update_ref(repo_path, &update.refname, &update.new_sha) {
            Ok(()) => {
                update.message = "ok".to_string();
            }
            Err(e) => {
                update.status = "error".to_string();
                update.message = format!("{}", e);
            }
        }
    }

    Ok(updates)
}

fn enforce_signed_commit_policies(
    repo_path: &Path,
    updates: &mut [RefUpdate],
    patterns: &[String],
) {
    let gateway = match crate::cli_gateway::global_gateway().as_ref() {
        Ok(gateway) => gateway,
        Err(error) => {
            for update in updates.iter_mut().filter(|update| {
                update.status == "ok"
                    && patterns
                        .iter()
                        .any(|pattern| ref_matches_rejection_pattern(&update.refname, pattern))
            }) {
                update.status = "error".into();
                update.message = format!("unable to verify required commit signatures: {error}");
            }
            return;
        }
    };

    for update in updates.iter_mut().filter(|update| {
        update.status == "ok"
            && patterns
                .iter()
                .any(|pattern| ref_matches_rejection_pattern(&update.refname, pattern))
    }) {
        let mut args = vec!["rev-list", update.new_sha.as_str()];
        let old_exclusion;
        if !update.old_sha.starts_with("0000000") {
            old_exclusion = format!("^{}", update.old_sha);
            args.push(&old_exclusion);
        }
        let commits = match gateway.run(&args, Some(repo_path)) {
            Ok(output) if output.success() => output.stdout_str(),
            Ok(output) => {
                update.status = "error".into();
                update.message = format!(
                    "failed to enumerate commits for signature verification: {}",
                    output.stderr_str().trim()
                );
                continue;
            }
            Err(error) => {
                update.status = "error".into();
                update.message =
                    format!("failed to enumerate commits for signature verification: {error}");
                continue;
            }
        };
        if let Some(unsigned) = commits.lines().find(|sha| {
            gateway
                .run(&["verify-commit", sha], Some(repo_path))
                .map(|output| !output.success())
                .unwrap_or(true)
        }) {
            update.status = "error".into();
            update.message =
                format!("commit {unsigned} does not have a cryptographically valid signature");
        }
    }
}

/// Match a full ref against a rejection pattern. `*` matches any sequence;
/// patterns without wildcards retain exact-match behavior.
pub fn ref_matches_rejection_pattern(refname: &str, pattern: &str) -> bool {
    if !pattern.contains('*') {
        return refname == pattern;
    }
    let value = refname.as_bytes();
    let pattern = pattern.as_bytes();
    let mut dp = vec![vec![false; value.len() + 1]; pattern.len() + 1];
    dp[0][0] = true;
    for i in 1..=pattern.len() {
        if pattern[i - 1] == b'*' {
            dp[i][0] = dp[i - 1][0];
        }
        for j in 1..=value.len() {
            dp[i][j] = if pattern[i - 1] == b'*' {
                dp[i - 1][j] || dp[i][j - 1]
            } else {
                dp[i - 1][j - 1] && pattern[i - 1] == value[j - 1]
            };
        }
    }
    dp[pattern.len()][value.len()]
}

#[cfg(test)]
mod rejection_pattern_tests {
    use super::{enforce_signed_commit_policies, ref_matches_rejection_pattern, RefUpdate};
    #[test]
    fn matches_exact_branches_and_wildcard_tags() {
        assert!(ref_matches_rejection_pattern(
            "refs/heads/main",
            "refs/heads/main"
        ));
        assert!(!ref_matches_rejection_pattern(
            "refs/heads/feature",
            "refs/heads/main"
        ));
        assert!(ref_matches_rejection_pattern(
            "refs/tags/v1.2.3",
            "refs/tags/v*"
        ));
        assert!(ref_matches_rejection_pattern(
            "refs/tags/release/2026/07",
            "refs/tags/release/**"
        ));
        assert!(!ref_matches_rejection_pattern(
            "refs/tags/test-1",
            "refs/tags/v*"
        ));
    }

    #[test]
    fn signed_commit_policy_rejects_unsigned_commit_on_matching_branch() {
        let temp = tempfile::tempdir().unwrap();
        let gateway = crate::cli_gateway::global_gateway().as_ref().unwrap();
        assert!(gateway.run(&["init"], Some(temp.path())).unwrap().success());
        assert!(gateway
            .run(&["config", "user.name", "Test"], Some(temp.path()))
            .unwrap()
            .success());
        assert!(gateway
            .run(
                &["config", "user.email", "test@example.com"],
                Some(temp.path())
            )
            .unwrap()
            .success());
        assert!(gateway
            .run(&["config", "commit.gpgsign", "false"], Some(temp.path()))
            .unwrap()
            .success());
        std::fs::write(temp.path().join("README.md"), "unsigned").unwrap();
        assert!(gateway
            .run(&["add", "README.md"], Some(temp.path()))
            .unwrap()
            .success());
        assert!(gateway
            .run(&["commit", "-m", "unsigned"], Some(temp.path()))
            .unwrap()
            .success());
        let sha = gateway
            .run(&["rev-parse", "HEAD"], Some(temp.path()))
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();
        let mut updates = vec![RefUpdate {
            old_sha: "0".repeat(40),
            new_sha: sha,
            refname: "refs/heads/main".into(),
            status: "ok".into(),
            message: String::new(),
        }];
        enforce_signed_commit_policies(temp.path(), &mut updates, &["refs/heads/main".into()]);
        assert_eq!(updates[0].status, "error");
        assert!(updates[0]
            .message
            .contains("cryptographically valid signature"));
    }
}

async fn drain_pack<R: AsyncRead + Unpin>(reader: &mut BufReader<R>) -> Result<()> {
    let mut buf = [0u8; 8192];
    loop {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            return Ok(());
        }
    }
}

/// Update a ref to point to a new SHA using gix API.
fn update_ref(repo_path: &Path, refname: &str, new_sha: &str) -> Result<()> {
    let repo = gix::open(repo_path).context("failed to open repository")?;
    let object_id = gix::ObjectId::from_hex(new_sha.as_bytes())
        .map_err(|e| anyhow::anyhow!("invalid SHA: {}", e))?;

    // Use repo.reference() to create or update a reference
    // PreviousValue::Any means set unconditionally (like git update-ref)
    repo.reference(
        refname,
        object_id,
        gix::refs::transaction::PreviousValue::Any,
        "update via receive-pack",
    )
    .map_err(|e| anyhow::anyhow!("failed to update ref {}: {}", refname, e))?;

    Ok(())
}

/// Send the response back to the client using the report-status protocol.
///
/// When `side-band-64k` is negotiated (which we always advertise), the entire
/// report-status payload MUST be sideband-encoded as band 1 data.
///
/// Observed correct wire format (verified against real git receive-pack):
///
///   [sideband pkt-line: band=\x01, payload = <report-status pkt-lines concatenated>]
///   [sideband flush: 0000]
///
/// Where the inner report-status pkt-lines payload is:
///   000eunpack ok\n
///   0017ok refs/heads/main\n    (one per ref)
///   0000                        (plain flush — embedded in the band-1 payload)
///
/// The git client reads sideband until it gets a sideband flush `0000`.
/// The band-1 content is then parsed as report-status pkt-lines.
async fn send_response<W: AsyncWrite + Unpin>(writer: &mut W, results: &[RefUpdate]) -> Result<()> {
    // Build the report-status pkt-lines into an in-memory buffer.
    // These will be sent as band-1 sideband data in one shot.
    let mut report_buf: Vec<u8> = Vec::new();

    // 1. unpack status (MUST be first)
    write_pkt_line(&mut report_buf, &PktLine::text("unpack ok")).await?;

    // 2. per-ref update status
    for result in results {
        if result.status == "ok" {
            let line = format!("ok {}", result.refname);
            write_pkt_line(&mut report_buf, &PktLine::text(&line)).await?;
        } else {
            let line = format!("ng {} {}", result.refname, result.message);
            write_pkt_line(&mut report_buf, &PktLine::text(&line)).await?;
        }
    }

    // 3. Flush packet embedded in the band-1 payload
    write_flush(&mut report_buf).await?;

    // Send the entire report as sideband band-1 data
    sideband::write_sideband_data(writer, &report_buf).await?;

    // Send sideband flush to signal end of the sideband stream
    sideband::write_sideband_flush(writer).await?;

    // Ensure everything is flushed to the transport layer
    writer.flush().await?;

    tracing::info!("Receive-pack response sent");
    Ok(())
}
