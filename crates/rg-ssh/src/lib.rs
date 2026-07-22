//! IronForge SSH server implementation using russh.
//!
//! Phase 2: auth_publickey queries the database for matching SSH keys.
//! auth_password queries the database and verifies via Argon2.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use rand::rngs::OsRng;
use russh::keys::ssh_key::LineEnding;
use russh::keys::{load_secret_key, Algorithm, PrivateKey};
use russh::server::{Auth, Config, Handler, Msg, Server as _, Session};
use russh::{Channel, ChannelId, ChannelStream};
use sea_orm::DatabaseConnection;
use tokio::io::AsyncWriteExt;

use rg_git::protocol::receive_pack::{
    handle_receive_pack_stream, handle_receive_pack_stream_with_rejections,
};
use rg_git::protocol::upload_pack::handle_upload_pack_stream;
use rg_git::protocol::v2::handle_v2_stream;

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
}

/// Shared state passed to every SshHandler.
struct SharedState {
    repo_root: Arc<PathBuf>,
    db: Option<Arc<DatabaseConnection>>,
}

/// The IronForge SSH server — implements `russh::server::Server`.
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
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create host key directory: {:?}", parent))?;
        }
    }
    let key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519)
        .context("failed to generate ed25519 host key")?;
    let pem = key
        .to_openssh(LineEnding::LF)
        .context("failed to encode generated host key")?;
    std::fs::write(path, pem.as_bytes())
        .with_context(|| format!("failed to write generated host key: {:?}", path))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("failed to set host key permissions: {:?}", path))?;
    }
    tracing::info!(path = ?path, "generated new SSH host key (ed25519)");
    Ok(())
}

impl SshServer {
    /// Create a new SSH server from configuration.
    /// Loads the host key and validates the key file permissions.
    pub fn new(ssh_config: SshServerConfig) -> Result<Self> {
        ensure_host_key(&ssh_config.host_key_path)?;
        let host_key = load_secret_key(&ssh_config.host_key_path, None)
            .with_context(|| format!("failed to load host key: {:?}", ssh_config.host_key_path))?;

        let config = Config {
            auth_rejection_time: std::time::Duration::from_secs(1),
            auth_rejection_time_initial: Some(std::time::Duration::from_secs(0)),
            keys: vec![host_key],
            ..Default::default()
        };

        let shared = Arc::new(SharedState {
            repo_root: Arc::new(ssh_config.repo_root),
            db: ssh_config.db.map(Arc::new),
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

/// russh Handler implementation for IronForge.
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

    // CRITICAL: Auth::Reject must include `partial_success: false` (踩坑经验 #5)
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
                self.authenticated_identity = Some(AuthenticatedIdentity::User(key.user_id));
                if let Err(error) = rg_db::ops::ssh_key_ops::touch_last_used(db, key.id).await {
                    tracing::warn!(key_id = key.id, error = %error, "failed to update SSH key usage time");
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
                        tracing::warn!(key_id = key.id, error = %error, "failed to update deploy key usage time");
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
                    tracing::error!(%error, "DB error during deploy-key lookup");
                    Ok(Auth::Reject {
                        proceed_with_methods: None,
                        partial_success: false,
                    })
                }
            },
            Err(e) => {
                tracing::error!(error = %e, "DB error during pubkey lookup");
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

        match rg_db::ops::user_ops::find_by_username(db, username).await {
            Ok(Some(user)) => {
                match rg_core::auth::password::verify_password(password, &user.password_hash) {
                    Ok(true) => {
                        self.authenticated_identity = Some(AuthenticatedIdentity::User(user.id));
                        tracing::info!(username, "SSH password auth accepted");
                        Ok(Auth::Accept)
                    }
                    Ok(false) => {
                        tracing::warn!(username, "SSH password auth rejected");
                        Ok(Auth::Reject {
                            proceed_with_methods: None,
                            partial_success: false,
                        })
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "password verify error");
                        Ok(Auth::Reject {
                            proceed_with_methods: None,
                            partial_success: false,
                        })
                    }
                }
            }
            Ok(None) => {
                tracing::warn!(username, "SSH password auth: user not found");
                Ok(Auth::Reject {
                    proceed_with_methods: None,
                    partial_success: false,
                })
            }
            Err(e) => {
                tracing::error!(error = %e, "DB error during password auth");
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
                    error = %e,
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

        tokio::spawn(async move {
            tracing::info!(%service_name, path = %repo_full_path.display(), "Starting git SSH session");

            let mut stream: ChannelStream<Msg> = ch.into_stream();

            let result: Result<(), anyhow::Error> = if git_protocol_version == "2" {
                tracing::info!(%service_name, "Using Protocol V2");
                handle_v2_stream(&repo_full_path, &mut stream).await
            } else {
                match service_name.as_str() {
                    "git-upload-pack" => handle_upload_pack_stream(&repo_full_path, &mut stream)
                        .await
                        .map(|_| ()),
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
            };

            let exit_code: u32 = if result.is_ok() { 0 } else { 1 };

            match &result {
                Ok(_) => tracing::info!(%service_name, "Git SSH session complete"),
                Err(e) => tracing::error!(error = %e, %service_name, "Git SSH session failed"),
            }

            // CRITICAL: SSH stream shutdown order (踩坑经验)
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

fn branch_protection_rejected_refs(
    protections: Vec<rg_db::entities::protected_branch::Model>,
    actor_id: Option<i64>,
) -> Vec<(String, String)> {
    protections
        .into_iter()
        .filter_map(|protection| {
            if direct_push_allowed_by_rule(&protection, actor_id) {
                return None;
            }

            let message = if protection.require_pr {
                format!(
                    "push to protected branch '{}' is not allowed; open a pull request instead",
                    protection.branch_name
                )
            } else if !protection.allow_force_push {
                format!(
                    "force push to protected branch '{}' is not allowed",
                    protection.branch_name
                )
            } else {
                return None;
            };

            Some((format!("refs/heads/{}", protection.branch_name), message))
        })
        .collect()
}

fn direct_push_allowed_by_rule(
    protection: &rg_db::entities::protected_branch::Model,
    actor_id: Option<i64>,
) -> bool {
    if let Some(uid) = actor_id {
        if let Some(allowed_json) = &protection.allowed_push_user_ids {
            if let Ok(allowed_ids) = serde_json::from_str::<Vec<i64>>(allowed_json) {
                if allowed_ids.contains(&uid) {
                    return true;
                }
            }
        }
    }

    false
}

fn signed_commit_required_refs(
    protections: &[rg_db::entities::protected_branch::Model],
) -> Vec<String> {
    protections
        .iter()
        .filter(|rule| rule.require_signed_commits)
        .map(|rule| format!("refs/heads/{}", rule.branch_name))
        .collect()
}

fn tag_protection_rejected_refs(
    protections: Vec<rg_db::entities::protected_tag::Model>,
    actor_id: Option<i64>,
) -> Vec<(String, String)> {
    protections
        .into_iter()
        .filter_map(|protection| {
            let allowed = actor_id.is_some_and(|uid| {
                protection
                    .allowed_user_ids
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<Vec<i64>>(json).ok())
                    .is_some_and(|ids| ids.contains(&uid))
            });
            (!allowed).then(|| {
                (
                    format!("refs/tags/{}", protection.pattern),
                    format!(
                        "creation or update of protected tag pattern '{}' is not allowed",
                        protection.pattern
                    ),
                )
            })
        })
        .collect()
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
    use super::{deploy_key_allows, parse_git_command, parse_repo_owner_name};

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
}
