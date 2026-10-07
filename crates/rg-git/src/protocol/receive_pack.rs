//! Git receive-pack protocol implementation (git push).
//!
//! Supports split HTTP reader/writer after advertisement and a single
//! bidirectional SSH stream. Both production receive paths select the variant
//! that carries the caller's pre-receive rejections when policy requires it.

use std::path::Path;

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt, BufReader};

use crate::pkt_line::{read_pkt_line, write_flush, write_pkt_line, PktLine};
use crate::refname::validate_refname;
use crate::sideband;

const NULL_SHA1: &str = "0000000000000000000000000000000000000000";

/// The `ng` reason for a push into one of the server's own namespaces — see
/// [`crate::ref_advertisement::is_server_owned`].
pub const SERVER_OWNED_NAMESPACE: &str = "server-owned namespace";

/// Hard ceiling for one incoming pack (1 GiB), shared by HTTP, SSH and direct
/// library callers. The CLI indexer streams under this bound; the native gix
/// path spools to disk under the same bound instead of retaining the pack in a
/// `Vec`.
pub const MAX_PACK_INPUT_BYTES: usize = 1024 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
#[error("incoming git pack exceeds the configured {max_bytes}-byte limit")]
struct PackInputTooLarge {
    max_bytes: usize,
}

/// Result of processing a push for a single ref update.
#[derive(Clone, Debug)]
pub struct RefUpdate {
    pub old_sha: String,
    pub new_sha: String,
    pub refname: String,
    pub status: String,
    pub message: String,
}

/// Everything the transport decided about one pusher before the push is read.
///
/// Loaded once per push by the caller — branch and tag protection, the
/// repository's pull mirror, the LFS locks other people hold — and enforced
/// here, so HTTP and SSH cannot drift apart. `Default` is "no policy applies",
/// which a caller may legitimately know; it is not the same as a caller that
/// never asked (card_6cb7471a52b2).
#[derive(Clone, Debug, Default)]
pub struct PushPolicy {
    /// `(pattern, message)` pairs: an update whose ref matches a pattern (see
    /// [`ref_matches_rejection_pattern`]) is refused with that message, before
    /// any object is read. The first match wins.
    pub rejected_refs: Vec<(String, String)>,
    /// Ref patterns every new commit of which must carry a valid signature.
    pub require_signed_refs: Vec<String>,
    /// LFS locks held by someone other than the pusher. A new commit that
    /// changes one of these paths refuses its ref.
    pub foreign_locks: Vec<ForeignLock>,
}

/// One path locked by another person, as [`PushPolicy::foreign_locks`] carries
/// it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignLock {
    /// The locked path, repository-relative, exactly as the lock spells it.
    pub path: String,
    /// The lock holder's username, for the refusal the pusher reads.
    pub owner: String,
}

impl ForeignLock {
    /// What a writer refused by this lock reads — the same words over
    /// receive-pack, the web editor and a merge.
    pub fn refusal(&self) -> String {
        format!("path '{}' is locked by {}", self.path, self.owner)
    }
}

/// Ref updates a push has already applied, kept somewhere a cancelled future
/// cannot take them with it.
///
/// [`ReceivePackOutcome`] carries the landed updates home for every failure
/// that still lets the push handler *return*. Cancellation is not one of those:
/// both transports bound the whole handler with a wall-clock budget
/// (`timeouts.git_stream_secs`, 300 s by default), and an elapsed budget simply
/// drops the future — no outcome is produced, however far the push had got.
///
/// The window is not theoretical. `update_ref` runs at the very end of the
/// push, so the last thing the budget can interrupt is exactly the
/// report-status write and the response drain that follow a branch which has
/// already moved; a first push of a large repository over a slow link reaches
/// that point around the 300 s mark as a matter of course. The hooks are owed
/// all the same, and there is no second chance: the retry carries no objects
/// and is answered `Everything up-to-date` (card_ca431156e7df).
///
/// So the transport creates the sink *outside* the timeout, hands it to the
/// handler, and reads it on every path that came back without an outcome.
#[derive(Debug, Default)]
pub struct AppliedRefUpdates(std::sync::Arc<std::sync::Mutex<Vec<RefUpdate>>>);

impl AppliedRefUpdates {
    /// An empty sink. Create it *before* the future that may be cancelled —
    /// one created inside dies with it, which is the whole defect.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record what this push applied. Called by the receive-pack handler on the
    /// far side of its point of no return, where no `.await` separates the last
    /// `update_ref` from this write, so cancellation cannot slip in between
    /// them. Nothing else has business writing here — but a transport's own
    /// tests do need to build the sink in the state the handler leaves it in.
    pub fn record(&self, updates: &[RefUpdate]) {
        let mut landed = self.lock();
        landed.clear();
        landed.extend_from_slice(updates);
    }

    /// Take what landed, or `None` when the push never reached its refs.
    ///
    /// Read this only on a path that came back *without* a
    /// [`ReceivePackOutcome`]: a run that returned one already carries the same
    /// updates there, and firing the hooks off both would run them twice.
    pub fn take_landed(&self) -> Option<Vec<RefUpdate>> {
        let mut landed = self.lock();
        (!landed.is_empty()).then(|| std::mem::take(&mut *landed))
    }

    /// A poisoned sink still holds the only record of a landed push, so the
    /// panic of some other holder must not cost the hooks their updates.
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<RefUpdate>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Outcome of a receive-pack run that reached its point of no return.
///
/// By the time a caller holds one of these the push has *happened*: the pack is
/// indexed and every accepted ref has already been written to `refs/*`. The
/// only work left was telling the client about it, and that step is reported in
/// `report_status` instead of destroying the run — an EPIPE from a client that
/// hit Ctrl-C, a dead HTTP duplex reader or a cancelled future must not cost
/// the push its CI pipeline, its `push` webhook, its watch fan-out or the
/// head-SHA refresh of the open PRs on the branch. There is no second chance to
/// run them: the refs are already at the new SHA, so a repeated `git push`
/// carries no objects and gets `Everything up-to-date` (card_abd7384eed60).
#[derive(Debug)]
pub struct ReceivePackOutcome {
    /// The ref updates this push applied — each one carrying its own per-ref
    /// `status`. Feed these to the post-push hooks on *every* branch, including
    /// the ones that answer the client with an error.
    pub ref_updates: Vec<RefUpdate>,
    /// `Ok` when the client received its report-status, `Err` when the refs
    /// landed but the response never reached it. The transport still owes the
    /// client a failure in that case (HTTP 5xx / SSH exit 1) — it genuinely
    /// does not know what happened to its push.
    pub report_status: Result<()>,
}

/// Handle receive-pack with a single bidirectional stream (SSH mode), with a
/// caller-provided pre-receive validator.
///
/// Takes a mutable reference so the caller can send exit-status before dropping
/// the stream. Returns the [`ReceivePackOutcome`] of the push: the ref updates
/// that were processed, plus whether the client got its report-status.
///
/// The validator receives the parsed ref update commands before pack indexing
/// and before any ref is written. It can mark individual updates as `error`
/// while leaving allowed updates as `ok`.
///
/// There is deliberately no validator-free twin. There used to be, and the SSH
/// transport reached for it whenever it had no protection context — which is to
/// say it ran the push with no branch and no tag protection at all. Passing
/// `PushPolicy::default()` says the same thing at the call site, where it is
/// visible (card_6cb7471a52b2).
pub async fn handle_receive_pack_stream_with_rejections<S>(
    repo_path: &Path,
    stream: &mut S,
    policy: PushPolicy,
    applied: &AppliedRefUpdates,
) -> Result<ReceivePackOutcome>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    do_receive_pack_stream_with_rejections(repo_path, stream, policy, applied).await
}

/// Handle receive-pack for HTTP mode with a caller-provided pre-receive validator.
pub async fn handle_receive_pack_http_with_rejections<R, W>(
    repo_path: &Path,
    reader: R,
    mut writer: W,
    policy: PushPolicy,
    applied: &AppliedRefUpdates,
) -> Result<ReceivePackOutcome>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = BufReader::new(reader);

    let ref_updates =
        process_push_with_rejections(repo_path, &mut reader, &policy, applied).await?;
    // Point of no return: the line above indexed the pack and wrote every
    // accepted ref. `send_response` is an ordinary network write and may fail
    // for reasons that have nothing to do with the push, so its error travels
    // *beside* the applied updates rather than through `?` — see
    // [`ReceivePackOutcome`].
    let report_status = send_response(&mut writer, &ref_updates).await;
    Ok(ReceivePackOutcome {
        ref_updates,
        report_status,
    })
}

/// Internal: SSH mode implementation with single stream type.
async fn do_receive_pack_stream_with_rejections<S>(
    repo_path: &Path,
    stream: &mut S,
    policy: PushPolicy,
    applied: &AppliedRefUpdates,
) -> Result<ReceivePackOutcome>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let ref_list = build_ref_list(repo_path)?;
    let ad = build_ref_advertisement(&ref_list, "git-receive-pack");
    for pkt in &ad {
        write_pkt_line(stream, pkt).await?;
    }
    write_flush(stream).await?;

    let ref_updates = {
        let mut reader = BufReader::new(&mut *stream);
        process_push_with_rejections(repo_path, &mut reader, &policy, applied).await?
    };

    // Point of no return crossed above, exactly as on the HTTP twin: the
    // report-status write is reported beside the applied updates instead of
    // taking them down with it (see [`ReceivePackOutcome`]).
    let report_status = send_response(stream, &ref_updates).await;
    Ok(ReceivePackOutcome {
        ref_updates,
        report_status,
    })
}

/// Build the list of refs with their SHAs for advertisement.
fn build_ref_list(repo_path: &Path) -> Result<Vec<(String, String)>> {
    let advertisement = crate::ref_advertisement::collect_for_clients(repo_path)?;
    let mut refs = advertisement.refs;

    if let Some(head_oid) = advertisement.head_oid {
        refs.insert(0, (head_oid, "HEAD".to_string()));
    }

    if refs.is_empty() {
        // Empty repo — add a null ref
        refs.push((
            "0000000000000000000000000000000000000000".to_string(),
            "capabilities^{}".to_string(),
        ));
    }

    Ok(refs)
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
    let caps = "report-status report-status-v2 side-band-64k agent=plombir-git/0.1";

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
///
/// An empty [`PushPolicy`] means "no policy applies to this push", which is a
/// thing a caller may legitimately know — it is not the same as a caller that
/// never asked. The convenience wrapper that used to hide the distinction is
/// gone (card_6cb7471a52b2).
async fn process_push_with_rejections<R>(
    repo_path: &Path,
    reader: &mut BufReader<R>,
    policy: &PushPolicy,
    applied: &AppliedRefUpdates,
) -> Result<Vec<RefUpdate>>
where
    R: AsyncRead + Unpin,
{
    let mut updates = Vec::new();
    let mut negotiation_bytes = 0_usize;

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
                negotiation_bytes = checked_receive_negotiation_bytes(
                    negotiation_bytes,
                    bytes.len(),
                    super::MAX_NEGOTIATION_INPUT_BYTES,
                )?;
                // RefUpdate stores a String and every policy/hook downstream
                // compares that exact spelling. Lossy decoding silently
                // rewrote a non-UTF-8 wire ref to U+FFFD and could therefore
                // authorize one name before updating another.
                let line = std::str::from_utf8(&bytes)
                    .context("receive-pack update command must be valid UTF-8")?;
                let line = line.trim_end_matches('\n');

                if line.is_empty() {
                    continue;
                }

                if updates.len() >= super::MAX_NEGOTIATION_ENTRIES {
                    bail!(
                        "receive-pack update list exceeds the configured {}-entry limit",
                        super::MAX_NEGOTIATION_ENTRIES
                    );
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

                let invalid = validate_wire_object_id(&old_sha)
                    .and_then(|_| validate_wire_object_id(&new_sha))
                    .err()
                    .map(|error| error.to_string())
                    .or_else(|| {
                        validate_refname(&refname)
                            .err()
                            .map(|error| error.to_string())
                    })
                    // The server writes some of these for its own work and
                    // compares what it wrote on the way out (`update-ref <ref>
                    // <new> <old>`), and gives the others a meaning of its own
                    // (a pull request's pipeline ref, git's object
                    // replacement), so a client's write there is never wanted:
                    // git's own `receive.hideRefs` makes a hidden ref
                    // unwritable too (card_e62ac71c4768). Decided per ref,
                    // before any object is read, so the rest of the push is
                    // unaffected.
                    .or_else(|| {
                        crate::ref_advertisement::is_server_owned(&refname)
                            .then(|| SERVER_OWNED_NAMESPACE.to_string())
                    });
                if let Some(message) = invalid {
                    updates.push(RefUpdate {
                        old_sha,
                        new_sha,
                        refname,
                        status: "error".to_string(),
                        message,
                    });
                    continue;
                }

                // Skip null SHA (delete) for now.
                if new_sha == NULL_SHA1 {
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

        if let Some((_, message)) = policy
            .rejected_refs
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

    // Receive the incoming pack and index it into the repository.
    //
    // Two implementations exist behind a flag (default: the git CLI):
    //   * `index_pack_via_git`    — `git index-pack --fix-thin --stdin` (subprocess).
    //   * `index_pack_native`     — `gix_pack::Bundle::write_to_directory` (in-process,
    //     interrupt-driven). Opt-in PoC via `PLOMBIR_GIT_NATIVE_INDEX_PACK`, off by default.
    //
    // Both must resolve the thin-pack the same way: Plombir Git advertises the
    // `thin-pack` capability, so clients send deltas whose base objects live in
    // the repo but NOT in the pack. The CLI resolves them with `--fix-thin`; the
    // native path passes the repo as the thin-pack base-object lookup. Omitting
    // either fails with "missing delta base object".
    if native_index_pack_enabled() {
        index_pack_native(repo_path, reader).await?;
    } else {
        index_pack_via_git(repo_path, reader).await?;
    }

    enforce_signed_commit_policies(repo_path, &mut updates, &policy.require_signed_refs);
    enforce_foreign_lfs_locks(repo_path, &mut updates, &policy.foreign_locks).await;

    // Update the refs
    for update in &mut updates {
        if update.status != "ok" {
            continue;
        }
        match update_ref(repo_path, &update.refname, &update.old_sha, &update.new_sha) {
            Ok(()) => {
                update.message = "ok".to_string();
            }
            Err(e) => {
                update.status = "error".to_string();
                update.message = format!("{}", e);
            }
        }
    }

    // Point of no return crossed: the refs above are written and the caller's
    // post-push hooks are owed. Everything after this line — the report-status
    // write, the duplex drain — runs inside the transport's wall-clock budget,
    // and an elapsed budget drops this future instead of letting it return, so
    // the updates go into a sink that outlives the drop (card_ca431156e7df).
    // No `.await` sits between the last `update_ref` and this call.
    applied.record(&updates);

    Ok(updates)
}

fn checked_receive_negotiation_bytes(
    current: usize,
    frame_bytes: usize,
    max_bytes: usize,
) -> Result<usize> {
    current
        .checked_add(frame_bytes)
        .filter(|size| *size <= max_bytes)
        .context("receive-pack negotiation exceeds the configured byte limit")
}

fn validate_wire_object_id(sha: &str) -> Result<()> {
    if sha.len() != NULL_SHA1.len() || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("receive-pack object id must be 40 hexadecimal characters");
    }
    Ok(())
}

/// Whether receive-pack should index the incoming pack with the native gix
/// indexer instead of the `git index-pack` subprocess.
///
/// PoC, opt-in — **default off**. Enable with `PLOMBIR_GIT_NATIVE_INDEX_PACK` set
/// to one of `1` / `true` / `yes` / `on` (case-insensitive). Any other value
/// (or an unset variable) keeps the git CLI path.
fn native_index_pack_enabled() -> bool {
    std::env::var("PLOMBIR_GIT_NATIVE_INDEX_PACK")
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Index the incoming pack via the `git index-pack --fix-thin --stdin`
/// subprocess (the default path). `--fix-thin` resolves delta bases that are in
/// the repo but not in the pack, completing the thin pack before indexing.
async fn index_pack_via_git<R>(repo_path: &Path, reader: &mut BufReader<R>) -> Result<()>
where
    R: AsyncRead + Unpin,
{
    let mut index_pack = crate::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?
        .spawn_async(&["index-pack", "--fix-thin", "--stdin"], Some(repo_path))
        .await
        .context("failed to spawn git index-pack")?;

    let stdin = index_pack.stdin.as_mut().context("no stdin")?;

    copy_pack_with_limit(reader, stdin, MAX_PACK_INPUT_BYTES).await?;
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
    Ok(())
}

/// Sets the shared interrupt flag when dropped, so an aborted async scope (e.g.
/// the wall-clock `with_git_timeout` upstream cancelling this future) propagates
/// into a detached `spawn_blocking` unpack that only observes an `AtomicBool`.
struct InterruptOnDrop(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Drop for InterruptOnDrop {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Index the incoming pack natively with `gix_pack::Bundle::write_to_directory`,
/// the interrupt-driven, subprocess-free replacement for `git index-pack`.
///
/// Two properties are load-bearing and mirror `index_pack_via_git`:
///
///   * **Thin-pack resolution (== `--fix-thin`).** Plombir Git advertises the
///     `thin-pack` capability, so the client sends a thin pack whose deltas
///     reference base objects that already exist in the repo. We pass the opened
///     repository as `thin_pack_base_object_lookup`; the writer resolves the
///     missing bases and emits a complete (non-thin) pack + index into
///     `objects/pack`. Passing `None` here would fail on the first missing base.
///
///   * **Interruptibility.** The unpack runs on a blocking thread and observes a
///     shared `AtomicBool` that gix checks on every read *and* during delta
///     resolution. [`InterruptOnDrop`] flips it if this async scope is cancelled
///     (the A1 idle/wall-clock watchdog dropping the handler future), so a
///     runaway unpack is actually aborted rather than left running detached.
async fn index_pack_native<R>(repo_path: &Path, reader: &mut BufReader<R>) -> Result<()>
where
    R: AsyncRead + Unpin,
{
    // The native writer is synchronous, so bridge the async transport through
    // a request-private file. This keeps memory flat while preserving the SSH
    // idle watchdog on each network read. The same byte ceiling as the CLI path
    // is enforced before gix sees the pack.
    let pack_dir = repo_path.join("objects").join("pack");
    tokio::fs::create_dir_all(&pack_dir)
        .await
        .with_context(|| format!("failed to create {}", pack_dir.display()))?;
    let pack = tempfile::tempfile_in(&pack_dir)
        .with_context(|| format!("failed to create temporary pack in {}", pack_dir.display()))?;
    let mut pack = tokio::fs::File::from_std(pack);
    copy_pack_with_limit(reader, &mut pack, MAX_PACK_INPUT_BYTES)
        .await
        .context("failed to spool incoming pack stream")?;
    pack.flush()
        .await
        .context("failed to flush temporary pack")?;
    pack.seek(std::io::SeekFrom::Start(0))
        .await
        .context("failed to rewind temporary pack")?;
    let pack = pack.into_std().await;

    let repo_path = repo_path.to_owned();
    let interrupt = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    // Held across the blocking join: on cancellation its Drop sets `interrupt`.
    let _guard = InterruptOnDrop(interrupt.clone());

    let join = tokio::task::spawn_blocking(move || -> Result<()> {
        let repo = crate::repository::open(&repo_path).context("failed to open repository")?;
        let pack_dir = repo_path.join("objects").join("pack");
        // `write_to_directory` requires the target directory to already exist.
        std::fs::create_dir_all(&pack_dir)
            .with_context(|| format!("failed to create {}", pack_dir.display()))?;

        let mut pack = std::io::BufReader::new(pack);
        let mut progress = gix::progress::Discard;
        let outcome = gix::odb::pack::Bundle::write_to_directory(
            &mut pack,
            Some(pack_dir.as_path()),
            &mut progress,
            &interrupt,
            Some(&repo), // thin-pack base lookup — the native equivalent of --fix-thin
            Default::default(),
        );

        if interrupt.load(std::sync::atomic::Ordering::Relaxed) {
            bail!("native index-pack interrupted (timeout)");
        }
        let outcome = outcome.context("native pack indexing failed")?;

        // `git index-pack` leaves a plain, immediately-usable pack. The writer
        // may drop a `.keep` alongside it; remove it so the objects are live.
        //
        // `rg-git` sits below `rg-core`, so it cannot use the shared
        // `platform::fs::discard_file` helper — this is the local copy of the
        // same contract: silent when the file is already gone, a warning naming
        // the path otherwise.
        if let Some(keep) = outcome.keep_path {
            if let Err(error) = std::fs::remove_file(&keep) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        path = %keep.display(),
                        %error,
                        "failed to remove the .keep guard of a freshly indexed pack; git gc will \
                         keep the pack pinned until the file is removed"
                    );
                }
            }
        }
        Ok(())
    });

    join.await.context("native index-pack task panicked")?
}

/// Operational failure while checking a required commit signature.
#[derive(Debug)]
pub enum RequiredSignatureError {
    Unavailable(String),
    Enumeration(anyhow::Error),
    Verification {
        commit: String,
        source: anyhow::Error,
    },
}

impl RequiredSignatureError {
    /// Message safe for receive-pack's per-ref status report.
    ///
    /// The `ng <ref> <message>` line goes to whoever could push, so the text is
    /// held to what that person already knows: the refname and the SHAs they
    /// just sent. Naming the commit is therefore free, and necessary — without
    /// it the pusher cannot tell which commit to re-sign.
    ///
    /// What the pusher does *not* know is how the server stores repositories or
    /// which git invocation reads them, and both remaining variants carry
    /// exactly that: `Enumeration` wraps a `GitCliError` whose `command` /
    /// `NotFound` text starts with `-C <absolute repository path>`, or the raw
    /// `git rev-list` stderr; `Unavailable` is the flattened refusal of the
    /// `git --version` probe. `Display` still renders all of it — for the
    /// `tracing::warn!` in [`enforce_signed_commit_policies`], which logs the
    /// same chain next to the refname, so the fixed text below costs no
    /// diagnostics.
    fn receive_pack_message(&self) -> String {
        match self {
            Self::Verification { commit, .. } => {
                format!("server-side signature verification failure for commit {commit}")
            }
            Self::Unavailable(_) | Self::Enumeration(_) => {
                "required-signature check could not run on the server".to_string()
            }
        }
    }
}

impl std::fmt::Display for RequiredSignatureError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unavailable(error) => {
                write!(
                    formatter,
                    "unable to verify required commit signatures: {error}"
                )
            }
            Self::Enumeration(error) => write!(
                formatter,
                "failed to enumerate commits for signature verification: {error:#}"
            ),
            Self::Verification { commit, source } => write!(
                formatter,
                "server-side signature verification failure for commit {commit}: {source:#}"
            ),
        }
    }
}

impl std::error::Error for RequiredSignatureError {}

/// Return the first unsigned commit introduced by a protected ref update.
///
/// `Ok(None)` means either the ref does not require signatures or every new
/// commit is valid.  Git enumeration / verification failures stay `Err`: a
/// broken verifier is not evidence that the pusher supplied an unsigned
/// commit.  Receive-pack and Plombir Git's server-side commit adapter share this
/// function so both paths keep the same matcher and `%G?` semantics.
pub fn unsigned_commit_for_required_signature(
    repo_path: &Path,
    old_sha: &str,
    new_sha: &str,
    refname: &str,
    patterns: &[String],
) -> std::result::Result<Option<String>, RequiredSignatureError> {
    if !patterns
        .iter()
        .any(|pattern| ref_matches_rejection_pattern(refname, pattern))
    {
        return Ok(None);
    }

    let gateway = crate::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|error| RequiredSignatureError::Unavailable(format!("{error:#}")))?;
    let mut args = vec!["rev-list", new_sha];
    let old_exclusion;
    if !old_sha.starts_with("0000000") {
        old_exclusion = format!("^{old_sha}");
        args.push(&old_exclusion);
    }
    let commits = gateway
        .run(&args, Some(repo_path))
        .map_err(RequiredSignatureError::Enumeration)?;
    if !commits.success() {
        return Err(RequiredSignatureError::Enumeration(anyhow::anyhow!(
            "{}",
            commits.stderr_str().trim()
        )));
    }

    for commit in commits.stdout_str().lines() {
        let verification = gateway
            .run(&["log", "--format=%G?", "-1", commit], Some(repo_path))
            .and_then(|output| signature_is_cryptographically_valid(&output))
            .map_err(|source| RequiredSignatureError::Verification {
                commit: commit.to_string(),
                source,
            })?;
        if !verification {
            return Ok(Some(commit.to_string()));
        }
    }

    Ok(None)
}

fn enforce_signed_commit_policies(
    repo_path: &Path,
    updates: &mut [RefUpdate],
    patterns: &[String],
) {
    for update in updates.iter_mut().filter(|update| update.status == "ok") {
        match unsigned_commit_for_required_signature(
            repo_path,
            &update.old_sha,
            &update.new_sha,
            &update.refname,
            patterns,
        ) {
            Ok(Some(commit)) => {
                update.status = "error".into();
                update.message =
                    format!("commit {commit} does not have a cryptographically valid signature");
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    refname = %update.refname,
                    error = %format!("{error:#}"),
                    "server-side commit signature verification failed"
                );
                update.status = "error".into();
                update.message = error.receive_pack_message();
            }
        }
    }
}

/// The `ng` reason for a ref refused because the LFS lock check itself could
/// not run. Fixed text for the reason [`RequiredSignatureError`] gives: the
/// failure chain names server paths, and goes to the log instead.
const LFS_LOCK_CHECK_UNAVAILABLE: &str = "LFS lock check could not run on the server";

/// Refuse every ref whose new commits change a path another person has locked
/// (card_4a40b70a6796).
///
/// The stock `git lfs` client checks this itself only when it is configured
/// with `lfs.locksverify = true`; without it, it prints "would have halted this
/// push" and pushes anyway. The server is the one place that sees every push,
/// so the lock is enforced here as well, the same way for HTTP and SSH.
///
/// "New" means what the client's own check means: commits this push brings
/// that no branch or tag already holds. Fast-forwarding `main` onto the lock
/// holder's already-pushed work is therefore not a violation by whoever moves
/// it, while a commit that reached the repository some other way — a fork pull
/// request's head, a merge-queue group — still counts as new to the branches.
/// The refs being updated have not moved yet, so `--branches --tags` is the
/// state before this push.
///
/// A repository nobody else holds a lock in costs nothing: no git process is
/// started. A check that cannot run refuses the ref rather than waving it
/// through, exactly as the required-signature check does.
async fn enforce_foreign_lfs_locks(
    repo_path: &Path,
    updates: &mut [RefUpdate],
    locks: &[ForeignLock],
) {
    if locks.is_empty() {
        return;
    }
    let locked = locked_paths(locks);
    for update in updates.iter_mut().filter(|update| update.status == "ok") {
        match first_locked_path_changed(repo_path, &update.new_sha, &locked).await {
            Ok(None) => {}
            Ok(Some(lock)) => {
                update.status = "error".into();
                update.message = lock.refusal();
            }
            Err(error) => {
                tracing::warn!(
                    refname = %update.refname,
                    error = %format!("{error:#}"),
                    "server-side LFS lock check failed"
                );
                update.status = "error".into();
                update.message = LFS_LOCK_CHECK_UNAVAILABLE.into();
            }
        }
    }
}

/// `locks` by path, the shape both lock checks match a changed path against.
fn locked_paths(locks: &[ForeignLock]) -> std::collections::HashMap<&str, &ForeignLock> {
    locks
        .iter()
        .map(|lock| (lock.path.as_str(), lock))
        .collect()
}

/// The first of `locks` whose path a commit new to `repo_path` changes: one
/// reachable from `new_sha` that no branch or tag holds yet — the same "new"
/// receive-pack enforces (see `enforce_foreign_lfs_locks`).
///
/// For a merge the server makes: the head of a fork pull request lives under a
/// scratch ref, so its commits count, while a same-repository head is already
/// on a branch and was checked when it was pushed.
pub async fn first_foreign_lock_changed(
    repo_path: &Path,
    new_sha: &str,
    locks: &[ForeignLock],
) -> Result<Option<ForeignLock>> {
    if locks.is_empty() {
        return Ok(None);
    }
    first_locked_path_changed(repo_path, new_sha, &locked_paths(locks)).await
}

/// The first of `locks` whose path `commit` changes against its parent — or,
/// for a root commit, adds.
///
/// For a commit the server has just made on top of a branch (a web edit, an
/// applied suggestion), where that one commit is everything the ref move
/// brings. Not for a merge commit: `diff-tree` lists nothing for one.
pub fn foreign_lock_changed_by_commit(
    repo_path: &Path,
    commit: &str,
    locks: &[ForeignLock],
) -> Result<Option<ForeignLock>> {
    if locks.is_empty() {
        return Ok(None);
    }
    let output = crate::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?
        .run(
            &[
                "diff-tree",
                "-r",
                "-z",
                "--root",
                "--no-commit-id",
                "--name-only",
                "--no-renames",
                commit,
                "--",
            ],
            Some(repo_path),
        )
        .context("failed to run git diff-tree for the LFS lock check")?;
    output
        .ensure_success()
        .context("git diff-tree for the LFS lock check failed")?;
    let locked = locked_paths(locks);
    Ok(output
        .stdout
        .split(|byte| *byte == b'\0')
        .filter_map(|name| std::str::from_utf8(name).ok())
        .find_map(|name| locked.get(name).map(|lock| (*lock).clone())))
}

/// The first locked path a commit new to the repository changes, with its
/// lock holder.
///
/// `git log --name-only` is streamed rather than collected: a first push of a
/// large history lists every path of every commit, and only one hit is needed.
/// Leaving early drops the child, which `spawn_async` kills.
///
/// `-c`, because `git log` lists no paths at all for a merge commit by default:
/// a merge whose result changes a locked path that neither parent changed —
/// an edit made in the merge itself — went through. The combined form names
/// exactly the paths the merge result differs from every parent in, so a clean
/// merge of work already checked still lists nothing.
async fn first_locked_path_changed(
    repo_path: &Path,
    new_sha: &str,
    locked: &std::collections::HashMap<&str, &ForeignLock>,
) -> Result<Option<ForeignLock>> {
    use tokio::io::AsyncBufReadExt;

    let mut child = crate::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?
        .spawn_async(
            &[
                "log",
                "--format=",
                "--name-only",
                "-c",
                "-z",
                "--no-renames",
                new_sha,
                "--not",
                "--branches",
                "--tags",
                "--",
            ],
            Some(repo_path),
        )
        .await
        .context("failed to spawn git log for the LFS lock check")?;
    drop(child.stdin.take());
    let stdout = child.stdout.take().context("git log has no stdout")?;
    let mut paths = BufReader::new(stdout);
    let mut path = Vec::new();
    loop {
        path.clear();
        if paths
            .read_until(b'\0', &mut path)
            .await
            .context("failed to read git log output for the LFS lock check")?
            == 0
        {
            break;
        }
        let name = path.strip_suffix(b"\0").unwrap_or(&path);
        if let Some(lock) = std::str::from_utf8(name)
            .ok()
            .and_then(|name| locked.get(name))
        {
            return Ok(Some((*lock).clone()));
        }
    }

    let status = child.wait().await.context("failed to wait for git log")?;
    if !status.success() {
        let mut stderr = Vec::new();
        if let Some(mut pipe) = child.stderr.take() {
            pipe.read_to_end(&mut stderr).await?;
        }
        bail!(
            "git log for the LFS lock check failed ({status}): {}",
            String::from_utf8_lossy(&stderr).trim()
        );
    }
    Ok(None)
}

/// Interpret `git log --format=%G?` without turning an unavailable verifier
/// into a claim that the client supplied a bad signature.
fn signature_is_cryptographically_valid(
    verify_output: &crate::cli_gateway::GitOutput,
) -> Result<bool> {
    verify_output
        .ensure_success()
        .context("git could not verify commit signature")?;

    match verify_output.stdout_str().trim() {
        "G" => Ok(true),
        // Validity is stricter than merely having a signature: an expired,
        // revoked, untrusted, bad, or absent signature cannot satisfy this
        // branch-protection policy.
        "B" | "N" | "U" | "X" | "Y" | "R" => Ok(false),
        "E" => bail!("git could not check the commit signature (status E)"),
        status => bail!("git returned an unexpected commit signature status {status:?}"),
    }
}

/// Why one tag-protection pattern cannot be honoured by the receive-pack
/// matcher.
#[derive(Debug, thiserror::Error)]
pub enum TagProtectionPatternError {
    #[error("tag pattern must not be empty")]
    Empty,
    #[error("tag pattern must be at most 255 bytes")]
    TooLong,
    #[error("tag pattern must omit the refs/ prefix")]
    Qualified,
    #[error(
        "tag pattern contains unsupported wildcard metacharacter '{0}'; only '*' is supported"
    )]
    UnsupportedMetacharacter(char),
    #[error("tag pattern cannot match a valid tag ref: {0}")]
    InvalidRefName(String),
}

/// Validate the exact pattern language [`ref_matches_rejection_pattern`]
/// implements for protected tags.
///
/// `*` is the only wildcard. GitHub-style `?`, `[]`, and `+` would otherwise
/// look meaningful while being compared literally. Replacing each supported
/// wildcard with a safe component gives the Git ref validator a concrete name
/// to check, and catches patterns which cannot match any tag (`.lock`, `@{`,
/// control characters, and the rest of Git's ref-name exclusions).
pub fn validate_tag_protection_pattern(
    pattern: &str,
) -> std::result::Result<(), TagProtectionPatternError> {
    use gix::bstr::ByteSlice;

    if pattern.is_empty() {
        return Err(TagProtectionPatternError::Empty);
    }
    if pattern.len() > 255 {
        return Err(TagProtectionPatternError::TooLong);
    }
    if pattern.starts_with("refs/") {
        return Err(TagProtectionPatternError::Qualified);
    }
    if let Some(metacharacter) = pattern.chars().find(|c| matches!(c, '?' | '[' | '+')) {
        return Err(TagProtectionPatternError::UnsupportedMetacharacter(
            metacharacter,
        ));
    }

    let witness = format!("refs/tags/{}", pattern.replace('*', "wildcard"));
    gix::validate::reference::name(witness.as_bytes().as_bstr())
        .map_err(|error| TagProtectionPatternError::InvalidRefName(error.to_string()))?;
    Ok(())
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

async fn drain_pack<R: AsyncRead + Unpin>(reader: &mut BufReader<R>) -> Result<()> {
    copy_pack_with_limit(reader, &mut tokio::io::sink(), MAX_PACK_INPUT_BYTES)
        .await
        .map(|_| ())
}

async fn copy_pack_with_limit<R, W>(
    reader: &mut R,
    writer: &mut W,
    max_bytes: usize,
) -> Result<usize>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut copied = 0_usize;
    let mut buf = [0_u8; 8192];
    loop {
        let read = reader
            .read(&mut buf)
            .await
            .context("failed to read incoming pack stream")?;
        if read == 0 {
            return Ok(copied);
        }
        copied = copied
            .checked_add(read)
            .filter(|size| *size <= max_bytes)
            .ok_or_else(|| anyhow::Error::new(PackInputTooLarge { max_bytes }))?;
        writer
            .write_all(&buf[..read])
            .await
            .context("failed to write incoming pack stream")?;
    }
}

/// Update a ref to point to a new SHA using the old value from the wire as a
/// compare-and-swap precondition.
fn update_ref(repo_path: &Path, refname: &str, old_sha: &str, new_sha: &str) -> Result<()> {
    validate_refname(refname).context("invalid receive-pack refname")?;
    validate_wire_object_id(old_sha)?;
    validate_wire_object_id(new_sha)?;

    let repo = crate::repository::open(repo_path).context("failed to open repository")?;
    let object_id = gix::ObjectId::from_hex(new_sha.as_bytes())
        .map_err(|e| anyhow::anyhow!("invalid SHA: {}", e))?;

    let expected = if old_sha == NULL_SHA1 {
        gix::refs::transaction::PreviousValue::MustNotExist
    } else {
        let old_object_id = gix::ObjectId::from_hex(old_sha.as_bytes())
            .map_err(|e| anyhow::anyhow!("invalid old SHA: {}", e))?;
        gix::refs::transaction::PreviousValue::MustExistAndMatch(gix::refs::Target::Object(
            old_object_id,
        ))
    };

    repo.reference(refname, object_id, expected, "update via receive-pack")
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

/// A push that landed must reach the caller even when the client never takes
/// its report-status.
///
/// Both transports are exercised through their real entry point, because the
/// caller's post-push hooks (CI, `push` webhook, watch fan-out, open-PR head
/// SHA) hang off the returned updates and the branch has already moved by the
/// time the response write is attempted. A failure here is permanent: the
/// pusher's retry sends nothing and gets `Everything up-to-date`.
#[cfg(test)]
mod landed_push_tests {
    use std::io::Cursor;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll};

    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio::sync::Notify;

    use super::{
        handle_receive_pack_http_with_rejections, handle_receive_pack_stream_with_rejections,
        AppliedRefUpdates, NULL_SHA1,
    };

    /// The 20-byte trailer of a zero-object pack — the SHA-1 over its own
    /// 12-byte header, which is what a client sends when the objects the push
    /// names are already in the repository. `git index-pack --fix-thin --stdin`
    /// accepts it, so the push reaches ref writing without a fixture pack.
    const EMPTY_PACK_CHECKSUM: &str = "029d08823bd8a8eab510ad6ac75c823cfd3ed31e";

    /// The commit the pushed branch is moved to. `update_ref` compares the wire
    /// old SHA and writes the reference; it does not resolve the new object, so
    /// no commit has to be fabricated to observe the branch move.
    fn pushed_sha() -> String {
        "a".repeat(40)
    }

    /// One `git push` of `refs/heads/main`, wire-shaped: the update command
    /// with the capabilities a real client negotiates, a flush, then the pack.
    fn push_request() -> Vec<u8> {
        let command = format!(
            "{} {} refs/heads/main\0report-status side-band-64k agent=git/2.43\n",
            NULL_SHA1,
            pushed_sha()
        );
        let mut request = format!("{:04x}", command.len() + 4).into_bytes();
        request.extend_from_slice(command.as_bytes());
        request.extend_from_slice(b"0000");
        request.extend_from_slice(b"PACK");
        request.extend_from_slice(&2u32.to_be_bytes());
        request.extend_from_slice(&0u32.to_be_bytes());
        request.extend_from_slice(
            gix::ObjectId::from_hex(EMPTY_PACK_CHECKSUM.as_bytes())
                .expect("empty-pack checksum is a valid object id")
                .as_slice(),
        );
        request
    }

    /// A response writer whose every write fails, as a closed HTTP duplex or a
    /// socket to a client that pressed Ctrl-C does.
    struct BrokenWriter;

    impl AsyncWrite for BrokenWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "client hung up",
            )))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// A response writer whose first write never completes, as a duplex whose
    /// reader is gone does. It announces the attempt, because that instant is
    /// the earliest one at which the refs are already on disk — which is
    /// exactly where an elapsed wall-clock budget drops the handler.
    struct HangingWriter {
        reached_response: Arc<Notify>,
    }

    impl AsyncWrite for HangingWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.reached_response.notify_one();
            Poll::Pending
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Pending
        }
    }

    /// What the SSH fixture client does once it has handed over the whole push
    /// and the server starts writing the report-status back.
    #[derive(Clone, Copy)]
    enum ResponseFate {
        /// The socket is gone: every write fails, and the handler returns.
        Broken,
        /// The socket takes nothing and never will: the write stays pending,
        /// which is where the wall-clock budget elapses and the future is
        /// dropped without ever returning.
        Hangs,
    }

    /// The SSH twin: one bidirectional stream that takes the advertisement,
    /// hands over a complete push, and then breaks. Writes fail only once the
    /// request is drained, so the advertisement still goes out and the failure
    /// lands exactly on the report-status.
    struct HungUpClient {
        request: Cursor<Vec<u8>>,
        request_drained: bool,
        fate: ResponseFate,
        reached_response: Arc<Notify>,
    }

    impl HungUpClient {
        fn new(fate: ResponseFate, reached_response: Arc<Notify>) -> Self {
            Self {
                request: Cursor::new(push_request()),
                request_drained: false,
                fate,
                reached_response,
            }
        }
    }

    impl AsyncRead for HungUpClient {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            let remaining = {
                let request = self.request.get_ref();
                let position = self.request.position() as usize;
                &request[position.min(request.len())..]
            };
            if remaining.is_empty() {
                self.request_drained = true;
                return Poll::Ready(Ok(()));
            }
            let take = remaining.len().min(buf.remaining());
            let chunk = remaining[..take].to_vec();
            buf.put_slice(&chunk);
            let position = self.request.position();
            self.request.set_position(position + take as u64);
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncWrite for HungUpClient {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            if self.request_drained {
                self.reached_response.notify_one();
                return match self.fate {
                    ResponseFate::Broken => Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "client hung up",
                    ))),
                    ResponseFate::Hangs => Poll::Pending,
                };
            }
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn assert_branch_landed(repo_path: &std::path::Path, outcome: &super::ReceivePackOutcome) {
        let head = gix::open(repo_path)
            .expect("open pushed repository")
            .find_reference("refs/heads/main")
            .expect("the push must have created the branch")
            .id()
            .to_string();
        assert_eq!(head, pushed_sha(), "the branch did not take the pushed SHA");

        let [update] = outcome.ref_updates.as_slice() else {
            panic!(
                "one ref update expected, got {:?}",
                outcome.ref_updates.len()
            );
        };
        assert_eq!(update.refname, "refs/heads/main");
        assert_eq!(update.new_sha, pushed_sha());
        assert_eq!(update.status, "ok");
        assert!(
            outcome.report_status.is_err(),
            "the client never read the report-status, so its delivery must be reported as failed"
        );
    }

    #[tokio::test]
    async fn http_push_keeps_its_ref_updates_when_the_report_status_cannot_be_sent() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("http-push.git");
        gix::init_bare(&repo_path).unwrap();

        let outcome = handle_receive_pack_http_with_rejections(
            &repo_path,
            Cursor::new(push_request()),
            BrokenWriter,
            super::PushPolicy::default(),
            &AppliedRefUpdates::new(),
        )
        .await
        .expect("a landed push must survive a broken response writer");

        assert_branch_landed(&repo_path, &outcome);
    }

    #[tokio::test]
    async fn ssh_push_keeps_its_ref_updates_when_the_report_status_cannot_be_sent() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("ssh-push.git");
        gix::init_bare(&repo_path).unwrap();
        let mut client = HungUpClient::new(ResponseFate::Broken, Arc::new(Notify::new()));

        let outcome = handle_receive_pack_stream_with_rejections(
            &repo_path,
            &mut client,
            super::PushPolicy::default(),
            &AppliedRefUpdates::new(),
        )
        .await
        .expect("a landed push must survive a broken response writer");

        assert_branch_landed(&repo_path, &outcome);
    }

    /// What a transport is left holding when the wall-clock budget elapses:
    /// no outcome, a branch that has moved, and a sink that still knows it.
    fn assert_landed_without_an_outcome(repo_path: &std::path::Path, applied: &AppliedRefUpdates) {
        let head = gix::open(repo_path)
            .expect("open pushed repository")
            .find_reference("refs/heads/main")
            .expect("the push must have created the branch")
            .id()
            .to_string();
        assert_eq!(head, pushed_sha(), "the branch did not take the pushed SHA");

        let landed = applied.take_landed().expect(
            "a push cancelled after its refs landed still owes the post-push hooks its updates",
        );
        let [update] = landed.as_slice() else {
            panic!("one ref update expected, got {:?}", landed.len());
        };
        assert_eq!(update.refname, "refs/heads/main");
        assert_eq!(update.new_sha, pushed_sha());
        assert_eq!(update.status, "ok");
        assert!(
            applied.take_landed().is_none(),
            "the sink hands its updates over once — a second read would run every hook twice"
        );
    }

    #[tokio::test]
    async fn http_push_cancelled_after_its_refs_landed_still_yields_them() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("http-timeout-push.git");
        gix::init_bare(&repo_path).unwrap();
        let applied = AppliedRefUpdates::new();
        let reached_response = Arc::new(Notify::new());

        let mut push = Box::pin(handle_receive_pack_http_with_rejections(
            &repo_path,
            Cursor::new(push_request()),
            HangingWriter {
                reached_response: reached_response.clone(),
            },
            super::PushPolicy::default(),
            &applied,
        ));

        // Cancel precisely where the transport's wall-clock budget does its
        // damage: the pack is indexed, `refs/heads/main` is written, and the
        // report-status write is in flight. `with_git_timeout` drops the future
        // exactly as this does — no `Err` ever comes back out of it.
        tokio::select! {
            outcome = &mut push => panic!(
                "the push cannot finish: its response writer never completes ({:?})",
                outcome.map(|outcome| outcome.ref_updates.len())
            ),
            _ = reached_response.notified() => {}
        }
        drop(push);

        assert_landed_without_an_outcome(&repo_path, &applied);
    }

    #[tokio::test]
    async fn ssh_push_cancelled_after_its_refs_landed_still_yields_them() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("ssh-timeout-push.git");
        gix::init_bare(&repo_path).unwrap();
        let applied = AppliedRefUpdates::new();
        let reached_response = Arc::new(Notify::new());
        let mut client = HungUpClient::new(ResponseFate::Hangs, reached_response.clone());

        let mut push = Box::pin(handle_receive_pack_stream_with_rejections(
            &repo_path,
            &mut client,
            super::PushPolicy::default(),
            &applied,
        ));

        tokio::select! {
            outcome = &mut push => panic!(
                "the push cannot finish: its response writer never completes ({:?})",
                outcome.map(|outcome| outcome.ref_updates.len())
            ),
            _ = reached_response.notified() => {}
        }
        drop(push);

        assert_landed_without_an_outcome(&repo_path, &applied);
    }
}

#[cfg(test)]
mod ref_advertisement_tests {
    use super::build_ref_list;

    #[test]
    fn receive_pack_does_not_advertise_the_server_private_namespaces() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = crate::test_support::repository_with_server_private_refs(dir.path());

        let refs = build_ref_list(&repo_path).unwrap();
        let names: Vec<&str> = refs.iter().map(|(_, name)| name.as_str()).collect();

        assert_eq!(names, ["HEAD", "refs/heads/main", "refs/tags/v1"]);
    }

    #[test]
    fn receive_pack_keeps_the_protocol_null_ref_for_an_unborn_repository() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("unborn.git");
        gix::init_bare(&repo_path).unwrap();

        let refs = build_ref_list(&repo_path).unwrap();

        assert_eq!(
            refs,
            vec![(
                "0000000000000000000000000000000000000000".to_string(),
                "capabilities^{}".to_string()
            )]
        );
    }

    #[tokio::test]
    async fn receive_pack_stream_rejects_an_unreadable_ref_before_advertising() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("broken-ref.git");
        gix::init_bare(&repo_path).unwrap();
        std::fs::create_dir_all(repo_path.join("refs/heads")).unwrap();
        std::fs::write(repo_path.join("refs/heads/broken"), "not-an-object-id\n").unwrap();
        let (mut server, _client) = tokio::io::duplex(256);

        let error = super::handle_receive_pack_stream_with_rejections(
            &repo_path,
            &mut server,
            super::PushPolicy::default(),
            &super::AppliedRefUpdates::new(),
        )
        .await
        .unwrap_err();

        assert!(
            format!("{error:#}").contains("failed to read a reference"),
            "{error:#}"
        );
    }
}

#[cfg(test)]
mod ref_update_cas_tests {
    use super::{update_ref, NULL_SHA1};

    #[test]
    fn ref_update_compares_the_wire_old_sha_before_publishing() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("cas.git");
        gix::init_bare(&repo_path).unwrap();

        let first = "1".repeat(40);
        let second = "2".repeat(40);
        let stale = "3".repeat(40);

        update_ref(&repo_path, "refs/heads/main", NULL_SHA1, &first).unwrap();
        assert!(
            update_ref(&repo_path, "refs/heads/main", NULL_SHA1, &stale).is_err(),
            "a second creator must not overwrite the reference"
        );

        update_ref(&repo_path, "refs/heads/main", &first, &second).unwrap();
        assert!(
            update_ref(&repo_path, "refs/heads/main", &first, &stale).is_err(),
            "a stale writer must lose the compare-and-swap"
        );

        let actual = gix::open(&repo_path)
            .unwrap()
            .find_reference("refs/heads/main")
            .unwrap()
            .id()
            .to_string();
        assert_eq!(actual, second, "the losing update must not move the ref");
    }
}

#[cfg(test)]
mod rejection_pattern_tests {
    use super::{
        enforce_signed_commit_policies, ref_matches_rejection_pattern,
        signature_is_cryptographically_valid, RefUpdate,
    };
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

    #[test]
    fn signature_verification_distinguishes_invalid_and_unavailable() {
        let gateway = crate::cli_gateway::global_gateway().as_ref().unwrap();
        let success_status = gateway.run(&["--version"], None).unwrap().status;
        let output = |status: &str| crate::cli_gateway::GitOutput {
            stdout: format!("{status}\n").into_bytes(),
            stderr: Vec::new(),
            status: success_status,
            command: "git log --format=%G? -1 fixture".into(),
        };

        assert!(signature_is_cryptographically_valid(&output("G")).unwrap());
        assert!(!signature_is_cryptographically_valid(&output("B")).unwrap());
        assert!(!signature_is_cryptographically_valid(&output("N")).unwrap());
        assert!(signature_is_cryptographically_valid(&output("E")).is_err());
    }

    #[test]
    fn nonzero_signature_command_output_is_an_operational_error() {
        let gateway = crate::cli_gateway::global_gateway().as_ref().unwrap();
        let output = gateway
            .run(&["rev-parse", "--verify", "not-a-real-commit"], None)
            .unwrap();
        assert!(!output.success());

        let error = signature_is_cryptographically_valid(&output).unwrap_err();
        assert!(
            format!("{error:#}").contains("git could not verify commit signature"),
            "non-zero git exit must be an operational verification error: {error:#}"
        );
    }
}

/// The `ng <ref> <message>` line of the per-ref status report is read by anyone
/// who can push. A required-signature check that could not *run* is a server
/// incident, and its detail — the git command line with the absolute repository
/// path, raw `git rev-list` stderr, the `git --version` refusal — belongs in the
/// log, not on that line.
#[cfg(test)]
mod required_signature_message_tests {
    use super::{enforce_signed_commit_policies, RefUpdate, RequiredSignatureError};
    use crate::cli_gateway::GitCliError;
    use crate::test_support::CapturedLogs;
    use std::time::Duration;

    /// A server path shaped like a real deployment, so a leak is unmistakable.
    const SERVER_REPO_PATH: &str = "/srv/plombir-git/repositories/octocat/private-mirror.git";

    /// The exact `GitCliError` a timed-out `git rev-list` produces: the gateway
    /// stores the command line it built, and `build_command_line` puts the
    /// absolute repository path in it.
    fn enumeration_that_timed_out() -> RequiredSignatureError {
        RequiredSignatureError::Enumeration(anyhow::Error::new(GitCliError::Timeout {
            command: format!(
                "-C {SERVER_REPO_PATH} rev-list 1111111111111111111111111111111111111111"
            ),
            timeout: Duration::from_secs(120),
        }))
    }

    #[test]
    fn a_timed_out_enumeration_keeps_the_git_command_line_off_the_status_report() {
        let error = enumeration_that_timed_out();

        // The harness only has teeth if the unsanitized rendering really does
        // carry all three internals.
        let rendered = format!("{error}");
        assert!(rendered.contains("-C "), "{rendered}");
        assert!(rendered.contains(SERVER_REPO_PATH), "{rendered}");
        assert!(rendered.contains("rev-list"), "{rendered}");

        let message = error.receive_pack_message();
        assert!(!message.contains("-C "), "{message}");
        assert!(!message.contains(SERVER_REPO_PATH), "{message}");
        assert!(!message.contains("rev-list"), "{message}");
        assert_eq!(
            message,
            "required-signature check could not run on the server"
        );
    }

    #[test]
    fn an_unavailable_verifier_keeps_the_probe_failure_off_the_status_report() {
        let probe = anyhow::Error::new(GitCliError::NotFound(
            "--version: No such file or directory (os error 2)".to_string(),
        ))
        .context("git command gateway unavailable");
        let error = RequiredSignatureError::Unavailable(format!("{probe:#}"));

        let rendered = format!("{error}");
        assert!(rendered.contains("No such file or directory"), "{rendered}");
        assert!(
            rendered.contains("git command gateway unavailable"),
            "{rendered}"
        );

        let message = error.receive_pack_message();
        assert!(!message.contains("No such file or directory"), "{message}");
        assert!(!message.contains("--version"), "{message}");
        assert_eq!(
            message,
            "required-signature check could not run on the server"
        );
    }

    /// The half the pusher is owed: a commit whose signature was *checked* and
    /// found wanting has to be named, or there is nothing to re-sign.
    #[test]
    fn a_verification_failure_still_names_the_commit_and_nothing_else() {
        let commit = "c0ffee".repeat(6) + "abcd";
        let error = RequiredSignatureError::Verification {
            commit: commit.clone(),
            source: anyhow::anyhow!("-C {SERVER_REPO_PATH} log --format=%G? -1 {commit}: killed"),
        };

        let message = error.receive_pack_message();
        assert!(message.contains(&commit), "{message}");
        assert!(!message.contains(SERVER_REPO_PATH), "{message}");
        assert!(!message.contains("--format"), "{message}");
    }

    /// End to end on the real code path: `git rev-list` refuses an object the
    /// repository does not have, and its stderr must reach the log without
    /// reaching `update.message`.
    #[test]
    fn a_refused_enumeration_reaches_the_log_but_not_the_pusher() {
        let temp = tempfile::tempdir().unwrap();
        let repo_path = temp.path().join("enumeration.git");
        gix::init_bare(&repo_path).unwrap();

        let mut updates = vec![RefUpdate {
            old_sha: "0".repeat(40),
            new_sha: "1".repeat(40),
            refname: "refs/heads/main".into(),
            status: "ok".into(),
            message: String::new(),
        }];

        let rendered = {
            let (logs, _guard) = CapturedLogs::capture();
            enforce_signed_commit_policies(&repo_path, &mut updates, &["refs/heads/main".into()]);
            logs.rendered()
        };

        assert_eq!(updates[0].status, "error");
        let message = &updates[0].message;
        assert_eq!(
            message,
            "required-signature check could not run on the server"
        );
        assert!(!message.contains("fatal"), "{message}");
        assert!(
            !message.contains(&repo_path.display().to_string()),
            "{message}"
        );

        // Diagnostics are not the price of the fix: the same chain the pusher no
        // longer sees is in the log, next to the refname.
        assert!(rendered.contains("refs/heads/main"), "{rendered}");
        assert!(
            rendered.contains("failed to enumerate commits for signature verification"),
            "{rendered}"
        );
        assert!(rendered.contains("bad object"), "{rendered}");
    }
}

#[cfg(test)]
mod wire_tests {
    use super::*;
    use crate::pkt_line::read_pkt_line;
    use std::io::Cursor;
    use tokio::io::BufReader;

    #[test]
    fn receive_negotiation_refuses_wire_bytes_past_its_ceiling() {
        assert_eq!(checked_receive_negotiation_bytes(2, 2, 4).unwrap(), 4);
        let error = checked_receive_negotiation_bytes(4, 1, 4)
            .expect_err("the first byte above the negotiation ceiling must be refused");

        assert!(error.to_string().contains("byte limit"), "{error:#}");
    }

    #[tokio::test]
    async fn pack_copy_refuses_the_first_chunk_past_its_ceiling() {
        let mut reader = Cursor::new(b"12345".to_vec());
        let mut copied = Vec::new();

        let error = copy_pack_with_limit(&mut reader, &mut copied, 4)
            .await
            .expect_err("the fifth byte must be refused");

        assert!(
            error.to_string().contains("configured 4-byte limit"),
            "{error:#}"
        );
        assert!(
            copied.is_empty(),
            "a chunk crossing the ceiling must not be forwarded partially"
        );
    }

    #[tokio::test]
    async fn pack_copy_accepts_exactly_the_ceiling() {
        let mut reader = Cursor::new(b"1234".to_vec());
        let mut copied = Vec::new();

        assert_eq!(
            copy_pack_with_limit(&mut reader, &mut copied, 4)
                .await
                .unwrap(),
            4
        );
        assert_eq!(copied, b"1234");
    }

    /// Encode one pkt-line (`<4-hex-len><payload>`) for building test streams.
    fn pkt(data: &[u8]) -> Vec<u8> {
        let mut out = format!("{:04x}", data.len() + 4).into_bytes();
        out.extend_from_slice(data);
        out
    }

    #[tokio::test]
    async fn report_status_wraps_unpack_and_per_ref_status_in_sideband() {
        // send_response is the report-status writer. It must emit, as band-1
        // sideband data: `unpack ok`, then one `ok <ref>` / `ng <ref> <msg>`
        // line per update, then an embedded flush.
        let results = vec![
            RefUpdate {
                old_sha: "a".repeat(40),
                new_sha: "b".repeat(40),
                refname: "refs/heads/main".into(),
                status: "ok".into(),
                message: "ok".into(),
            },
            RefUpdate {
                old_sha: "c".repeat(40),
                new_sha: "0".repeat(40),
                refname: "refs/heads/bad".into(),
                status: "error".into(),
                message: "deletion not supported".into(),
            },
        ];

        let mut out: Vec<u8> = Vec::new();
        send_response(&mut out, &results).await.unwrap();

        // Outer layer: a single sideband band-1 pkt-line carrying the report.
        let mut outer = BufReader::new(Cursor::new(out));
        let band = match read_pkt_line(&mut outer).await.unwrap() {
            PktLine::Data(d) => d,
            other => panic!("expected sideband Data, got {other:?}"),
        };
        assert_eq!(band[0], 1u8, "report-status must ride on sideband band 1");

        // Inner layer: the report-status pkt-lines.
        let mut inner = BufReader::new(Cursor::new(band[1..].to_vec()));
        assert_eq!(
            read_pkt_line(&mut inner).await.unwrap(),
            PktLine::text("unpack ok"),
            "unpack status must come first"
        );
        assert_eq!(
            read_pkt_line(&mut inner).await.unwrap(),
            PktLine::text("ok refs/heads/main")
        );
        assert_eq!(
            read_pkt_line(&mut inner).await.unwrap(),
            PktLine::text("ng refs/heads/bad deletion not supported")
        );
        assert!(matches!(
            read_pkt_line(&mut inner).await.unwrap(),
            PktLine::Flush
        ));
    }

    #[tokio::test]
    async fn process_push_handles_malformed_commands_without_spawning_indexer() {
        // A garbage line (too few fields) is skipped; a deletion (null target)
        // is reported as an error. With zero surviving `ok` updates, the pack is
        // drained and no `git index-pack` is spawned — so this stays hermetic and
        // must not panic on the hostile first line.
        let repo = tempfile::tempdir().unwrap();

        let mut stream = Vec::new();
        stream.extend_from_slice(&pkt(b"garbage-line-with-one-field\n"));
        let delete = format!("{} {} refs/heads/gone\n", "c".repeat(40), "0".repeat(40));
        stream.extend_from_slice(&pkt(delete.as_bytes()));
        stream.extend_from_slice(b"0000"); // flush → end of update commands

        let mut reader = BufReader::new(Cursor::new(stream));
        let updates = process_push_with_rejections(
            repo.path(),
            &mut reader,
            &PushPolicy::default(),
            &AppliedRefUpdates::new(),
        )
        .await
        .unwrap();

        assert_eq!(updates.len(), 1, "only the deletion produces an update");
        assert_eq!(updates[0].refname, "refs/heads/gone");
        assert_eq!(updates[0].status, "error");
        assert_eq!(updates[0].message, "deletion not supported");
    }

    #[tokio::test]
    async fn process_push_rejects_every_hostile_refname_before_pack_indexing() {
        let repo = tempfile::tempdir().unwrap();
        let mut stream = Vec::new();
        for refname in ["main^", "@{-1}", "refs/heads/-x", "a..b"] {
            let command = format!("{} {} {refname}\n", NULL_SHA1, "a".repeat(40));
            stream.extend_from_slice(&pkt(command.as_bytes()));
        }
        stream.extend_from_slice(b"0000");

        let mut reader = BufReader::new(Cursor::new(stream));
        let updates = process_push_with_rejections(
            repo.path(),
            &mut reader,
            &PushPolicy::default(),
            &AppliedRefUpdates::new(),
        )
        .await
        .unwrap();

        assert_eq!(updates.len(), 4);
        for update in updates {
            assert_eq!(update.status, "error", "{} was accepted", update.refname);
            assert!(
                update.message.contains("refname") || update.message.contains("branch name"),
                "unexpected rejection for {}: {}",
                update.refname,
                update.message
            );
        }
    }

    #[tokio::test]
    async fn process_push_refuses_to_rewrite_non_utf8_ref_bytes() {
        let repo = tempfile::tempdir().unwrap();
        let mut command = format!("{} {} refs/heads/", NULL_SHA1, "a".repeat(40)).into_bytes();
        command.extend_from_slice(&[0xff, b'\n']);

        let mut stream = pkt(&command);
        stream.extend_from_slice(b"0000");
        let mut reader = BufReader::new(Cursor::new(stream));

        let error = process_push_with_rejections(
            repo.path(),
            &mut reader,
            &PushPolicy::default(),
            &AppliedRefUpdates::new(),
        )
        .await
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("must be valid UTF-8"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn process_push_propagates_error_on_non_hex_header() {
        // A malformed pkt-line header in the command stream must surface as Err,
        // never a panic (CWE-755).
        let repo = tempfile::tempdir().unwrap();
        let mut reader = BufReader::new(Cursor::new(Vec::from(b"zzzz".as_slice())));
        let result = process_push_with_rejections(
            repo.path(),
            &mut reader,
            &PushPolicy::default(),
            &AppliedRefUpdates::new(),
        )
        .await;
        assert!(
            result.is_err(),
            "non-hex header must surface as Err, got {result:?}"
        );
    }
}

/// Parity + interrupt tests for the native (`gix_pack`) receive-pack indexer.
///
/// All git subprocesses go through the sanctioned `GitCommandGateway` so the
/// raw-git-invocation guard in `cli_gateway.rs` stays green.
#[cfg(test)]
mod native_index_pack_tests {
    use super::*;
    use std::io::Cursor;
    use std::path::Path;
    use tokio::io::{AsyncWriteExt, BufReader};

    fn gw() -> &'static crate::cli_gateway::GitCommandGateway {
        crate::cli_gateway::global_gateway().as_ref().unwrap()
    }

    /// Run a git command (cwd = `dir` via `-C`), assert success, return trimmed stdout.
    fn git_ok(args: &[&str], dir: &Path) -> String {
        let out = gw().run(args, Some(dir)).unwrap();
        assert!(
            out.success(),
            "git {args:?} failed: {}",
            out.stderr_str().trim()
        );
        out.stdout_str().trim().to_owned()
    }

    /// Run a git command feeding `input` to stdin (cwd = `dir`), return raw stdout bytes.
    async fn git_stdin(args: &[&str], dir: &Path, input: &[u8]) -> (bool, Vec<u8>) {
        let mut child = gw().spawn_async(args, Some(dir)).await.unwrap();
        {
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(input).await.unwrap();
            stdin.shutdown().await.unwrap();
        }
        let out = child.wait_with_output().await.unwrap();
        (out.status.success(), out.stdout)
    }

    /// Sorted list of every object id physically present in `dir` (across all packs + loose).
    fn all_object_ids(dir: &Path) -> Vec<String> {
        let mut ids: Vec<String> = git_ok(
            &[
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname)",
            ],
            dir,
        )
        .lines()
        .map(str::to_owned)
        .collect();
        ids.sort();
        ids
    }

    fn seed_repo(work: &Path) {
        git_ok(&["init", "-q", "-b", "main"], work);
        git_ok(&["config", "user.name", "T"], work);
        git_ok(&["config", "user.email", "t@example.invalid"], work);
        git_ok(&["config", "commit.gpgsign", "false"], work);
    }

    /// Core acceptance test: pushing a **real thin pack** through the native
    /// `write_to_directory` path yields the identical object set + working ref
    /// as `git index-pack --fix-thin`, and the thin-pack base lookup is proven
    /// load-bearing (indexing into a repo lacking the base fails).
    #[tokio::test]
    async fn native_index_pack_thin_parity_with_git_fix_thin() {
        if crate::cli_gateway::global_gateway().is_err() {
            eprintln!("skipping native_index_pack parity: git not available");
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        seed_repo(&work);

        // Commit A: a sizable file so a later 1-line edit deltifies (→ thin pack).
        let base: String = (0..2000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(work.join("data.txt"), &base).unwrap();
        git_ok(&["add", "."], &work);
        git_ok(&["commit", "-q", "-m", "A"], &work);
        let a = git_ok(&["rev-parse", "HEAD"], &work);

        // Three bare targets seeded with EXACTLY A (objects + refs/heads/main → A),
        // cloned before B exists so their object store has only A's objects.
        let work_s = work.to_str().unwrap();
        let t_git = tmp.path().join("t_git.git");
        let t_native = tmp.path().join("t_native.git");
        for dst in [&t_git, &t_native] {
            git_ok(
                &["clone", "-q", "--bare", work_s, dst.to_str().unwrap()],
                tmp.path(),
            );
        }
        // A fresh empty bare repo (NO base objects) to prove the pack is thin.
        let t_empty = tmp.path().join("t_empty.git");
        git_ok(
            &["init", "-q", "--bare", t_empty.to_str().unwrap()],
            tmp.path(),
        );

        // Commit B: small edit → its blob deltifies against A's blob (external base).
        let edited = format!("{base}appended tail line\n");
        std::fs::write(work.join("data.txt"), &edited).unwrap();
        git_ok(&["add", "."], &work);
        git_ok(&["commit", "-q", "-m", "B"], &work);
        let b = git_ok(&["rev-parse", "HEAD"], &work);

        // Build the thin pack: objects in B but not A, deltas against A (not in pack).
        let revs = format!("^{a}\n{b}\n");
        let (ok, thin) = git_stdin(
            &["pack-objects", "--thin", "--revs", "--stdout"],
            &work,
            revs.as_bytes(),
        )
        .await;
        assert!(ok, "pack-objects failed");
        assert!(
            thin.windows(4).any(|w| w == b"PACK"),
            "expected a PACK stream"
        );

        // Path 1 — git index-pack --fix-thin into t_git.
        let (git_ok_status, _) =
            git_stdin(&["index-pack", "--fix-thin", "--stdin"], &t_git, &thin).await;
        assert!(git_ok_status, "git index-pack --fix-thin failed");

        // Path 2 — native indexer into t_native.
        let mut reader = BufReader::new(Cursor::new(thin.clone()));
        index_pack_native(&t_native, &mut reader)
            .await
            .expect("native index-pack should succeed against a repo holding the base");

        // Thin-ness proof: same pack into a repo WITHOUT the base must fail — the
        // thin lookup cannot resolve the external delta base. This deterministically
        // proves both (a) the pack is genuinely thin and (b) our lookup is load-bearing.
        let mut reader_empty = BufReader::new(Cursor::new(thin.clone()));
        let empty_res = index_pack_native(&t_empty, &mut reader_empty).await;
        assert!(
            empty_res.is_err(),
            "indexing a thin pack without its base must fail; pack was not thin"
        );

        // Publish the ref in both real targets, then assert parity.
        for dst in [&t_git, &t_native] {
            git_ok(&["update-ref", "refs/heads/main", &b], dst);
            assert!(
                gw().run(&["cat-file", "-e", &b], Some(dst))
                    .unwrap()
                    .success(),
                "B unreachable in {}",
                dst.display()
            );
            assert!(
                gw().run(&["fsck", "--strict"], Some(dst))
                    .unwrap()
                    .success(),
                "fsck failed in {}",
                dst.display()
            );
        }

        let git_objs = all_object_ids(&t_git);
        let native_objs = all_object_ids(&t_native);
        assert_eq!(
            native_objs, git_objs,
            "native path produced a different object set than git index-pack --fix-thin"
        );
        assert!(git_objs.contains(&b), "B commit object must be present");
        assert!(
            git_objs.len() > all_object_ids(&t_empty).len(),
            "targets must hold more than the empty repo"
        );
    }

    /// A pre-set interrupt flag aborts the unpack promptly instead of indexing —
    /// the mechanism the wall-clock/idle watchdog uses to cancel a runaway push.
    #[tokio::test]
    async fn native_index_pack_respects_preset_interrupt() {
        if crate::cli_gateway::global_gateway().is_err() {
            eprintln!("skipping native_index_pack interrupt: git not available");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        seed_repo(&work);
        std::fs::write(work.join("f.txt"), "hello\n").unwrap();
        git_ok(&["add", "."], &work);
        git_ok(&["commit", "-q", "-m", "c"], &work);
        let head = git_ok(&["rev-parse", "HEAD"], &work);

        // A full (non-thin) pack of HEAD.
        let (ok, pack) = git_stdin(
            &["pack-objects", "--revs", "--stdout"],
            &work,
            format!("{head}\n").as_bytes(),
        )
        .await;
        assert!(ok, "pack-objects failed");

        let target = tmp.path().join("t.git");
        git_ok(
            &["init", "-q", "--bare", target.to_str().unwrap()],
            tmp.path(),
        );

        // Drive write_to_directory directly with an already-tripped interrupt.
        let repo_path = target.clone();
        let interrupt = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let res = tokio::task::spawn_blocking(move || -> Result<()> {
            let repo = gix::open(&repo_path)?;
            let pack_dir = repo_path.join("objects").join("pack");
            std::fs::create_dir_all(&pack_dir)?;
            let mut cursor = Cursor::new(pack);
            let mut progress = gix::progress::Discard;
            let outcome = gix::odb::pack::Bundle::write_to_directory(
                &mut cursor,
                Some(pack_dir.as_path()),
                &mut progress,
                &interrupt,
                Some(&repo),
                Default::default(),
            );
            if interrupt.load(std::sync::atomic::Ordering::Relaxed) {
                bail!("interrupted");
            }
            outcome.map(|_| ()).map_err(Into::into)
        })
        .await
        .unwrap();
        assert!(res.is_err(), "a pre-set interrupt must abort the unpack");
    }
}

/// Per-ref policy a push is held to before any ref moves: the server's own
/// namespaces (card_e62ac71c4768) and the LFS locks other people hold
/// (card_4a40b70a6796). Real repositories and a real `git`, because both
/// checks are about what a client can make the server write.
#[cfg(test)]
mod push_policy_tests {
    use std::io::Cursor;
    use std::path::Path;

    use tokio::io::BufReader;

    use super::{
        process_push_with_rejections, AppliedRefUpdates, ForeignLock, PushPolicy, RefUpdate,
        LFS_LOCK_CHECK_UNAVAILABLE, NULL_SHA1, SERVER_OWNED_NAMESPACE,
    };

    /// See `landed_push_tests::EMPTY_PACK_CHECKSUM`: the objects every fixture
    /// below names are already in the repository, so the pack carries none.
    const EMPTY_PACK_CHECKSUM: &str = "029d08823bd8a8eab510ad6ac75c823cfd3ed31e";

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = crate::cli_gateway::global_gateway()
            .as_ref()
            .expect("git gateway")
            .run_with_env(
                args,
                Some(dir),
                &[
                    ("GIT_AUTHOR_NAME", "fixture"),
                    ("GIT_AUTHOR_EMAIL", "fixture@example.invalid"),
                    ("GIT_COMMITTER_NAME", "fixture"),
                    ("GIT_COMMITTER_EMAIL", "fixture@example.invalid"),
                ],
            )
            .expect("run git");
        assert!(
            output.success(),
            "git {args:?} failed: {}",
            output.stderr_str()
        );
        output.stdout_str().trim().to_string()
    }

    fn commit(work: &Path, file: &str, contents: &str) -> String {
        std::fs::write(work.join(file), contents).unwrap();
        git(work, &["add", file]);
        git(work, &["commit", "-q", "-m", file]);
        git(work, &["rev-parse", "HEAD"])
    }

    /// The served repository the way a push finds it: `main` at `base`, and
    /// whatever else the test hands over present as objects under no branch —
    /// exactly what `index-pack` leaves behind before the refs move.
    struct Served {
        _dir: tempfile::TempDir,
        bare: std::path::PathBuf,
        work: std::path::PathBuf,
        base: String,
    }

    impl Served {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let bare = dir.path().join("served.git");
            let work = dir.path().join("work");
            std::fs::create_dir_all(&work).unwrap();
            git(dir.path(), &["init", "-q", "--bare", "served.git"]);
            git(&work, &["init", "-q", "-b", "main"]);
            let base = commit(&work, "readme.txt", "base\n");
            git(&work, &["push", "-q", &bare.to_string_lossy(), "main"]);
            Self {
                _dir: dir,
                bare,
                work,
                base,
            }
        }

        /// Hand `sha`'s objects to the served repository without a branch or
        /// tag reaching it.
        fn deliver_objects(&self, sha: &str) {
            let scratch = format!("{sha}:refs/fixture/delivered");
            git(
                &self.work,
                &["push", "-q", &self.bare.to_string_lossy(), &scratch],
            );
            git(&self.bare, &["update-ref", "-d", "refs/fixture/delivered"]);
        }

        /// Publish `sha` as `branch` in the served repository.
        fn publish(&self, sha: &str, branch: &str) {
            let refspec = format!("{sha}:refs/heads/{branch}");
            git(
                &self.work,
                &["push", "-q", &self.bare.to_string_lossy(), &refspec],
            );
        }
    }

    fn pkt(data: &[u8]) -> Vec<u8> {
        let mut out = format!("{:04x}", data.len() + 4).into_bytes();
        out.extend_from_slice(data);
        out
    }

    /// A wire push of `commands` (`old new refname`) followed by an empty pack.
    fn push_stream(commands: &[(&str, &str, &str)]) -> Vec<u8> {
        let mut stream = Vec::new();
        for (index, (old, new, refname)) in commands.iter().enumerate() {
            let capabilities = if index == 0 {
                "\0report-status side-band-64k"
            } else {
                ""
            };
            stream.extend_from_slice(&pkt(
                format!("{old} {new} {refname}{capabilities}\n").as_bytes()
            ));
        }
        stream.extend_from_slice(b"0000");
        stream.extend_from_slice(b"PACK");
        stream.extend_from_slice(&2u32.to_be_bytes());
        stream.extend_from_slice(&0u32.to_be_bytes());
        stream.extend_from_slice(
            gix::ObjectId::from_hex(EMPTY_PACK_CHECKSUM.as_bytes())
                .unwrap()
                .as_slice(),
        );
        stream
    }

    async fn push(
        repo: &Path,
        commands: &[(&str, &str, &str)],
        policy: &PushPolicy,
    ) -> Vec<RefUpdate> {
        let mut reader = BufReader::new(Cursor::new(push_stream(commands)));
        process_push_with_rejections(repo, &mut reader, policy, &AppliedRefUpdates::new())
            .await
            .expect("the push itself must run to the end")
    }

    fn outcome<'a>(updates: &'a [RefUpdate], refname: &str) -> &'a RefUpdate {
        updates
            .iter()
            .find(|update| update.refname == refname)
            .unwrap_or_else(|| panic!("no update for {refname} in {updates:?}"))
    }

    fn branch(repo: &Path, name: &str) -> Option<String> {
        let output = crate::cli_gateway::global_gateway()
            .as_ref()
            .unwrap()
            .run(
                &["rev-parse", "--verify", "-q", &format!("refs/heads/{name}")],
                Some(repo),
            )
            .unwrap();
        output
            .success()
            .then(|| output.stdout_str().trim().to_string())
    }

    fn alice_holds(path: &str) -> PushPolicy {
        PushPolicy {
            foreign_locks: vec![ForeignLock {
                path: path.to_string(),
                owner: "alice".to_string(),
            }],
            ..PushPolicy::default()
        }
    }

    #[tokio::test]
    async fn a_push_into_a_server_namespace_is_refused_ref_by_ref() {
        let served = Served::new();
        let next = commit(&served.work, "next.txt", "next\n");
        served.deliver_objects(&next);

        let updates = push(
            &served.bare,
            &[
                (NULL_SHA1, &next, "refs/merge-queue/1"),
                (NULL_SHA1, &next, "refs/forks/x"),
                (NULL_SHA1, &next, "refs/pull/7/head"),
                (NULL_SHA1, &next, &format!("refs/replace/{}", served.base)),
                (&served.base, &next, "refs/heads/main"),
            ],
            &PushPolicy::default(),
        )
        .await;

        let replace = format!("refs/replace/{}", served.base);
        for refname in [
            "refs/merge-queue/1",
            "refs/forks/x",
            "refs/pull/7/head",
            replace.as_str(),
        ] {
            let update = outcome(&updates, refname);
            assert_eq!(update.status, "error", "{refname} was accepted");
            assert_eq!(update.message, SERVER_OWNED_NAMESPACE);
        }
        assert_eq!(outcome(&updates, "refs/heads/main").status, "ok");
        assert_eq!(branch(&served.bare, "main"), Some(next));
        let written = crate::cli_gateway::global_gateway()
            .as_ref()
            .unwrap()
            .run(
                &[
                    "for-each-ref",
                    "refs/merge-queue/",
                    "refs/forks/",
                    "refs/pull/",
                    "refs/replace/",
                ],
                Some(&served.bare),
            )
            .unwrap();
        assert_eq!(
            written.stdout_str().trim(),
            "",
            "a refused server-namespace ref was written anyway"
        );
    }

    #[tokio::test]
    async fn a_commit_changing_someone_else_s_locked_path_refuses_only_its_own_ref() {
        let served = Served::new();
        let free = commit(&served.work, "free.txt", "anyone may edit this\n");
        let locked = commit(&served.work, "castle.level", "bob's castle\n");
        served.deliver_objects(&locked);

        let updates = push(
            &served.bare,
            &[
                (&served.base, &locked, "refs/heads/main"),
                (NULL_SHA1, &free, "refs/heads/free"),
            ],
            &alice_holds("castle.level"),
        )
        .await;

        let main = outcome(&updates, "refs/heads/main");
        assert_eq!(main.status, "error", "a locked path went through");
        assert_eq!(main.message, "path 'castle.level' is locked by alice");
        assert_eq!(branch(&served.bare, "main"), Some(served.base.clone()));
        assert_eq!(
            outcome(&updates, "refs/heads/free").status,
            "ok",
            "a ref whose commits leave the locked path alone was refused with it"
        );
        assert_eq!(branch(&served.bare, "free"), Some(free));
    }

    #[tokio::test]
    async fn commits_a_branch_already_holds_are_not_the_pusher_s_change() {
        let served = Served::new();
        let locked = commit(&served.work, "castle.level", "alice's castle\n");
        // The lock holder's own work, already on a branch of the repository.
        served.publish(&locked, "alice-castle");

        let updates = push(
            &served.bare,
            &[(&served.base, &locked, "refs/heads/main")],
            &alice_holds("castle.level"),
        )
        .await;

        assert_eq!(
            outcome(&updates, "refs/heads/main").status,
            "ok",
            "moving main onto commits a branch already holds was blamed on the pusher"
        );
    }

    /// The lock holds an edit made inside a merge commit — one neither parent
    /// carries — and lets a clean merge of already-pushed work through.
    #[tokio::test]
    async fn a_merge_commit_s_own_edit_of_a_locked_path_is_refused() {
        let served = Served::new();
        let side = commit(&served.work, "free.txt", "side\n");
        served.publish(&side, "side");
        git(&served.work, &["reset", "-q", "--hard", &served.base]);
        let other = commit(&served.work, "other.txt", "other\n");
        served.publish(&other, "other");

        git(
            &served.work,
            &["merge", "-q", "--no-ff", "-m", "clean", &side],
        );
        let clean = git(&served.work, &["rev-parse", "HEAD"]);
        git(&served.work, &["reset", "-q", "--hard", &other]);
        git(
            &served.work,
            &["merge", "-q", "--no-ff", "--no-commit", &side],
        );
        std::fs::write(served.work.join("castle.level"), "edited in the merge\n").unwrap();
        git(&served.work, &["add", "castle.level"]);
        git(&served.work, &["commit", "-q", "-m", "evil"]);
        let evil = git(&served.work, &["rev-parse", "HEAD"]);
        served.deliver_objects(&clean);
        served.deliver_objects(&evil);

        let updates = push(
            &served.bare,
            &[
                (&other, &evil, "refs/heads/other"),
                (NULL_SHA1, &clean, "refs/heads/clean"),
            ],
            &alice_holds("castle.level"),
        )
        .await;

        let refused = outcome(&updates, "refs/heads/other");
        assert_eq!(refused.status, "error", "the merge's own edit went through");
        assert_eq!(refused.message, "path 'castle.level' is locked by alice");
        assert_eq!(branch(&served.bare, "other"), Some(other));
        assert_eq!(
            outcome(&updates, "refs/heads/clean").status,
            "ok",
            "a clean merge of work the repository already holds was refused"
        );
    }

    /// A `refs/replace/<E>` already in the repository — from before such
    /// pushes were refused — must not answer the lock check for `E`: the
    /// branch receives `E`, so `E` is what is read (card_03ed757463d4).
    #[tokio::test]
    async fn a_replacement_in_the_repository_does_not_hide_a_locked_path() {
        let served = Served::new();
        let locked = commit(&served.work, "castle.level", "bob's castle\n");
        git(&served.work, &["reset", "-q", "--hard", &served.base]);
        let decoy = commit(&served.work, "free.txt", "nothing to see\n");
        served.deliver_objects(&locked);
        served.deliver_objects(&decoy);
        git(
            &served.bare,
            &["update-ref", &format!("refs/replace/{locked}"), &decoy],
        );

        // The fixture holds a well-formed replacement; that the check reads
        // past it is what dropping `GIT_NO_REPLACE_OBJECTS` from the gateway
        // turns red.
        assert_eq!(
            git(&served.bare, &["replace", "--list", "--format=long"]),
            format!("{locked} (commit) -> {decoy} (commit)")
        );

        let updates = push(
            &served.bare,
            &[(&served.base, &locked, "refs/heads/main")],
            &alice_holds("castle.level"),
        )
        .await;

        let main = outcome(&updates, "refs/heads/main");
        assert_eq!(main.status, "error", "the replacement hid the locked path");
        assert_eq!(main.message, "path 'castle.level' is locked by alice");
        assert_eq!(branch(&served.bare, "main"), Some(served.base.clone()));
    }

    #[tokio::test]
    async fn a_lock_check_that_cannot_run_refuses_rather_than_admits() {
        let served = Served::new();
        // An object id the repository does not have: `git log` cannot walk
        // from it, which is the check failing, not the pusher.
        let missing = "b".repeat(40);

        let updates = push(
            &served.bare,
            &[(NULL_SHA1, &missing, "refs/heads/ghost")],
            &alice_holds("castle.level"),
        )
        .await;

        let ghost = outcome(&updates, "refs/heads/ghost");
        assert_eq!(ghost.status, "error");
        assert_eq!(ghost.message, LFS_LOCK_CHECK_UNAVAILABLE);
        assert_eq!(branch(&served.bare, "ghost"), None);
    }
}
