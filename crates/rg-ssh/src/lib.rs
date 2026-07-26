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
    handle_receive_pack_stream, handle_receive_pack_stream_with_rejections,
};
use rg_git::protocol::upload_pack::handle_upload_pack_stream;
use rg_git::protocol::v2::handle_v2_stream;

/// Wall-clock guard around a streaming git protocol handler.
///
/// The SSH stream handlers (`handle_upload_pack_stream`, `handle_v2_stream`,
/// `handle_receive_pack_stream{,_with_rejections}`) each spawn a `git`
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

/// SSH server configuration.
pub struct SshServerConfig {
    /// Path to the SSH host key file (e.g., ed25519).
    pub host_key_path: PathBuf,
    /// Address to listen on (e.g., "0.0.0.0:2222").
    pub listen_addr: String,
    /// Root directory for git repositories.
    pub repo_root: PathBuf,
    /// Database connection (None = open access, Phase 1 compat).
    pub db: Option<DatabaseConnection>,
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
}

/// Shared state passed to every SshHandler.
struct SharedState {
    repo_root: Arc<PathBuf>,
    db: Option<Arc<DatabaseConnection>>,
    /// Wall-clock bound (seconds) applied around each git streaming handler.
    /// 0 = disabled. See [`SshServerConfig::git_stream_timeout_secs`].
    git_stream_timeout_secs: u64,
    /// Idle bound (seconds) applied to the git stream itself via
    /// [`IdleTimeout`]. 0 = disabled. See
    /// [`SshServerConfig::git_idle_timeout_secs`].
    git_idle_timeout_secs: u64,
}

/// The ForgeKeep SSH server — implements `russh::server::Server`.
struct SshServer {
    config: Arc<Config>,
    shared: Arc<SharedState>,
    id: usize,
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
            std::fs::create_dir_all(parent).map_err(|e| {
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
    std::fs::write(path, pem.as_bytes()).map_err(|e| {
        anyhow::anyhow!(rg_core::platform::fs::describe_path_error(
            "SSH host key",
            path,
            &e,
            "the server generates the key on first start and needs write access to its directory",
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set host key permissions: {:?}", path))?;
    }
    tracing::info!(path = ?path, "generated new SSH host key (ed25519)");
    Ok(())
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
            db: ssh_config.db.map(Arc::new),
            git_stream_timeout_secs: ssh_config.git_stream_timeout_secs,
            git_idle_timeout_secs: ssh_config.git_idle_timeout_secs,
        });

        Ok(Self {
            config: Arc::new(config),
            shared,
            id: 0,
        })
    }

    /// Run the SSH server on the given address. Blocks until the server stops.
    pub async fn run(&mut self, listen_addr: &str) -> Result<()> {
        let addr: std::net::SocketAddr = listen_addr
            .parse()
            .with_context(|| format!("invalid listen address: {}", listen_addr))?;

        tracing::info!(%listen_addr, "Starting SSH server");
        self.run_on_address(self.config.clone(), addr)
            .await
            .context("SSH server error")?;

        Ok(())
    }
}

impl russh::server::Server for SshServer {
    type Handler = SshHandler;

    fn new_client(&mut self, peer: Option<std::net::SocketAddr>) -> Self::Handler {
        let handler = SshHandler {
            shared: self.shared.clone(),
            id: self.id,
            _peer: peer,
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
    _peer: Option<std::net::SocketAddr>,
    /// The channel opened by the client for this session.
    channel: Option<Channel<Msg>>,
    /// Repository-scoped identity resolved during authentication.
    authenticated_identity: Option<AuthenticatedIdentity>,
    /// Git protocol version requested by the client (default: "1").
    /// Set to "2" when the client sends GIT_PROTOCOL=version=2 via env_request.
    git_protocol_version: String,
}

#[derive(Clone, Debug)]
enum AuthenticatedIdentity {
    User(i64),
    DeployKey { repo_id: i64, read_only: bool },
}

impl AuthenticatedIdentity {
    fn user_id(&self) -> Option<i64> {
        match self {
            Self::User(user_id) => Some(*user_id),
            Self::DeployKey { .. } => None,
        }
    }
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
        let Some(db) = &self.shared.db else {
            // Phase 1 compat: no DB, accept all
            return Ok(Auth::Accept);
        };

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
                match rg_db::ops::user_ops::find_by_id(db, key.user_id).await {
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
                        tracing::error!(error = %format!("{error:#}"), "DB error during key-owner lookup");
                        return Ok(Auth::Reject {
                            proceed_with_methods: None,
                            partial_success: false,
                        });
                    }
                }
                self.authenticated_identity = Some(AuthenticatedIdentity::User(key.user_id));
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
                    self.authenticated_identity = Some(AuthenticatedIdentity::DeployKey {
                        repo_id: key.repo_id,
                        read_only: key.read_only,
                    });
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
        let Some(db) = &self.shared.db else {
            // Phase 1 compat
            return Ok(Auth::Accept);
        };

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

        // `is_usable` is read after the hash, never before: skipping the
        // verification for a disabled account would answer "is this account
        // disabled?" through the response time.
        match found {
            Some(user) if password_ok && user.is_usable() => {
                self.authenticated_identity = Some(AuthenticatedIdentity::User(user.id));
                tracing::info!(username, "SSH password auth accepted");
                Ok(Auth::Accept)
            }
            _ => {
                tracing::warn!(username, "SSH password auth rejected");
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

        if let Some(db) = &self.shared.db {
            if let Err(e) = authorize_git_service(
                db,
                &service,
                &repo_path,
                self.authenticated_identity.as_ref(),
            )
            .await
            {
                tracing::warn!(
                    error = %format!("{e:#}"),
                    identity = ?self.authenticated_identity,
                    %service,
                    %repo_path,
                    "SSH git repository access denied"
                );
                session.channel_failure(channel_id)?;
                return Err(HandlerError(format!(
                    "repository access denied: {}",
                    repo_path
                )));
            }
        }

        let receive_pack_context = if service == "git-receive-pack" {
            if let Some(db) = &self.shared.db {
                let (owner, repo_name) = parse_repo_owner_name(&repo_path)?;
                let repo = rg_core::repo::service::find_repo_by_owner_name(db, &owner, &repo_name)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("repository not found"))?;
                let protection_rules =
                    rg_db::ops::protected_branch_ops::list_by_repo(db, repo.id).await?;
                let tag_protection_rules =
                    rg_db::ops::protected_tag_ops::list_by_repo(db, repo.id).await?;
                Some((
                    protection_rules,
                    tag_protection_rules,
                    self.authenticated_identity
                        .as_ref()
                        .and_then(AuthenticatedIdentity::user_id),
                ))
            } else {
                None
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

        tokio::spawn(async move {
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
            let handler_fut = async {
                if git_protocol_version == "2" {
                    tracing::info!(%service_name, "Using Protocol V2");
                    handle_v2_stream(&repo_full_path, &mut stream).await
                } else {
                    match service_name.as_str() {
                        "git-upload-pack" => {
                            handle_upload_pack_stream(&repo_full_path, &mut stream)
                                .await
                                .map(|_| ())
                        }
                        "git-receive-pack" => {
                            if let Some((protection_rules, tag_protection_rules, actor_id)) =
                                receive_pack_context
                            {
                                let require_signed_refs =
                                    signed_commit_required_refs(&protection_rules);
                                let mut rejected_refs =
                                    branch_protection_rejected_refs(protection_rules, actor_id);
                                rejected_refs.extend(tag_protection_rejected_refs(
                                    tag_protection_rules,
                                    actor_id,
                                ));
                                handle_receive_pack_stream_with_rejections(
                                    &repo_full_path,
                                    &mut stream,
                                    rejected_refs,
                                    require_signed_refs,
                                )
                                .await
                                .map(|_| ())
                            } else {
                                handle_receive_pack_stream(&repo_full_path, &mut stream)
                                    .await
                                    .map(|_| ())
                            }
                        }
                        _ => Err(anyhow::anyhow!("Unknown git service: {}", service_name)),
                    }
                }
            };

            let result: Result<(), anyhow::Error> =
                match with_git_timeout(git_stream_timeout_secs, handler_fut).await {
                    Ok(r) => r,
                    Err(_elapsed) => {
                        tracing::warn!(
                            %service_name,
                            timeout_secs = git_stream_timeout_secs,
                            "git SSH session exceeded wall-clock timeout — killed git, closing channel"
                        );
                        Err(anyhow::anyhow!("git operation timed out"))
                    }
                };

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
                Err(e) => tracing::error!(error = %format!("{e:#}"), %service_name, "Git SSH session failed"),
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
) -> Result<()> {
    let identity = identity.ok_or_else(|| anyhow::anyhow!("authentication required"))?;
    let (owner, repo_name) = parse_repo_owner_name(repo_path)?;
    let repo = rg_core::repo::service::find_repo_by_owner_name(db, &owner, &repo_name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("repository not found"))?;

    let allowed = match identity {
        AuthenticatedIdentity::User(actor_id) => match service {
            "git-upload-pack" => {
                rg_core::repo::service::can_read_repo(db, &repo, Some(*actor_id)).await?
            }
            "git-receive-pack" => {
                rg_core::repo::service::can_write_repo(db, &repo, Some(*actor_id)).await?
            }
            _ => false,
        },
        AuthenticatedIdentity::DeployKey { repo_id, read_only } => {
            deploy_key_allows(*repo_id, *read_only, repo.id, service)
        }
    };

    if !allowed {
        anyhow::bail!("insufficient repository permission");
    }

    Ok(())
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

#[cfg(test)]
mod tests {
    use super::{
        check_host_key_readable, deploy_key_allows, ensure_host_key, parse_git_command,
        parse_repo_owner_name, with_git_timeout,
    };
    use std::time::Duration;

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
        after.split_whitespace().next().and_then(|s| s.chars().next())
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
        assert!(proc_state(pid).is_some(), "child should be alive before timeout");

        // The child is owned *inside* the future, mirroring the real handlers.
        let res = with_git_timeout(1, async move {
            let mut child = child;
            let mut buf = Vec::new();
            // This never completes within the bound — the child sleeps 60s.
            if let Some(mut out) = child.stdout.take() {
                let _ = out.read_to_end(&mut buf).await;
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
        assert!(killed, "child must be killed/zombie after timeout drop, not still running");
    }
}
