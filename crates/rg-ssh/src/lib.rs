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
    /// Database connection (None = open access, Phase 1 compat).
    pub db: Option<DatabaseConnection>,
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
    /// Post-push automation (CI trigger, webhook fan-out, open-PR head-SHA
    /// refresh, auto-merge / merge-queue evaluation) run after an accepted
    /// `git-receive-pack`.
    ///
    /// The identical hooks the Smart-HTTP transport runs — they were private to
    /// `rg-http` until card_b4fefeee8abf, so a push over SSH silently ran none
    /// of them. `None` disables the hooks (no automation configured, or no
    /// database at all); the push itself still succeeds.
    pub post_push: Option<Arc<rg_core::push_hooks::PostPushContext>>,
}

/// Shared state passed to every SshHandler.
struct SharedState {
    repo_root: Arc<PathBuf>,
    db: Option<Arc<DatabaseConnection>>,
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
            instance_settings: ssh_config.instance_settings,
            git_stream_timeout_secs: ssh_config.git_stream_timeout_secs,
            git_idle_timeout_secs: ssh_config.git_idle_timeout_secs,
            post_push: ssh_config.post_push,
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
            rg_core::auth::lockout::PasswordAttempt::Accepted => {
                let account = found
                    .as_ref()
                    .expect("an accepted password attempt resolved to an account");
                // The generation is read from the row this password was checked
                // against, so a reset racing the login can only make the session
                // *older* than the database — and an older generation is refused
                // on the first exec. The other direction, a session that outlives
                // the reset, is the bug this carries the number for.
                self.authenticated_identity = Some(AuthenticatedIdentity::User {
                    user_id: account.id,
                    credential: UserCredential::Password {
                        session_version: account.session_version,
                    },
                });
                tracing::info!(username, "SSH password auth accepted");
                Ok(Auth::Accept)
            }
            rg_core::auth::lockout::PasswordAttempt::SecondFactorRequired => {
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
            rg_core::auth::lockout::PasswordAttempt::Rejected { locked } => {
                tracing::warn!(username, locked, "SSH password auth rejected");
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
        }

        if service == "git-receive-pack" {
            if let Some(db) = &self.shared.db {
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
        }

        let receive_pack_context = if service == "git-receive-pack" {
            if let Some(db) = &self.shared.db {
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
        // The pushing account, for the watch fan-out inside the hooks. `None` on
        // an open-access server that authenticated nobody.
        let hook_pusher_id = receive_pack_context
            .as_ref()
            .and_then(|context| context.actor_id);

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
            //
            // It resolves to `Ok(Some(updates))` only for an accepted push —
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
                        let ref_updates = if let Some(context) = receive_pack_context {
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
                            handle_receive_pack_stream_with_rejections(
                                &repo_full_path,
                                &mut stream,
                                rejected_refs,
                                require_signed_refs,
                            )
                            .await?
                        } else {
                            handle_receive_pack_stream(&repo_full_path, &mut stream).await?
                        };
                        Ok(Some(ref_updates))
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

            let result: Result<(), anyhow::Error> = match with_git_timeout(
                git_stream_timeout_secs,
                handler_fut,
            )
            .await
            {
                Ok(Ok(ref_updates)) => {
                    // ── Post-push hooks: CI, webhooks, PR head-SHA ─────────
                    //
                    // The same hooks the Smart-HTTP transport runs, from the
                    // same `rg-core` entry point. Detached so the client isn't
                    // held while CI is triggered and the webhooks fan out —
                    // but *tracked*: the client is about to get its exit
                    // status, so a bare `tokio::spawn` would be severed by a
                    // SIGTERM in the next few seconds with no pipeline, no
                    // webhook and no trace that any of it was owed.
                    // `delivery_tracker()` is drained by `rg_http::run` after
                    // it stops accepting, the same contract the HTTP push path
                    // relies on.
                    if let (Some(ref_updates), Some(hooks), Some(db), Some((owner, repo_name))) =
                        (ref_updates, post_push, hook_db, hook_target)
                    {
                        let delivery_tracker = hooks.delivery_tracker.clone();
                        delivery_tracker.spawn(async move {
                            hooks
                                .run(
                                    &db,
                                    &hook_repo_path,
                                    &owner,
                                    &repo_name,
                                    hook_pusher_id,
                                    &ref_updates,
                                )
                                .await;
                        });
                    }
                    Ok(())
                }
                Ok(Err(e)) => Err(e),
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
    let allowed = match identity {
        AuthenticatedIdentity::User {
            user_id,
            credential,
        } => {
            let account = match rg_db::ops::user_ops::find_by_id(db, *user_id)
                .await
                .map_err(GitServiceError::ServerUnavailable)?
            {
                Some(user) if user.is_usable() => user,
                _ => return Err(GitServiceError::AccessDenied("account is disabled or gone")),
            };
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
                // The half a password session has instead. Nothing is deleted
                // when the owner resets their password or logs out — the only
                // record of it is that `session_version` moved, and a
                // connection opened before the move is exactly what those two
                // acts are performed to end.
                UserCredential::Password { session_version } => {
                    if account.session_version != *session_version {
                        return Err(GitServiceError::AccessDenied(
                            "this session ended when the account's password was reset or it logged out",
                        ));
                    }
                }
            }
            match service {
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
            }
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

#[cfg(test)]
mod tests {
    use super::{
        check_host_key_readable, deploy_key_allows, ensure_host_key, parse_git_command,
        parse_repo_owner_name, with_git_timeout,
    };
    use std::time::Duration;

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
        let lines: Vec<&str> = source.lines().collect();
        let call = lines
            .iter()
            .position(|line| line.trim_start().starts_with("hooks"))
            .expect("the receive-pack branch must still run the post-push hooks");
        let spawn = lines[..call]
            .iter()
            .rposition(|line| line.contains("spawn("))
            .expect("the post-push call must sit inside a spawn");
        let spawn_line = lines[spawn].trim();
        assert!(
            spawn_line.contains("delivery_tracker.spawn(")
                || spawn_line.contains("delivery_tracker().spawn("),
            "post-push hooks must be spawned through a delivery tracker \
             so the shutdown drain awaits them; found `{spawn_line}` at lib.rs:{}",
            spawn + 1
        );
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
}
