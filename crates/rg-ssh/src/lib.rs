//! ForgeKeep SSH server implementation using russh.
//!
//! Phase 2: auth_publickey queries the database for matching SSH keys.
//! auth_password queries the database and verifies via Argon2.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use russh::keys::ssh_key::LineEnding;
use russh::keys::{load_secret_key, Algorithm, PrivateKey};
use russh::server::{Auth, Config, Handler, Msg, Server as _, Session};
use russh::{Channel, ChannelId, ChannelStream};
use sea_orm::DatabaseConnection;
use tokio::io::AsyncWriteExt;

use rg_core::branch_protection::push_rules::{
    branch_protection_rejected_refs, signed_commit_required_refs, tag_protection_rejected_refs,
};
use rg_git::io_timeout::{is_idle_timeout, IdleTimeout};
use rg_git::protocol::receive_pack::{
    handle_receive_pack_stream_with_rejections, ReceivePackOutcome, RefUpdate,
};
use rg_git::protocol::upload_pack::handle_upload_pack_stream;
use rg_git::protocol::v2::handle_v2_stream;

/// Wall-clock guard around a streaming git protocol handler.
///
/// The SSH stream handlers (`handle_upload_pack_stream`, `handle_v2_stream`,
/// `handle_receive_pack_stream_with_rejections`) each spawn a `git`
/// subprocess via [`rg_git::cli_gateway::GitCommandGateway::spawn_async`],
/// which only sets `kill_on_drop(true)` and asks the caller to bound the I/O
/// loop. Without a bound a hung or pathologically slow git process holds the
/// SSH connection + subprocess forever.
///
/// On timeout the inner future is dropped, which drops the `git` child held
/// inside the handler → `kill_on_drop` kills the subprocess. No explicit PID
/// management is needed. This mirrors the HTTP transport's identically-named
/// helper in `rg-http/src/git_http.rs`.
///
/// `secs == 0` disables the bound (the future runs unbounded).
async fn with_git_timeout<T>(
    secs: u64,
    fut: impl std::future::Future<Output = T>,
) -> Result<T, tokio::time::error::Elapsed> {
    if secs == 0 {
        return Ok(fut.await);
    }
    tokio::time::timeout(std::time::Duration::from_secs(secs), fut).await
}

/// Split a finished git session into what the client hears and what the
/// post-push hooks are owed.
///
/// A push is applied inside rg-git — pack indexed, every accepted ref written
/// — *before* the report-status goes back down the channel, so
/// [`ReceivePackOutcome`] carries a delivery failure beside the updates instead
/// of in place of them. The client still gets exit 1 for such a session (it
/// genuinely does not know what happened to its push), but CI, the `push`
/// webhook, the watch fan-out and the open-PR head-SHA refresh are owed all the
/// same: the branch has moved, and a retry carries no objects and is answered
/// `Everything up-to-date`, so there is no second chance to run them
/// (card_abd7384eed60).
fn split_git_session(
    session: Result<Option<ReceivePackOutcome>>,
) -> (Result<()>, Option<Vec<RefUpdate>>) {
    match session {
        Ok(Some(outcome)) => (outcome.report_status, Some(outcome.ref_updates)),
        // Fetch / ls-refs: nothing moved, nothing owed.
        Ok(None) => (Ok(()), None),
        // Failed before the point of no return — no ref was written.
        Err(error) => (Err(error), None),
    }
}

/// Error type for SSH handler.
#[derive(Debug)]
struct HandlerError(String);

impl std::fmt::Display for HandlerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for HandlerError {}

impl From<russh::Error> for HandlerError {
    fn from(e: russh::Error) -> Self {
        HandlerError(e.to_string())
    }
}

impl From<anyhow::Error> for HandlerError {
    fn from(e: anyhow::Error) -> Self {
        HandlerError(format!("{:#}", e))
    }
}

/// A failed SSH git gate, classified before anything is written to the client.
///
/// Git-over-SSH has no HTTP status code or `AppError` envelope, so the error
/// class has to survive until `exec_request` can translate it into stderr +
/// exit-status. In particular, a failed storage lookup is not an access
/// decision and must never become "repository access denied".
#[derive(Debug)]
enum GitServiceError {
    AuthenticationRequired,
    RepositoryNotFound,
    AccessDenied(&'static str),
    ServerUnavailable(anyhow::Error),
}

impl GitServiceError {
    fn client_message(&self) -> &'static str {
        match self {
            Self::AuthenticationRequired => "authentication required",
            Self::RepositoryNotFound => "repository not found",
            Self::AccessDenied(_) => "repository access denied",
            Self::ServerUnavailable(_) => "server temporarily unavailable; try again later",
        }
    }

    fn class(&self) -> &'static str {
        match self {
            Self::AuthenticationRequired => "authentication_required",
            Self::RepositoryNotFound => "repository_not_found",
            Self::AccessDenied(_) => "access_denied",
            Self::ServerUnavailable(_) => "server_unavailable",
        }
    }

    fn log_detail(&self) -> String {
        match self {
            Self::AuthenticationRequired => "no authenticated SSH identity".to_string(),
            Self::RepositoryNotFound => "repository not found".to_string(),
            Self::AccessDenied(reason) => (*reason).to_string(),
            Self::ServerUnavailable(error) => format!("{error:#}"),
        }
    }

    fn is_server_failure(&self) -> bool {
        matches!(self, Self::ServerUnavailable(_))
    }
}

/// SSH server configuration.
pub struct SshServerConfig {
    /// Path to the SSH host key file (e.g., ed25519).
    pub host_key_path: PathBuf,
    /// Address to listen on (e.g., "0.0.0.0:2222").
    pub listen_addr: String,
    /// Root directory for git repositories.
    pub repo_root: PathBuf,
    /// Database connection.
    ///
    /// Not optional, and deliberately so: every gate on the SSH path — key and
    /// password authentication, the per-exec repository permission, maintenance
    /// mode, branch and tag protection — *is* a database read. An absent handle
    /// would therefore not mean "no database", it would mean "no gate". This
    /// used to be an `Option` whose `None` arm was labelled "Phase 1 compat"
    /// and accepted every key and every password (card_6cb7471a52b2); making
    /// the handle mandatory moves the guarantee from five hand-written
    /// `if let Some(db)` sites to the type.
    pub db: DatabaseConnection,
    /// Shared instance settings cache. In the normal HTTP+SSH process this is
    /// the same handle the HTTP admin API updates, so maintenance mode reaches
    /// SSH pushes without a restart.
    pub instance_settings: rg_core::instance::InstanceSettingsCache,
    /// Wall-clock timeout (seconds) for the streaming git transport
    /// (upload-pack / receive-pack / v2). Bounds a hung or pathologically slow
    /// `git` subprocess so a stalled-but-connected SSH client can't hold a
    /// connection + process indefinitely. 0 disables the bound (default: 300).
    /// Mirrors the HTTP transport's `git_stream_timeout_secs`.
    pub git_stream_timeout_secs: u64,
    /// Idle timeout (seconds) layered *on top of* the wall-clock bound: the
    /// git stream is killed if it makes no read/write progress for this long,
    /// even while still under the wall-clock budget. Catches a **slow-drip**
    /// push/fetch that dribbles a byte at a time to pin a `git` subprocess +
    /// connection. 0 disables the idle watchdog (default: 30). See
    /// [`rg_git::io_timeout::IdleTimeout`].
    pub git_idle_timeout_secs: u64,
    /// The process-wide graceful-shutdown signal, when the embedder has one.
    ///
    /// `forgekeep serve` fans a single `watch` channel out to the HTTP server
    /// and every background worker; this transport used to be the one consumer
    /// it never reached, so a `SIGTERM` cut an SSH push mid-objects while the
    /// same push over HTTP was drained (card_5317e172fd25). `None` for a
    /// standalone start or a test — the server then runs until its listener
    /// dies, exactly as before.
    pub shutdown: Option<tokio::sync::watch::Receiver<bool>>,
    /// How long a stopping server waits for its in-flight git sessions before
    /// disconnecting whoever is left. Mirrors the HTTP transport's
    /// `shutdown_grace_secs`, and for the same reason: the process is on a
    /// stopwatch, so the drain has to be bounded.
    pub shutdown_grace_secs: u64,
    /// Post-push automation (CI trigger, webhook fan-out, open-PR head-SHA
    /// refresh, auto-merge / merge-queue evaluation) run after an accepted
    /// `git-receive-pack`.
    ///
    /// The identical hooks the Smart-HTTP transport runs — they were private to
    /// `rg-http` until card_b4fefeee8abf, so a push over SSH silently ran none
    /// of them. `None` disables the hooks (no automation configured); the push
    /// itself still succeeds.
    pub post_push: Option<Arc<rg_core::push_hooks::PostPushContext>>,
}

/// Shared state passed to every SshHandler.
struct SharedState {
    repo_root: Arc<PathBuf>,
    db: Arc<DatabaseConnection>,
    instance_settings: rg_core::instance::InstanceSettingsCache,
    /// Wall-clock bound (seconds) applied around each git streaming handler.
    /// 0 = disabled. See [`SshServerConfig::git_stream_timeout_secs`].
    git_stream_timeout_secs: u64,
    /// Idle bound (seconds) applied to the git stream itself via
    /// [`IdleTimeout`]. 0 = disabled. See
    /// [`SshServerConfig::git_idle_timeout_secs`].
    git_idle_timeout_secs: u64,
    /// Post-push hooks shared with the HTTP transport. See
    /// [`SshServerConfig::post_push`].
    post_push: Option<Arc<rg_core::push_hooks::PostPushContext>>,
    /// Every git session this server is currently streaming.
    ///
    /// The session runs detached — `exec_request` returns as soon as it is
    /// started, because russh needs the handler back — so without a tracker it
    /// is owned by nobody, and a `SIGTERM` severs a `git-receive-pack` in the
    /// middle of writing objects. This is what the shutdown path waits on
    /// before it lets russh disconnect anybody (card_5317e172fd25).
    ///
    /// Its own tracker rather than `rg_core::task_tracker::delivery_tracker()`:
    /// that one is process-global and holds the *aftermath* of a push (webhook,
    /// CI trigger), which has to outlive this transport and is drained once, by
    /// the supervisor, after both transports have stopped.
    git_sessions: rg_core::task_tracker::TaskTracker,
}

/// The ForgeKeep SSH server — implements `russh::server::Server`.
struct SshServer {
    config: Arc<Config>,
    shared: Arc<SharedState>,
    id: usize,
    /// The process-wide stop signal, when the embedder fans one out.
    /// `None` for a standalone start or a test, which is why the wait goes
    /// through [`rg_core::task_tracker::wait_optional_shutdown`].
    shutdown: Option<tokio::sync::watch::Receiver<bool>>,
    /// How long a stopping server waits for its in-flight git sessions.
    shutdown_grace: std::time::Duration,
}

/// Wait for the git sessions this server is streaming, bounded by `grace`.
///
/// `true` when they all finished, `false` when the window ran out with work
/// still in flight — which is the moment the caller stops being polite and lets
/// russh disconnect whoever is left.
///
/// Closing first is what makes the wait terminate: `TaskTracker::wait` resolves
/// once the tracker is closed *and* empty, so an open tracker would sit there
/// until the timeout even with nothing running. A tracker that is already empty
/// is not waited on at all, so an idle server stops immediately instead of
/// pausing for the grace window on every restart.
async fn drain_git_sessions(
    sessions: &rg_core::task_tracker::TaskTracker,
    grace: std::time::Duration,
) -> bool {
    sessions.close();
    if sessions.is_empty() {
        return true;
    }
    tokio::time::timeout(grace, sessions.wait()).await.is_ok()
}

/// Ensure an SSH host key exists at `path`, generating a fresh ed25519 key
/// (written as a PKCS#8 PEM, mode 0600) on first start when the file is
/// missing. This makes zero-config startup possible, mirroring Gitea.
fn ensure_host_key(path: &std::path::Path) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            // Owner-only: this runs on a first start, so the directory is one
            // the server is making rather than one an operator chose, and what
            // it is being made for is the instance's SSH host private key.
            rg_core::platform::fs::create_dir_all_owner_only(parent).map_err(|e| {
                anyhow::anyhow!(rg_core::platform::fs::describe_path_error(
                    "SSH host key directory",
                    parent,
                    &e,
                    "point `--host-key` / `[server].host_key` at a directory the server can write to",
                ))
            })?;
        }
    }
    let key = PrivateKey::random(&mut rand::rng(), Algorithm::Ed25519)
        .context("failed to generate ed25519 host key")?;
    let pem = key
        .to_openssh(LineEnding::LF)
        .context("failed to encode generated host key")?;
    let created = write_new_host_key(path, pem.as_bytes()).map_err(|e| {
        anyhow::anyhow!(rg_core::platform::fs::describe_path_error(
            "SSH host key",
            path,
            &e,
            "the server generates the key on first start and needs write access to its directory",
        ))
    })?;
    if created {
        tracing::info!(path = ?path, "generated new SSH host key (ed25519)");
    } else {
        // The `path.exists()` above and the creation below cannot be one step,
        // so a second start of the same instance can appear in between. It has
        // generated a key of its own by now; keeping the one already on disk is
        // what makes both processes agree on a single host identity.
        tracing::info!(
            path = ?path,
            "another start generated the SSH host key first; keeping the key already on disk"
        );
    }
    Ok(())
}

/// Persist a freshly generated host key, reporting whether this call is the one
/// that created it.
///
/// Owner-only from its first byte, and never an overwrite. A plain
/// `std::fs::write` gets both wrong for this file in particular: it creates
/// under the ambient `umask` (`0644` on a stock host) and leaves narrowing to a
/// separate `chmod`, and the key that would be exposed is the instance's SSH
/// identity — anyone who can read it can answer as this host to every client
/// that has already accepted its fingerprint. The `chmod` is also the step a
/// crash, a `SIGKILL` or a full disk gets to skip, and nothing later takes that
/// back: `ensure_host_key` returns early once the file exists, so a key born
/// `0644` stays `0644` for the life of the instance.
///
/// See [`rg_core::platform::fs::create_new_owner_only`] for why the mode
/// belongs to the `open(2)` and why an existing file is refused rather than
/// replaced.
fn write_new_host_key(path: &std::path::Path, pem: &[u8]) -> std::io::Result<bool> {
    use std::io::Write;

    let mut file = match rg_core::platform::fs::create_new_owner_only(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    };
    file.write_all(pem)?;
    // A host key that reached the page cache but not the disk is a key the next
    // boot does not have, while every client that connected in between has
    // accepted its fingerprint.
    file.sync_all()?;
    Ok(true)
}

/// Fail loudly *before* handing the path to russh when the host key is not a
/// readable regular file.
///
/// Both failure modes are one-way tickets to an unhelpful message otherwise:
/// a bind-mount whose source file was missing leaves a **directory** behind
/// (russh then reports a parse failure), and a key owned by a different uid —
/// the norm for a container, whose `forgekeep` user is not the host's
/// `forgekeep` user — surfaces as a bare `Permission denied (os error 13)`
/// with no clue which uid to chown to.
fn check_host_key_readable(path: &std::path::Path) -> Result<()> {
    let metadata = std::fs::metadata(path).map_err(|e| {
        anyhow::anyhow!(rg_core::platform::fs::describe_path_error(
            "SSH host key",
            path,
            &e,
            "point `--host-key` / `[server].host_key` at an existing OpenSSH private key",
        ))
    })?;
    if metadata.is_dir() {
        anyhow::bail!(
            "SSH host key path `{}` is a directory, not a file — a Docker bind-mount \
             likely auto-created it because the source file was missing; remove the directory \
             and let the server generate a key there, or bind-mount an existing key file",
            path.display()
        );
    }
    // `File::open` on a directory succeeds on Linux, so this check must come
    // after the is_dir() one to be meaningful.
    std::fs::File::open(path).map_err(|e| {
        anyhow::anyhow!(rg_core::platform::fs::describe_path_error(
            "SSH host key",
            path,
            &e,
            "the host key must be readable by the server process (mode 0600, owned by it)",
        ))
    })?;
    // "this process can read it" was as far as the promise in the name went,
    // and the two are not the same question: a host key another local account
    // can read lets that account impersonate this server to every git client
    // that has already trusted its fingerprint. `ensure_host_key` generates the
    // key `0600`, so only a key supplied from outside — a bind-mount, a restore,
    // a hand-copied file — can arrive wider than that. OpenSSH refuses the same
    // file for the same reason.
    rg_core::platform::fs::ensure_owner_only(path, "SSH host key")?;
    Ok(())
}

impl SshServer {
    /// Create a new SSH server from configuration.
    /// Loads the host key and validates the key file permissions.
    pub fn new(ssh_config: SshServerConfig) -> Result<Self> {
        ensure_host_key(&ssh_config.host_key_path)?;
        check_host_key_readable(&ssh_config.host_key_path)?;
        let host_key = load_secret_key(&ssh_config.host_key_path, None).with_context(|| {
            format!(
                "failed to load SSH host key {}",
                ssh_config.host_key_path.display()
            )
        })?;

        let config = Config {
            auth_rejection_time: std::time::Duration::from_secs(1),
            auth_rejection_time_initial: Some(std::time::Duration::from_secs(0)),
            keys: vec![host_key],
            ..Default::default()
        };

        let shared = Arc::new(SharedState {
            repo_root: Arc::new(ssh_config.repo_root),
            db: Arc::new(ssh_config.db),
            instance_settings: ssh_config.instance_settings,
            git_stream_timeout_secs: ssh_config.git_stream_timeout_secs,
            git_idle_timeout_secs: ssh_config.git_idle_timeout_secs,
            post_push: ssh_config.post_push,
            git_sessions: rg_core::task_tracker::TaskTracker::new(),
        });

        Ok(Self {
            config: Arc::new(config),
            shared,
            id: 0,
            shutdown: ssh_config.shutdown,
            shutdown_grace: std::time::Duration::from_secs(ssh_config.shutdown_grace_secs),
        })
    }

    /// Run the SSH server on the given address. Blocks until the server stops.
    pub async fn run(&mut self, listen_addr: &str) -> Result<()> {
        let addr: std::net::SocketAddr = listen_addr
            .parse()
            .with_context(|| format!("invalid listen address: {}", listen_addr))?;

        // Keep address-based production startup and the pre-bound harness on
        // one lifecycle path. Calling russh's `run_on_address` directly here
        // used to bypass the only shutdown select and session drain below.
        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("failed to bind SSH listener at {listen_addr}"))?;
        self.run_on_listener(&listener).await
    }

    /// Run on a listener that the caller has already bound.
    ///
    /// Returns when the listener dies or when the embedder asks the process to
    /// stop — and in the second case only after the in-flight git sessions have
    /// been given the grace window. The caller is expected to *await* this, so
    /// the answer means "this transport has stopped touching the database".
    async fn run_on_listener(&mut self, listener: &tokio::net::TcpListener) -> Result<()> {
        let listen_addr = listener
            .local_addr()
            .context("failed to read bound SSH listener address")?;

        tracing::info!(%listen_addr, "Starting SSH server");

        // Read out before `run_on_socket` borrows `self` for the server's
        // lifetime.
        let config = self.config.clone();
        let sessions = self.shared.git_sessions.clone();
        let grace = self.shutdown_grace;
        let mut shutdown = self.shutdown.clone();

        let mut server = self.run_on_socket(config, listener);
        let handle = server.handle();

        tokio::select! {
            result = &mut server => return result.context("SSH server error"),
            () = rg_core::task_tracker::wait_optional_shutdown(&mut shutdown) => {}
        }

        // The order matters. russh's own shutdown stops the accept loop *and*
        // disconnects every live session at once, so asking for it first would
        // cut exactly the push this drain exists to protect. Drain first, then
        // disconnect whatever outlasted the window.
        tracing::info!(
            grace_secs = grace.as_secs(),
            "shutdown signal received — draining in-flight git SSH sessions"
        );
        if drain_git_sessions(&sessions, grace).await {
            tracing::info!("in-flight git SSH sessions drained on shutdown");
        } else {
            tracing::warn!(
                grace_secs = grace.as_secs(),
                "git SSH sessions did not finish within the grace window — disconnecting"
            );
        }
        handle.shutdown("server is shutting down".to_string());
        // Bounded too: `RunningServer` resolves once its accept loop sees the
        // broadcast, but a wedged peer must not hold the process open past the
        // window the operator configured.
        match tokio::time::timeout(grace, server).await {
            Ok(result) => result.context("SSH server error")?,
            Err(_) => tracing::warn!("SSH server did not stop within the grace window"),
        }
        Ok(())
    }
}

impl russh::server::Server for SshServer {
    type Handler = SshHandler;

    fn new_client(&mut self, peer: Option<std::net::SocketAddr>) -> Self::Handler {
        let handler = SshHandler {
            shared: self.shared.clone(),
            id: self.id,
            peer,
            channel: None,
            authenticated_identity: None,
            git_protocol_version: "1".to_string(),
        };
        self.id += 1;
        handler
    }

    fn handle_session_error(&mut self, error: <Self::Handler as Handler>::Error) {
        tracing::error!("Session error: {:?}", error);
    }
}

/// russh Handler implementation for ForgeKeep.
/// One SshHandler per client connection.
struct SshHandler {
    shared: Arc<SharedState>,
    id: usize,
    /// Client address, recorded against rejected password attempts so a run of
    /// failures in `login_log` names where it came from.
    peer: Option<std::net::SocketAddr>,
    /// The channel opened by the client for this session.
    channel: Option<Channel<Msg>>,
    /// Repository-scoped identity resolved during authentication.
    authenticated_identity: Option<AuthenticatedIdentity>,
    /// Git protocol version requested by the client (default: "1").
    /// Set to "2" when the client sends GIT_PROTOCOL=version=2 via env_request.
    git_protocol_version: String,
}

/// Per-push state the `git-receive-pack` branch needs beyond the byte stream:
/// the protection rules enforced before the refs move, and the `owner`/`repo`
/// the post-push hooks are keyed on.
struct ReceivePackContext {
    protection_rules: Vec<rg_db::ops::protected_branch_ops::Rule>,
    tag_protection_rules: Vec<rg_db::ops::protected_tag_ops::Rule>,
    /// Pusher's user id, or `None` for a deploy key (protection rules treat an
    /// unidentified actor as "not on any allow-list").
    actor_id: Option<i64>,
    owner: String,
    repo_name: String,
}

/// What spoke for an account on this connection — and therefore what can take
/// the connection back.
///
/// The two arms are revoked by different acts, and that is the whole reason
/// they are told apart rather than folded into one nullable key id: a key is a
/// durable credential of its own, a password is what a session generation is
/// counted from.
#[derive(Clone, Debug)]
enum UserCredential {
    /// The SSH key row that matched. Revoked by deleting the key — and
    /// deliberately *not* by a password change: a registered key is a standing
    /// credential in its own right, the same way a personal access token is
    /// (see `rg_db::ops::user_ops::invalidate_sessions`).
    SshKey { key_id: i64 },
    /// A password, together with the `users.session_version` it was accepted
    /// under. A password reset or a `POST /users/logout` bumps that column, and
    /// a connection carrying the older generation stops being served.
    Password { session_version: i64 },
}

/// Who a session speaks for — and, just as importantly, *what row* said so.
///
/// The credential is carried rather than dropped because authentication
/// happens once and a connection carries any number of execs: without it the
/// only thing a later exec could re-read is the account, and a revoked key —
/// or a session the owner has since ended — would keep working for as long as
/// the connection stayed open.
#[derive(Clone, Debug)]
enum AuthenticatedIdentity {
    User {
        user_id: i64,
        credential: UserCredential,
    },
    /// A deploy key, named by its row alone: the repository it opens and
    /// whether it may write are re-read on every exec, so caching them here
    /// would only be a second, staler copy.
    DeployKey { key_id: i64 },
}

impl AuthenticatedIdentity {
    fn user_id(&self) -> Option<i64> {
        match self {
            Self::User { user_id, .. } => Some(*user_id),
            Self::DeployKey { .. } => None,
        }
    }
}

/// Reject an accepted SSH exec with a useful, sanitized git-facing failure.
///
/// `channel_failure` only says that the exec request itself was rejected; git
/// gets no stderr and normally reports an opaque SSH failure. Accepting the
/// exec and then sending stderr + exit-status 1 mirrors a short-lived command
/// that failed normally, which is both observable by humans and retryable by
/// automation. The internal error chain remains in the operator log only.
fn reject_git_exec(
    session: &mut Session,
    channel_id: ChannelId,
    error: &GitServiceError,
    identity: Option<&AuthenticatedIdentity>,
    service: &str,
    repo_path: &str,
) -> Result<(), HandlerError> {
    let detail = error.log_detail();
    let class = error.class();
    if error.is_server_failure() {
        tracing::error!(
            error = %detail,
            %class,
            ?identity,
            %service,
            %repo_path,
            "SSH git request failed before the git process could start"
        );
    } else {
        tracing::warn!(
            error = %detail,
            %class,
            ?identity,
            %service,
            %repo_path,
            "SSH git request rejected"
        );
    }

    session.channel_success(channel_id)?;
    session.extended_data(channel_id, 1, format!("{}\n", error.client_message()))?;
    session.exit_status_request(channel_id, 1)?;
    session.close(channel_id)?;
    Ok(())
}

impl Handler for SshHandler {
    type Error = HandlerError;

    // CRITICAL: Auth::Reject must include `partial_success: false` (pitfall #5)
    //
    // russh's `Auth::Reject` has a field `partial_success: bool`.
    // If this is `true`, the server tells the client "you partially succeeded,
    // try other methods". This can cause:
    //   - Infinite auth loops
    //   - Clients reporting "partial success" errors
    //   - Unexpected behavior where auth should have been a clear reject
    //
    // Always use `partial_success: false` unless you specifically implement
    // multi-method partial auth (which we don't).
    //
    // Also: `fingerprint()` REQUIRES `HashAlg::Sha256` argument.
    // Without it, the method signature doesn't match (it requires the alg).
    // The returned Fingerprint displays as "SHA256:<base64>" which is
    // the standard format for authorized_keys.

    async fn auth_publickey(
        &mut self,
        _user: &str,
        public_key: &russh::keys::PublicKey,
    ) -> Result<Auth, Self::Error> {
        let db = &*self.shared.db;

        // Compute SHA-256 fingerprint. ssh_key is a transitive dep of russh via
        // internal-russh-forked-ssh-key. The fingerprint() method returns a
        // Fingerprint that implements Display as "SHA256:<base64>".
        use russh::keys::ssh_key;
        let fp = public_key.fingerprint(ssh_key::HashAlg::Sha256);
        let fp_str = fp.to_string();
        tracing::debug!(fingerprint = %fp_str, "SSH pubkey auth attempt");

        match rg_db::ops::ssh_key_ops::find_by_fingerprint(db, &fp_str).await {
            Ok(Some(key)) => {
                // The key alone says nothing about its owner. Without this the
                // longest-lived door into a deactivated account stays wide
                // open: the offboarded developer keeps pushing over SSH until
                // somebody remembers to delete the key by hand.
                match rg_db::ops::user_ops::finalize_standing_credential_owner(db, key.user_id)
                    .await
                {
                    Ok(Some(owner)) if owner.is_usable() => {}
                    Ok(_) => {
                        tracing::warn!(
                            user_id = key.user_id,
                            "SSH pubkey auth rejected: account is disabled or gone"
                        );
                        return Ok(Auth::Reject {
                            proceed_with_methods: None,
                            partial_success: false,
                        });
                    }
                    Err(error) => {
                        tracing::error!(
                            user_id = key.user_id,
                            key_id = key.id,
                            error = %format!("{error:#}"),
                            "DB error during SSH key-owner finalization"
                        );
                        return Ok(Auth::Reject {
                            proceed_with_methods: None,
                            partial_success: false,
                        });
                    }
                }
                self.authenticated_identity = Some(AuthenticatedIdentity::User {
                    user_id: key.user_id,
                    credential: UserCredential::SshKey { key_id: key.id },
                });
                if let Err(error) = rg_db::ops::ssh_key_ops::touch_last_used(db, key.id).await {
                    tracing::warn!(
                        key_id = key.id,
                        error = %format!("{error:#}"),
                        "failed to update SSH key usage time"
                    );
                }
                tracing::info!(user_id = key.user_id, "SSH pubkey auth accepted");
                Ok(Auth::Accept)
            }
            Ok(None) => match rg_db::ops::deploy_key_ops::find_by_fingerprint(db, &fp_str).await {
                Ok(Some(key)) => {
                    self.authenticated_identity =
                        Some(AuthenticatedIdentity::DeployKey { key_id: key.id });
                    if let Err(error) =
                        rg_db::ops::deploy_key_ops::touch_last_used(db, key.id).await
                    {
                        tracing::warn!(
                            key_id = key.id,
                            error = %format!("{error:#}"),
                            "failed to update deploy key usage time"
                        );
                    }
                    tracing::info!(
                        repo_id = key.repo_id,
                        read_only = key.read_only,
                        "SSH deploy key auth accepted"
                    );
                    Ok(Auth::Accept)
                }
                Ok(None) => {
                    tracing::warn!(fingerprint = %fp_str, "SSH pubkey not found");
                    Ok(Auth::Reject {
                        proceed_with_methods: None,
                        partial_success: false,
                    })
                }
                Err(error) => {
                    tracing::error!(error = %format!("{error:#}"), "DB error during deploy-key lookup");
                    Ok(Auth::Reject {
                        proceed_with_methods: None,
                        partial_success: false,
                    })
                }
            },
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "DB error during pubkey lookup");
                Ok(Auth::Reject {
                    proceed_with_methods: None,
                    partial_success: false,
                })
            }
        }
    }

    async fn auth_password(&mut self, username: &str, password: &str) -> Result<Auth, Self::Error> {
        let db = &*self.shared.db;

        let found = match rg_db::ops::user_ops::find_by_username(db, username).await {
            Ok(user) => user,
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "DB error during password auth");
                None
            }
        };
        // Verified even when there is no such user, so that a rejection always
        // costs one Argon2 hash — an early return here would let an attacker
        // enumerate accounts by how fast the server says no.
        let password_ok = match rg_core::auth::password::verify_password_or_dummy(
            password,
            found.as_ref().map(|user| user.password_hash.as_str()),
        ) {
            Ok(verdict) => verdict,
            Err(error) => {
                // SSH has no way to say "this is our fault, retry later" — the
                // client only ever learns accept or reject. So the log line is
                // the whole remedy, and it has to name the account: without it a
                // hash broken by a migration is indistinguishable from a user
                // who keeps mistyping their password.
                tracing::error!(
                    username,
                    user_id = ?found.as_ref().map(|user| user.id),
                    error = %format!("{error:#}"),
                    "cannot verify SSH password: stored hash is unusable"
                );
                false
            }
        };

        // The lockout is applied after the hash, never before: an early exit
        // for a locked account would answer "is this account locked?" — and so
        // "does it exist?" — through the response time. The same helper runs on
        // the registry's `docker login`, so the SSH port can no longer be used
        // to walk past a threshold the web login enforces.
        let peer_ip = self.peer.map(|peer| peer.ip().to_string());
        let attempt = rg_core::auth::lockout::settle_password_attempt(
            db,
            found.as_ref(),
            password_ok,
            rg_core::auth::lockout::AttemptOrigin {
                login: username,
                channel: "ssh",
                ip_address: peer_ip.as_deref(),
                user_agent: None,
            },
        )
        .await;

        match attempt {
            Ok(rg_core::auth::lockout::PasswordAttempt::Accepted(account)) => {
                // The generation is read from the row this password was checked
                // against by the lifecycle finalizer, so a reset racing the
                // credential lookup cannot make this session stale at birth.
                self.authenticated_identity = Some(AuthenticatedIdentity::User {
                    user_id: account.id,
                    credential: UserCredential::Password {
                        session_version: account.session_version,
                    },
                });
                tracing::info!(username, "SSH password auth accepted");
                Ok(Auth::Accept)
            }
            Ok(rg_core::auth::lockout::PasswordAttempt::SecondFactorRequired) => {
                // The password was right. SSH has no way to prompt for a TOTP
                // code — `auth_password` may only accept or reject — so the
                // account authenticates here with the credential that is a
                // standing second factor already: its SSH key. The log line is
                // the whole explanation the owner can be given, since the
                // client is told nothing but "no".
                tracing::warn!(
                    username,
                    "SSH password auth refused: the account requires a second factor, \
                     which this door cannot ask for — authenticate with an SSH key"
                );
                Ok(Auth::Reject {
                    proceed_with_methods: None,
                    partial_success: false,
                })
            }
            Ok(rg_core::auth::lockout::PasswordAttempt::Rejected { locked }) => {
                tracing::warn!(username, locked, "SSH password auth rejected");
                Ok(Auth::Reject {
                    proceed_with_methods: None,
                    partial_success: false,
                })
            }
            Err(error) => {
                tracing::error!(
                    username,
                    error = %format!("{error:#}"),
                    "SSH password verified but its account lifecycle could not be finalized"
                );
                Ok(Auth::Reject {
                    proceed_with_methods: None,
                    partial_success: false,
                })
            }
        }
    }

    async fn auth_keyboard_interactive(
        &mut self,
        _user: &str,
        _submethods: &str,
        _response: Option<russh::server::Response<'_>>,
    ) -> Result<Auth, Self::Error> {
        Ok(Auth::Reject {
            proceed_with_methods: None,
            partial_success: false,
        })
    }

    async fn env_request(
        &mut self,
        _channel_id: ChannelId,
        name: &str,
        value: &str,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name == "GIT_PROTOCOL" && value.contains("version=2") {
            tracing::info!(%value, "SSH client requested Git Protocol V2");
            self.git_protocol_version = "2".to_string();
        }
        // Accept env request (git needs GIT_PROTOCOL)
        Ok(())
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        _session: &mut Session,
    ) -> Result<bool, Self::Error> {
        tracing::debug!(id = self.id, channel_id = ?channel.id(), "channel_open_session");
        self.channel = Some(channel);
        Ok(true)
    }

    async fn exec_request(
        &mut self,
        channel_id: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).to_string();
        tracing::info!(%command, id = self.id, "SSH exec request");

        let (service, repo_path) = parse_git_command(&command)?;

        // H-02: Validate repo_path before joining with repo_root
        rg_core::platform::validate_repo_path(&repo_path)
            .with_context(|| format!("invalid repository path: {}", repo_path))?;

        let db = &*self.shared.db;

        if let Err(error) = authorize_git_service(
            db,
            &service,
            &repo_path,
            self.authenticated_identity.as_ref(),
        )
        .await
        {
            reject_git_exec(
                session,
                channel_id,
                &error,
                self.authenticated_identity.as_ref(),
                &service,
                &repo_path,
            )?;
            return Ok(());
        }

        if service == "git-receive-pack" {
            let settings = self.shared.instance_settings.get(db).await;
            if settings.maintenance_mode {
                let msg =
                    "Instance is in maintenance mode. SSH push is disabled; read-only access only.";
                tracing::warn!(
                    identity = ?self.authenticated_identity,
                    %repo_path,
                    "SSH git receive-pack rejected by maintenance mode"
                );
                session.channel_success(channel_id)?;
                session.extended_data(channel_id, 1, format!("{msg}\n"))?;
                session.exit_status_request(channel_id, 1)?;
                session.close(channel_id)?;
                return Ok(());
            }
        }

        let receive_pack_context = if service == "git-receive-pack" {
            match load_receive_pack_context(
                db,
                &repo_path,
                self.authenticated_identity
                    .as_ref()
                    .and_then(AuthenticatedIdentity::user_id),
            )
            .await
            {
                Ok(context) => Some(context),
                Err(error) => {
                    reject_git_exec(
                        session,
                        channel_id,
                        &error,
                        self.authenticated_identity.as_ref(),
                        &service,
                        &repo_path,
                    )?;
                    return Ok(());
                }
            }
        } else {
            None
        };

        let repo_full_path = {
            let p = self.shared.repo_root.join(&repo_path);
            if p.exists() {
                p
            } else {
                let with_git = self.shared.repo_root.join(format!("{}.git", repo_path));
                if with_git.exists() {
                    with_git
                } else {
                    let err_msg = format!("repository not found: {}", repo_path);
                    tracing::error!(%err_msg);
                    session.channel_failure(channel_id)?;
                    return Err(HandlerError(err_msg));
                }
            }
        };

        let ch = match self.channel.take() {
            Some(ch) => ch,
            None => {
                let msg = "no channel available for exec_request";
                tracing::error!(msg);
                session.channel_failure(channel_id)?;
                return Err(HandlerError(msg.into()));
            }
        };

        session.channel_success(channel_id)?;

        let handle = session.handle();
        let service_name = service.clone();
        let git_protocol_version = self.git_protocol_version.clone();
        let git_stream_timeout_secs = self.shared.git_stream_timeout_secs;
        let git_idle_timeout_secs = self.shared.git_idle_timeout_secs;

        // Post-push hook inputs, captured before `receive_pack_context` moves
        // into the streaming future below.
        let post_push = self.shared.post_push.clone();
        let hook_db = self.shared.db.clone();
        let hook_repo_path = repo_full_path.clone();
        let hook_target = receive_pack_context
            .as_ref()
            .map(|context| (context.owner.clone(), context.repo_name.clone()));
        // The pushing account, for the watch fan-out inside the hooks. `None`
        // for a deploy key, which speaks for a repository rather than a person.
        let hook_pusher_id = receive_pack_context
            .as_ref()
            .and_then(|context| context.actor_id);

        // Tracked, not bare: `exec_request` has to hand the handler back to
        // russh, so this future is detached — and a detached `git-receive-pack`
        // is severed mid-objects by a `SIGTERM` unless the shutdown path can
        // find it. See `SharedState::git_sessions`.
        self.shared.git_sessions.spawn(async move {
            tracing::info!(%service_name, path = %repo_full_path.display(), "Starting git SSH session");

            // Two watchdogs guard the streaming git session:
            //   1. Idle timeout — wrapped *around the network stream itself* via
            //      `IdleTimeout`, so a slow-drip peer that dribbles bytes to stay
            //      under the wall-clock budget still trips as soon as a single
            //      read/write goes quiet for `git_idle_timeout_secs`. The trip is
            //      an `io::Error(TimedOut)` bubbling out of the handler, dropping
            //      the `git` child → `kill_on_drop` reaps it.
            //   2. Wall-clock bound — `with_git_timeout` around the whole handler
            //      (below), capping *total* time.
            // `stream` stays wrapped throughout; the wrapper forwards
            // `shutdown()` to the channel, so exit-status + close still work.
            let mut stream: IdleTimeout<ChannelStream<Msg>> =
                IdleTimeout::from_secs(ch.into_stream(), git_idle_timeout_secs);

            // The `git` child lives *inside* this future (spawned via
            // `spawn_async`'s `kill_on_drop(true)`), so an elapsed wall-clock
            // timeout drops the future → drops the child → kills git. We still
            // own `stream` afterwards (the borrow ends when the future is
            // dropped), so we can report the exit status + shut the channel down
            // cleanly below.
            //
            // It resolves to `Ok(Some(outcome))` only for an accepted push —
            // that is what feeds the post-push hooks; fetch / ls-refs give
            // `Ok(None)`.
            let handler_fut = async {
                match service_name.as_str() {
                    // Protocol V2 defines no `receive-pack` command — git itself
                    // downgrades a push to v0 — so a push always takes the V1
                    // path, `GIT_PROTOCOL=version=2` env request or not. Routing
                    // it into the V2 handler would answer a send-pack client with
                    // a capability advertisement and drop the ref updates on the
                    // floor, taking every post-push hook with them.
                    "git-receive-pack" => {
                        // Loaded above for exactly this service, so a `None`
                        // here is an internal inconsistency and not a mode.
                        // The fallback this used to take ran the push through
                        // the plain receive-pack handler — that is, with no
                        // branch and no tag protection at all — which is not a
                        // safe way to be wrong. The HTTP transport has no such
                        // arm either (card_6cb7471a52b2).
                        let context = receive_pack_context
                            .context("receive-pack accepted without its protection context")?;
                        let require_signed_refs =
                            signed_commit_required_refs(&context.protection_rules);
                        // A rule whose stored allow-list does not decode
                        // aborts the session with a server error: rejecting
                        // the ref instead would blame the pusher for a
                        // broken row, and would be flatly wrong for a
                        // pusher who is on that list.
                        let mut rejected_refs = branch_protection_rejected_refs(
                            context.protection_rules,
                            context.actor_id,
                        )?;
                        rejected_refs.extend(tag_protection_rejected_refs(
                            context.tag_protection_rules,
                            context.actor_id,
                        )?);
                        let outcome = handle_receive_pack_stream_with_rejections(
                            &repo_full_path,
                            &mut stream,
                            rejected_refs,
                            require_signed_refs,
                        )
                        .await?;
                        Ok(Some(outcome))
                    }
                    "git-upload-pack" if git_protocol_version == "2" => {
                        tracing::info!(%service_name, "Using Protocol V2");
                        handle_v2_stream(&repo_full_path, &mut stream)
                            .await
                            .map(|_| None)
                    }
                    "git-upload-pack" => handle_upload_pack_stream(&repo_full_path, &mut stream)
                        .await
                        .map(|_| None),
                    _ => Err(anyhow::anyhow!("Unknown git service: {}", service_name)),
                }
            };

            let (result, applied_ref_updates): (Result<(), anyhow::Error>, Option<Vec<RefUpdate>>) =
                split_git_session(
                    match with_git_timeout(git_stream_timeout_secs, handler_fut).await {
                        Ok(session) => session,
                        Err(_elapsed) => {
                            tracing::warn!(
                                %service_name,
                                timeout_secs = git_stream_timeout_secs,
                                "git SSH session exceeded wall-clock timeout — killed git, closing channel"
                            );
                            Err(anyhow::anyhow!("git operation timed out"))
                        }
                    },
                );

            // ── Post-push hooks: CI, webhooks, PR head-SHA ─────────
            //
            // The same hooks the Smart-HTTP transport runs, from the same
            // `rg-core` entry point. Detached so the client isn't held while CI
            // is triggered and the webhooks fan out — but *tracked*: the client
            // is about to get its exit status, so a bare `tokio::spawn` would
            // be severed by a SIGTERM in the next few seconds with no pipeline,
            // no webhook and no trace that any of it was owed.
            // `delivery_tracker()` is drained by `rg_http::run` after it stops
            // accepting, the same contract the HTTP push path relies on.
            //
            // Outside the `result` branch by design — see `split_git_session`:
            // a push that landed owes these hooks even when the client is about
            // to get exit 1 for a report-status that never reached it.
            if let (Some(ref_updates), Some(hooks), Some((owner, repo_name))) =
                (applied_ref_updates, post_push, hook_target)
            {
                let delivery_tracker = hooks.delivery_tracker.clone();
                delivery_tracker.spawn(async move {
                    hooks
                        .run(
                            &hook_db,
                            &hook_repo_path,
                            &owner,
                            &repo_name,
                            hook_pusher_id,
                            &ref_updates,
                        )
                        .await;
                });
            }

            let exit_code: u32 = if result.is_ok() { 0 } else { 1 };

            match &result {
                Ok(_) => tracing::info!(%service_name, "Git SSH session complete"),
                // An idle-timeout trip surfaces as an `io::Error(TimedOut)` from
                // the stream wrapper; log it distinctly from an ordinary handler
                // failure (git was already killed via `kill_on_drop` when the
                // handler future returned Err). Exit code is 1 either way.
                Err(e) if is_idle_timeout(e) => tracing::warn!(
                    %service_name,
                    idle_timeout_secs = git_idle_timeout_secs,
                    "git SSH session idle (no read/write progress within idle window) — killed git, closing channel"
                ),
                Err(e) => {
                    tracing::error!(error = %format!("{e:#}"), %service_name, "Git SSH session failed")
                }
            }

            // CRITICAL: SSH stream shutdown order (pitfall)
            //
            // Must send exit_status BEFORE shutting down the stream.
            // The russh client expects to receive the exit-status message before
            // the channel is closed. If we shutdown the stream first, the
            // exit_status message may be lost, causing the client to report
            // "connection closed unexpectedly" or exit code 255.
            //
            // Correct order:
            //   1. Send exit_status to client
            //   2. Shutdown the stream (which sends SSH_MSG_CHANNEL_CLOSE)
            //   3. Drop the channel (happens automatically when tokio::spawn future completes)
            if let Err(e) = handle.exit_status_request(channel_id, exit_code).await {
                tracing::warn!(error = ?e, "failed to send exit_status to client");
            }

            // Now safe to shutdown the stream - client has received exit_status
            if let Err(e) = stream.shutdown().await {
                tracing::warn!(error = ?e, "failed to shutdown SSH stream");
            }
        });

        Ok(())
    }
}

/// Parse a git SSH command string like:
///   `git-upload-pack '/owner/repo'`
///   `git-receive-pack '/owner/repo.git'`
fn parse_git_command(command: &str) -> Result<(String, String)> {
    let parts: Vec<&str> = command.splitn(2, ' ').collect();
    if parts.len() < 2 {
        anyhow::bail!("invalid git command: {}", command);
    }

    let service = parts[0].trim().to_string();
    if service != "git-upload-pack" && service != "git-receive-pack" {
        anyhow::bail!("unsupported git command: {}", service);
    }

    let raw_path = parts[1].trim();
    let repo_path = raw_path
        .trim_start_matches('\'')
        .trim_end_matches('\'')
        .trim_start_matches('"')
        .trim_end_matches('"')
        .trim_start_matches('/')
        .to_string();

    Ok((service, repo_path))
}

fn parse_repo_owner_name(repo_path: &str) -> Result<(String, String)> {
    let normalized = repo_path
        .trim_start_matches('/')
        .trim_end_matches('/')
        .strip_suffix(".git")
        .unwrap_or(repo_path.trim_start_matches('/').trim_end_matches('/'));

    let mut parts = normalized.split('/');
    let owner = parts.next().unwrap_or_default();
    let repo_name = parts.next().unwrap_or_default();

    if owner.is_empty() || repo_name.is_empty() || parts.next().is_some() {
        anyhow::bail!("repository path must be owner/repo: {}", repo_path);
    }

    Ok((owner.to_string(), repo_name.to_string()))
}

async fn authorize_git_service(
    db: &DatabaseConnection,
    service: &str,
    repo_path: &str,
    identity: Option<&AuthenticatedIdentity>,
) -> std::result::Result<(), GitServiceError> {
    let identity = identity.ok_or(GitServiceError::AuthenticationRequired)?;
    let (owner, repo_name) =
        parse_repo_owner_name(repo_path).map_err(|_| GitServiceError::RepositoryNotFound)?;
    let repo = rg_core::repo::service::find_repo_by_owner_name(db, &owner, &repo_name)
        .await
        .map_err(GitServiceError::ServerUnavailable)?
        .ok_or(GitServiceError::RepositoryNotFound)?;

    // "Who are you" is re-asked here, next to "what may you do", so the two
    // cannot drift apart: `can_read_repo` / `can_write_repo` re-read the
    // permission on every exec but know nothing about `is_active` /
    // `deleted_at`, and the identity itself was resolved once, at
    // authentication. One connection carries any number of execs — under
    // `ControlMaster`, or a held `ssh -N`, that once was the whole lifetime of
    // an offboarded developer's access.
    //
    // The owner check is deliberately the *last* proof in the user branch.
    // A snapshot before the key and repository reads leaves a gap in which
    // retirement can finish and this function can still publish `Ok(())` from
    // the stale actor. The conditional owner finalizer below is the ordering
    // boundary which closes that gap.
    let allowed = match identity {
        AuthenticatedIdentity::User {
            user_id,
            credential,
        } => {
            match credential {
                // Deleting the key is the other half of offboarding, and the
                // owner is re-checked with it: an id reused after a deletion
                // must not reopen the door on somebody else's behalf.
                UserCredential::SshKey { key_id } => {
                    if rg_db::ops::ssh_key_ops::find_by_id(db, *key_id)
                        .await
                        .map_err(GitServiceError::ServerUnavailable)?
                        .is_none_or(|key| key.user_id != *user_id)
                    {
                        return Err(GitServiceError::AccessDenied(
                            "the SSH key this session authenticated with is gone",
                        ));
                    }
                }
                // Password-session standing is checked against the fresh row
                // returned by the final owner boundary below.
                UserCredential::Password { .. } => {}
            }
            let repository_allowed = match service {
                "git-upload-pack" => {
                    rg_core::repo::service::can_read_repo(db, &repo, Some(*user_id))
                        .await
                        .map_err(GitServiceError::ServerUnavailable)?
                }
                "git-receive-pack" => {
                    rg_core::repo::service::can_write_repo(db, &repo, Some(*user_id))
                        .await
                        .map_err(GitServiceError::ServerUnavailable)?
                }
                _ => false,
            };
            if !repository_allowed {
                return Err(GitServiceError::AccessDenied(
                    "insufficient repository permission",
                ));
            }

            let account =
                match rg_db::ops::user_ops::finalize_standing_credential_owner(db, *user_id)
                    .await
                    .map_err(GitServiceError::ServerUnavailable)?
                {
                    Some(user) if user.is_usable() => user,
                    _ => return Err(GitServiceError::AccessDenied("account is disabled or gone")),
                };
            // Nothing is deleted when the owner resets their password or logs
            // out — the only record of it is that `session_version` moved.
            // Compare it on the same fresh row which finalized account standing.
            if let UserCredential::Password { session_version } = credential {
                if account.session_version != *session_version {
                    return Err(GitServiceError::AccessDenied(
                        "this session ended when the account's password was reset or it logged out",
                    ));
                }
            }
            true
        }
        AuthenticatedIdentity::DeployKey { key_id } => {
            // Re-read rather than trust the cached pair: a key that was
            // narrowed to read-only, or re-pointed, has to take effect on the
            // next exec and not on the next connection.
            let key = rg_db::ops::deploy_key_ops::find_by_id(db, *key_id)
                .await
                .map_err(GitServiceError::ServerUnavailable)?
                .ok_or(GitServiceError::AccessDenied(
                    "the deploy key this session authenticated with is gone",
                ))?;
            deploy_key_allows(key.repo_id, key.read_only, repo.id, service)
        }
    };

    if !allowed {
        return Err(GitServiceError::AccessDenied(
            "insufficient repository permission",
        ));
    }

    Ok(())
}

/// Build the extra receive-pack policy context without letting a failed lookup
/// tear down the SSH connection as an opaque handler error.
async fn load_receive_pack_context(
    db: &DatabaseConnection,
    repo_path: &str,
    actor_id: Option<i64>,
) -> std::result::Result<ReceivePackContext, GitServiceError> {
    let (owner, repo_name) =
        parse_repo_owner_name(repo_path).map_err(|_| GitServiceError::RepositoryNotFound)?;
    let repo = rg_core::repo::service::find_repo_by_owner_name(db, &owner, &repo_name)
        .await
        .map_err(GitServiceError::ServerUnavailable)?
        .ok_or(GitServiceError::RepositoryNotFound)?;
    let protection_rules = rg_db::ops::protected_branch_ops::list_rules_by_repo(db, repo.id)
        .await
        .map_err(GitServiceError::ServerUnavailable)?;
    let tag_protection_rules = rg_db::ops::protected_tag_ops::list_rules_by_repo(db, repo.id)
        .await
        .map_err(GitServiceError::ServerUnavailable)?;

    Ok(ReceivePackContext {
        protection_rules,
        tag_protection_rules,
        actor_id,
        owner,
        repo_name,
    })
}

fn deploy_key_allows(
    key_repo_id: i64,
    read_only: bool,
    requested_repo_id: i64,
    service: &str,
) -> bool {
    key_repo_id == requested_repo_id
        && (service == "git-upload-pack" || (service == "git-receive-pack" && !read_only))
}

/// Public entry point to start the SSH server.
pub async fn start_ssh_server(config: SshServerConfig) -> Result<()> {
    let addr = config.listen_addr.clone();
    let mut server = SshServer::new(config)?;
    server.run(&addr).await
}

/// Start the SSH server on a listener that is already bound.
///
/// Binding before spawning the server removes the port-reservation race from
/// embedders and test harnesses. It is also the right entry point for callers
/// that must bind while they still hold elevated privileges.
pub async fn start_ssh_server_on_listener(
    config: SshServerConfig,
    listener: tokio::net::TcpListener,
) -> Result<()> {
    let mut server = SshServer::new(config)?;
    server.run_on_listener(&listener).await
}

#[cfg(test)]
mod tests {
    use super::{
        check_host_key_readable, deploy_key_allows, drain_git_sessions, ensure_host_key,
        parse_git_command, parse_repo_owner_name, split_git_session, with_git_timeout,
        write_new_host_key, ReceivePackOutcome, RefUpdate,
    };
    use std::time::Duration;

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    fn one_production_call(
        source: &str,
        function: &str,
        name: &str,
    ) -> Result<rust_source::CallSite, String> {
        let calls = rust_source::production_function_call_sites(source, function, &[name]);
        match calls.as_slice() {
            [call] => Ok(*call),
            _ => Err(format!(
                "expected one `{name}` call in production `{function}`, found {}",
                calls.len()
            )),
        }
    }

    fn ssh_post_push_tracking_contract(source: &str) -> Result<(), String> {
        let spawn = one_production_call(source, "exec_request", "delivery_tracker.spawn")?;
        let run = one_production_call(source, "exec_request", "run")?;
        if !rust_source::call_site_contains(source, spawn, run) {
            return Err(format!(
                "post-push run at lib.rs:{} is outside the tracked spawn at lib.rs:{}",
                run.line, spawn.line
            ));
        }
        Ok(())
    }

    fn without_production_call(source: &str, function: &str, name: &str) -> String {
        let call = one_production_call(source, function, name).expect("mutation target must exist");
        let name_at = source[..call.open_paren]
            .rfind(name)
            .expect("call name must precede its opening parenthesis");
        let mut mutated = source.to_owned();
        mutated.replace_range(name_at..name_at + name.len(), &" ".repeat(name.len()));
        mutated
    }

    /// The post-push hooks must stay on the *tracked* spawn path.
    ///
    /// `ssh_push_hook_tests` asserts the effect, but a bare `tokio::spawn`
    /// usually still finishes before that test reads the row, so the regression
    /// it guards is timing-dependent by nature. This one is not: only what the
    /// tracker owns can be drained on shutdown, so an untracked spawn here is
    /// the bug regardless of how the race lands. Mirrors the HTTP transport's
    /// `post_push_hooks_are_detached_through_the_delivery_tracker`.
    #[test]
    fn post_push_hooks_are_detached_through_the_delivery_tracker() {
        let source = include_str!("lib.rs");
        ssh_post_push_tracking_contract(source).unwrap_or_else(|error| panic!("{error}"));

        for name in ["delivery_tracker.spawn", "run"] {
            let mutated = without_production_call(source, "exec_request", name);
            assert!(
                ssh_post_push_tracking_contract(&mutated).is_err(),
                "removing `{name}` from production `exec_request` must fail this guard"
            );
        }
    }

    #[test]
    fn post_push_tracking_guard_ignores_non_code_decoys() {
        const SOURCE: &str = r####"
fn exec_request() {
    // delivery_tracker.spawn(hooks.run());
    let normal = "delivery_tracker.spawn(hooks.run())";
    let raw = r#"delivery_tracker.spawn(hooks.run())"#;
    let bytes = b"delivery_tracker.spawn(hooks.run())";
    let raw_bytes = br##"delivery_tracker.spawn(hooks.run())"##;
    delivery_tracker.spawn(async move {
        hooks.run();
    });
}

#[cfg(test)]
mod tests {
    fn decoy() {
        delivery_tracker.spawn(hooks.run());
    }
}
"####;

        ssh_post_push_tracking_contract(SOURCE).unwrap_or_else(|error| panic!("{error}"));
        let spawn = one_production_call(SOURCE, "exec_request", "delivery_tracker.spawn")
            .expect("live fixture spawn");
        let run = one_production_call(SOURCE, "exec_request", "run").expect("live fixture run");
        assert!(
            spawn.line < run.line,
            "fixture call lines must stay aligned to the original source"
        );

        for name in ["delivery_tracker.spawn", "run"] {
            let mutated = without_production_call(SOURCE, "exec_request", name);
            assert!(
                ssh_post_push_tracking_contract(&mutated).is_err(),
                "raw-source mutation must beat every retained non-code `{name}` decoy"
            );
        }
    }

    fn contains_identifier_bounded(haystack: &str, needle: &str) -> bool {
        haystack.match_indices(needle).any(|(at, _)| {
            let before = haystack[..at].chars().next_back();
            let after = haystack[at + needle.len()..].chars().next();
            !before.is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
                && !after.is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
        })
    }

    fn ssh_database_handle_contract(source: &str) -> Result<(), String> {
        let compact: String = rust_source::production_rust_code_only(source)
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();

        for forbidden in [
            "Option<DatabaseConnection>",
            "Option<Arc<DatabaseConnection>>",
        ] {
            if contains_identifier_bounded(&compact, forbidden) {
                return Err(format!(
                    "`{forbidden}` is back in the SSH server: an optional database handle turns \
                     every gate on this path into an `if let Some(db)` whose else-branch accepts \
                     everything (card_6cb7471a52b2)"
                ));
            }
        }
        Ok(())
    }

    /// The SSH database handle must stay mandatory.
    ///
    /// Every gate on this path is a database read — key auth, password auth,
    /// the per-exec repository permission, maintenance mode, branch and tag
    /// protection. So an optional handle here never described "a server
    /// without a database"; it described a server without authentication and
    /// without authorization, and that is what it did: the `None` arm was
    /// labelled "Phase 1 compat" and answered `Auth::Accept` to any key and
    /// any password (card_6cb7471a52b2).
    ///
    /// Nothing in the tree ever constructed that arm, which is precisely why
    /// no behavioural test can reach it — reintroducing the optionality is
    /// invisible until the day somebody writes the first `None`. The type is
    /// therefore the only place the guarantee can be checked, and this is that
    /// check.
    #[test]
    fn the_ssh_database_handle_cannot_be_made_optional_again() {
        let source = include_str!("lib.rs");
        ssh_database_handle_contract(source).unwrap_or_else(|error| panic!("{error}"));

        for forbidden in [
            "Option<DatabaseConnection>",
            "Option<Arc<DatabaseConnection>>",
        ] {
            let mutated = source.replacen(
                "pub db: DatabaseConnection,",
                &format!("pub db: {forbidden},"),
                1,
            );
            assert!(
                ssh_database_handle_contract(&mutated).is_err(),
                "returning the production database field to `{forbidden}` must fail the guard"
            );
        }
    }

    #[test]
    fn ssh_database_handle_guard_ignores_non_code_and_test_only_decoys() {
        const SOURCE: &str = r####"
// type LineComment = Option<DatabaseConnection>;
/* type BlockComment = Option<Arc<DatabaseConnection>>; */
const NORMAL: &str = "Option<DatabaseConnection>";
const RAW: &str = r#"Option<Arc<DatabaseConnection>>"#;
const BYTES: &[u8] = b"Option<DatabaseConnection>";
const RAW_BYTES: &[u8] = br##"Option<Arc<DatabaseConnection>>"##;
type NotOption = NotOption<DatabaseConnection>;

struct Config {
    pub db: DatabaseConnection,
}

#[cfg(test)]
mod tests {
    type Owned = Option<DatabaseConnection>;
    type Shared = Option<Arc<DatabaseConnection>>;
}
"####;

        ssh_database_handle_contract(SOURCE).unwrap_or_else(|error| panic!("{error}"));
    }

    /// The bind-mount trap: `docker compose up` with a missing `./ssh_host_key`
    /// leaves a *directory* at the mount point. `ensure_host_key` sees an
    /// existing path and steps aside, so the check must name the real cause
    /// instead of letting russh report a key-parse failure.
    #[test]
    fn host_key_directory_is_reported_as_a_bind_mount_mistake() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("ssh_host_key");
        std::fs::create_dir(&key_path).unwrap();

        let err = check_host_key_readable(&key_path).unwrap_err().to_string();

        assert!(err.contains("is a directory, not a file"), "{err}");
        assert!(err.contains("bind-mount"), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_host_key_reports_the_uid_and_the_fix() {
        use std::os::unix::fs::PermissionsExt;

        // root ignores the permission bits, so this can only be observed as a
        // regular user.
        if unsafe { libc::geteuid() } == 0 {
            return;
        }

        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("ssh_host_key");
        ensure_host_key(&key_path).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o000)).unwrap();

        let err = check_host_key_readable(&key_path).unwrap_err().to_string();

        assert!(err.contains("this process runs as uid="), "{err}");
        assert!(err.contains("chmod") || err.contains("chown"), "{err}");
    }

    /// A key the server can read is not the same as a key only the server can
    /// read. Anyone else who can read this file can answer as this host to
    /// every git client that has already accepted its fingerprint, so the check
    /// that carries "readable" in its name has to cover both halves.
    #[cfg(unix)]
    #[test]
    fn a_group_or_world_readable_host_key_is_refused_until_it_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("ssh_host_key");
        ensure_host_key(&key_path).unwrap();
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let err = check_host_key_readable(&key_path).unwrap_err().to_string();

        assert!(err.contains(&key_path.display().to_string()), "{err}");
        assert!(err.contains("mode 0644"), "{err}");
        assert!(err.contains("chmod 600"), "{err}");

        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        check_host_key_readable(&key_path).expect("the same owner-only key must be accepted");
    }

    #[test]
    fn missing_host_key_is_generated_with_owner_only_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("nested").join("ssh_host_key");

        ensure_host_key(&key_path).unwrap();

        assert!(key_path.is_file());
        check_host_key_readable(&key_path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&key_path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    /// The key must be owner-only *as created*, not owner-only after a second
    /// step. Under a stock `umask` a `std::fs::write` lands on `0644`, and the
    /// `chmod` that used to follow is what a crash, a `SIGKILL` or a full disk
    /// gets to skip — after which nothing narrows it, because the generator
    /// only ever runs on a first start.
    #[cfg(unix)]
    #[test]
    fn a_generated_host_key_is_owner_only_from_its_first_byte() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("ssh_host_key");

        assert!(
            write_new_host_key(&key_path, b"PRIVATE KEY").unwrap(),
            "the first write is the one that creates the key"
        );

        let mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "the host key was created {mode:04o}, so any other local account on this host can \
             answer as this server to every client that trusts its fingerprint"
        );
    }

    /// Two first starts of one instance race between `path.exists()` and the
    /// write. Both generate a key; the second must not replace the first, which
    /// by then may already have advertised its fingerprint to a client. A plain
    /// `std::fs::write` truncates instead, leaving the two processes serving
    /// different host identities from the same path.
    #[test]
    fn a_second_start_keeps_the_host_key_the_first_one_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("ssh_host_key");

        write_new_host_key(&key_path, b"the key clients already trust").unwrap();
        let created = write_new_host_key(&key_path, b"a second, competing host identity").unwrap();

        assert!(
            !created,
            "the second call must report that it created nothing"
        );
        assert_eq!(
            std::fs::read(&key_path).unwrap(),
            b"the key clients already trust",
            "the host identity on disk was replaced by a losing concurrent start"
        );
    }

    #[test]
    fn parses_git_command_with_quoted_repo_path() {
        let (service, path) = parse_git_command("git-upload-pack '/alice/project.git'").unwrap();

        assert_eq!(service, "git-upload-pack");
        assert_eq!(path, "alice/project.git");
    }

    #[test]
    fn parses_repo_owner_name_without_git_suffix() {
        let (owner, repo) = parse_repo_owner_name("alice/project").unwrap();

        assert_eq!(owner, "alice");
        assert_eq!(repo, "project");
    }

    #[test]
    fn parses_repo_owner_name_with_git_suffix() {
        let (owner, repo) = parse_repo_owner_name("alice/project.git").unwrap();

        assert_eq!(owner, "alice");
        assert_eq!(repo, "project");
    }

    #[test]
    fn rejects_nested_repo_paths_for_db_permission_lookup() {
        assert!(parse_repo_owner_name("alice/team/project").is_err());
    }

    #[test]
    fn deploy_keys_are_repository_scoped_and_respect_read_only() {
        assert!(deploy_key_allows(7, true, 7, "git-upload-pack"));
        assert!(!deploy_key_allows(7, true, 7, "git-receive-pack"));
        assert!(deploy_key_allows(7, false, 7, "git-receive-pack"));
        assert!(!deploy_key_allows(7, false, 8, "git-upload-pack"));
        assert!(!deploy_key_allows(7, false, 8, "git-receive-pack"));
    }

    #[tokio::test]
    async fn with_git_timeout_elapses_on_slow_future() {
        // An SSH git streaming handler slower than the wall-clock bound must
        // elapse so the caller kills git + closes the channel with exit != 0.
        let res = with_git_timeout(1, async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            42
        })
        .await;
        assert!(res.is_err(), "slow future should elapse");
    }

    fn landed_update() -> RefUpdate {
        RefUpdate {
            old_sha: "0".repeat(40),
            new_sha: "a".repeat(40),
            refname: "refs/heads/main".to_string(),
            status: "ok".to_string(),
            message: "ok".to_string(),
        }
    }

    /// The branch has moved and only the report-status was lost. The client is
    /// still owed exit 1 — it does not know what happened to its push — but the
    /// hooks are owed the update, because the retry that would re-trigger them
    /// carries no objects and is answered `Everything up-to-date`
    /// (card_abd7384eed60).
    #[test]
    fn a_push_whose_report_status_died_still_yields_its_ref_updates() {
        let (result, applied) = split_git_session(Ok(Some(ReceivePackOutcome {
            ref_updates: vec![landed_update()],
            report_status: Err(anyhow::anyhow!("client hung up")),
        })));

        assert!(
            result.is_err(),
            "the client never got its report-status, so the session is a failure to it"
        );
        assert_eq!(
            applied
                .as_deref()
                .map(|updates| updates.iter().map(|u| u.refname.as_str()).collect()),
            Some(vec!["refs/heads/main"]),
            "the hooks are owed the ref updates of a push that already landed"
        );
    }

    /// The other three shapes a finished session takes.
    #[test]
    fn a_delivered_push_a_fetch_and_an_early_failure_split_the_expected_way() {
        let (result, applied) = split_git_session(Ok(Some(ReceivePackOutcome {
            ref_updates: vec![landed_update()],
            report_status: Ok(()),
        })));
        assert!(result.is_ok());
        assert_eq!(applied.map(|updates| updates.len()), Some(1));

        let (result, applied) = split_git_session(Ok(None));
        assert!(result.is_ok());
        assert!(applied.is_none(), "a fetch owes no post-push hooks");

        let (result, applied) = split_git_session(Err(anyhow::anyhow!("pack indexing failed")));
        assert!(result.is_err());
        assert!(
            applied.is_none(),
            "a session that failed before the point of no return wrote no ref"
        );
    }

    #[tokio::test]
    async fn with_git_timeout_passes_fast_future() {
        let res = with_git_timeout(30, async { 7 }).await;
        assert_eq!(res.ok(), Some(7), "fast future should complete, not elapse");
    }

    #[tokio::test]
    async fn with_git_timeout_zero_disables_bound() {
        // 0 = opt out: the future runs unbounded and its value passes through.
        let res = with_git_timeout(0, async { 5 }).await;
        assert_eq!(res.ok(), Some(5));
    }

    /// Read a process's state char from `/proc/<pid>/stat`, or `None` if it no
    /// longer exists. `'Z'` = zombie (killed, awaiting reap). `comm` may contain
    /// spaces/parens, so split on the last `)`.
    #[cfg(target_os = "linux")]
    fn proc_state(pid: u32) -> Option<char> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after = stat.rsplit_once(')')?.1;
        after
            .split_whitespace()
            .next()
            .and_then(|s| s.chars().next())
    }

    /// The core anti-zombie guarantee: the git subprocess lives *inside* the
    /// future, so when `with_git_timeout` drops that future on elapse,
    /// `kill_on_drop` reaps it — it must not keep running. Emulated here with a
    /// long `sleep` child (the SSH git handlers spawn their child the same
    /// `kill_on_drop` way via `spawn_async`).
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn with_git_timeout_kills_child_on_elapse() {
        use tokio::io::AsyncReadExt;

        let child = tokio::process::Command::new("sleep")
            .arg("60")
            .stdout(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn sleep");
        let pid = child.id().expect("child pid");
        assert!(
            proc_state(pid).is_some(),
            "child should be alive before timeout"
        );

        // The child is owned *inside* the future, mirroring the real handlers.
        let res = with_git_timeout(1, async move {
            let mut child = child;
            let mut buf = Vec::new();
            // This never completes within the bound — the child sleeps 60s.
            if let Some(mut out) = child.stdout.take() {
                drop(out.read_to_end(&mut buf).await);
            }
            buf
        })
        .await;
        assert!(res.is_err(), "slow child future should elapse");

        // After the future is dropped, kill_on_drop must have killed the child.
        // Poll briefly — reaping is asynchronous.
        let mut killed = false;
        for _ in 0..50 {
            match proc_state(pid) {
                None | Some('Z') => {
                    killed = true;
                    break;
                }
                _ => tokio::time::sleep(Duration::from_millis(20)).await,
            }
        }
        assert!(
            killed,
            "child must be killed/zombie after timeout drop, not still running"
        );
    }

    /// The half of the stop path that decides whether a push survives a
    /// `SIGTERM`. Before it existed, the git session was a bare `tokio::spawn`
    /// nobody owned: the runtime went down and `git-receive-pack` was severed
    /// in the middle of writing objects, while the same push over HTTP was
    /// drained (card_5317e172fd25).
    #[tokio::test]
    async fn a_session_still_streaming_is_waited_for_within_the_grace_window() {
        let sessions = rg_core::task_tracker::TaskTracker::new();
        let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = finished.clone();
        sessions.spawn(async move {
            tokio::time::sleep(Duration::from_millis(80)).await;
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });

        assert!(
            drain_git_sessions(&sessions, Duration::from_secs(5)).await,
            "a session that finishes inside the window must be reported as drained"
        );
        assert!(
            finished.load(std::sync::atomic::Ordering::SeqCst),
            "the drain returned before the session it was waiting for had finished"
        );
    }

    /// The other half: the wait is bounded. A wedged peer must not hold the
    /// process open past the window the operator configured — the caller
    /// disconnects on `false`.
    #[tokio::test]
    async fn a_session_that_outlasts_the_window_ends_the_wait_rather_than_the_process() {
        let sessions = rg_core::task_tracker::TaskTracker::new();
        sessions.spawn(async { tokio::time::sleep(Duration::from_secs(30)).await });

        assert!(
            !drain_git_sessions(&sessions, Duration::from_millis(50)).await,
            "the drain must give up on a session that outlasts the grace window"
        );
    }

    /// An idle server stops at once. Closing is what makes `wait()` terminate at
    /// all, and skipping the wait on an empty tracker is what keeps every
    /// ordinary restart from pausing for the whole grace window.
    #[tokio::test]
    async fn an_idle_server_does_not_pause_for_the_grace_window() {
        let sessions = rg_core::task_tracker::TaskTracker::new();
        let started = tokio::time::Instant::now();

        assert!(drain_git_sessions(&sessions, Duration::from_secs(30)).await);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "an empty tracker was waited on: {:?}",
            started.elapsed()
        );
    }

    /// The public address-based entry point used by ordinary `forgekeep serve`
    /// must reach the same shutdown-aware accept loop as the pre-bound listener
    /// entry point used by the integration harness.
    ///
    /// A signal published before startup removes socket/readiness races from
    /// this regression: the production entry point still has to bind a real
    /// listener, then observe the pending signal and return. The old
    /// `SshServer::run` called russh's `run_on_address` directly, bypassing the
    /// only `tokio::select!` that listened for shutdown, so it timed out here.
    #[tokio::test]
    async fn production_start_entrypoint_observes_a_pending_shutdown() {
        let dir = tempfile::tempdir().expect("create SSH shutdown test directory");
        let db_path = dir.path().join("test.db");
        let db = rg_db::connect_with_pool(
            &format!("sqlite://{}?mode=rwc", db_path.display()),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            60,
            2,
        )
        .await
        .expect("connect SSH shutdown test database");
        rg_db::run_migrations(&db)
            .await
            .expect("migrate SSH shutdown test database");

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        shutdown_tx
            .send(true)
            .expect("publish shutdown before starting the SSH server");

        let result = tokio::time::timeout(
            Duration::from_secs(3),
            super::start_ssh_server(super::SshServerConfig {
                host_key_path: dir.path().join("host_ed25519"),
                listen_addr: "127.0.0.1:0".to_string(),
                repo_root: dir.path().join("repos"),
                db,
                instance_settings: Default::default(),
                git_stream_timeout_secs: 300,
                git_idle_timeout_secs: 30,
                shutdown: Some(shutdown_rx),
                shutdown_grace_secs: 1,
                post_push: None,
            }),
        )
        .await
        .expect("production SSH entry point ignored the pending shutdown signal");

        result.expect("production SSH entry point stopped cleanly");
    }
}
