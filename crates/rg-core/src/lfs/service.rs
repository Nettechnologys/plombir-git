//! Git LFS service — implements the LFS batch API.
//!
//! Git LFS (Large File Storage) replaces large files with pointer files in Git,
//! while storing the actual content separately. This service implements the
//! LFS batch API for upload/download operations.
//!
//! Storage layout: `<repo_root>/<owner>/<repo>.lfs/<oid_prefix>/<oid>`
//!
//! ## Compression
//!
//! Every object this service stores is zstd-compressed: `store_object_from_file`
//! compresses before publishing and keys the blob as
//! `<oid>.zst`. Storage format:
//! - Compressed: `<oid>.zst` (zstd compressed)
//! - Uncompressed (legacy): `<oid>` (raw)
//!
//! The `compression` field in DB tracks the algorithm used.
//!
//! Legacy uncompressed objects are **read** through the fallback at the end of
//! `read_object_source`, and are not re-compressed in place. There used to be a
//! `compress_existing` backfill here; nothing ever called it (card_f569b0674f50)
//! and it could not have been called safely as written — it walked the local
//! filesystem with `std::fs` rather than the `BlobStorage` backend the objects
//! now live in, and wrote the `.zst` outside the publication lease that makes a
//! concurrent write safe (card_2262e869a068). Leaving those objects uncompressed
//! costs disk, not correctness. A backfill that is wanted back has to go through
//! `publish_object` like every other writer, and that is a feature, not a
//! resurrection of the deleted function.
//!
//! ## Deletion
//!
//! There is deliberately no per-object delete here. Objects are reclaimed by the
//! owner that holds them: deleting a repository retires the whole
//! `lfs/<namespace>/<repo>` blob prefix (`repo::service::repository_blob_prefixes`),
//! and a publication that fails after writing bytes compensates itself through
//! `discard_stored_blob`. `delete_object` and `delete_object_from_storage` used
//! to sit here with zero callers — no route, no CLI command, no job
//! (card_9dc9cac96edc) — which read as "Plombir Git can delete an LFS object" when
//! nothing ever did.
//!
//! What is genuinely missing is a *garbage collector*: an object whose last
//! referencing commit is gone stays on disk forever. That needs a reachability
//! walk over the repository's history, which is a feature to design, not a
//! function to re-add — and it must go through `BlobStorage`, not the legacy
//! filesystem path the deleted pair reached for.

use anyhow::{Context, Result};
use chrono::Utc;
use hmac::{Hmac, Mac};
use sea_orm::{ActiveModelTrait, DatabaseConnection};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
#[cfg(test)]
use std::io::Write;
use std::path::PathBuf;

use crate::blob_storage::{BlobKey, BlobStorage};
use crate::platform::fs::discard_file;
use rg_db::entities::lfs_object;
use rg_db::ops::lfs_object_ops;

/// Compression level for zstd (1-22, default 3)
const ZSTD_LEVEL: i32 = 3;

/// Hard ceiling for one LFS object (10 GiB).
///
/// This is shared by the batch contract and the HTTP streaming backstop. A
/// client must not be able to obtain an upload action for bytes that the upload
/// route will later refuse, and a future non-HTTP caller must not silently
/// remove the boundary by bypassing Axum's route layer.
pub const LFS_OBJECT_MAX_BYTES: usize = 10 * 1024 * 1024 * 1024;

/// Signed download URLs are deliberately short-lived to limit leakage.
pub const DOWNLOAD_URL_TTL_SECONDS: i64 = 60 * 60;
/// Upload URLs allow enough time for large objects on slow connections.
pub const UPLOAD_URL_TTL_SECONDS: i64 = 6 * 60 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LfsActionKind {
    Download,
    Upload,
}

impl LfsActionKind {
    /// The batch API's `operation` / `git-lfs-authenticate`'s last argument.
    pub fn from_operation(operation: &str) -> Option<Self> {
        match operation {
            "download" => Some(Self::Download),
            "upload" => Some(Self::Upload),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::Upload => "upload",
        }
    }

    fn ttl_seconds(self) -> i64 {
        match self {
            Self::Download => DOWNLOAD_URL_TTL_SECONDS,
            Self::Upload => UPLOAD_URL_TTL_SECONDS,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LfsActionSignatureError {
    #[error("LFS action URL has expired")]
    Expired,
    #[error("invalid LFS action URL signature")]
    Invalid,
}

type HmacSha256 = Hmac<Sha256>;

/// The credential a signed URL was issued against — and therefore the thing
/// that has to still stand when the URL is redeemed.
///
/// Which question to ask depends on what was presented, because the two
/// credentials are revoked by different acts and neither act touches the other:
///
/// * a **session** is revoked by a password reset or a `POST /users/logout`,
///   both of which leave `is_usable()` true and bump `users.session_version`;
/// * a **personal access token** is revoked by deleting its row (or by its
///   `expires_at` passing), and deliberately survives a password change — which
///   is the instance's recorded policy for PATs.
///
/// Folding the session generation into *every* capability got both halves
/// wrong for a PAT-issued one: `pat_to_bearer_jwt` mints a synthetic JWT
/// carrying the owner's current generation, so a CI upload URL obtained with a
/// PAT died the moment a human logged out of a laptop — an event with no
/// bearing on the token — while deleting the PAT itself, the one act that means
/// "this credential is revoked", left the URL good for the rest of its six
/// hours (card_e4e177acd095).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LfsCredential {
    /// A browser or API session, revoked by its generation moving on.
    Session {
        /// The `users.session_version` the issuing session was authenticated under.
        version: i64,
    },
    /// A personal access token, revoked by the row going away.
    Token {
        /// `access_tokens.id` of the token the request presented.
        id: i64,
    },
    /// The SSH key an account authenticated with on the SSH port, revoked by
    /// deleting the key. Like a PAT it is a standing credential and survives a
    /// password change; it is what `git-lfs-authenticate` speaks for when the
    /// clone came from the SSH address.
    SshKey {
        /// `ssh_keys.id` of the key the SSH session authenticated with.
        id: i64,
    },
}

/// Who a signed URL is issued to, and the credential it was issued against.
///
/// An account travels together with its credential because an id on its own can
/// only ask half of the revocation question at redemption time. Re-reading the
/// account catches a deactivation, but a password reset and a
/// `POST /users/logout` leave `is_usable()` true — so a capability carrying only
/// the id had nothing to compare against, and an upload URL minted by a stolen
/// session stayed write access to a private repository for the rest of its six
/// hours (card_c742da1794e4). Same shape, same fix as the WebSocket half of this
/// class (`rg_http::api::auth::WsSessionUser`).
///
/// A deploy key is the one caller that is not an account: it opens exactly one
/// repository, read-only or not, and that is what has to still hold when its URL
/// is redeemed. It reaches LFS only through `git-lfs-authenticate` on the SSH
/// port, the one door a deploy key has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LfsActor {
    User {
        user_id: i64,
        credential: LfsCredential,
    },
    DeployKey {
        /// `deploy_keys.id`.
        key_id: i64,
    },
}

impl LfsActor {
    /// The account behind this actor; `None` for a deploy key, which speaks for
    /// a repository rather than for a person.
    pub fn user_id(self) -> Option<i64> {
        match self {
            Self::User { user_id, .. } => Some(user_id),
            Self::DeployKey { .. } => None,
        }
    }
}

/// How the actor a signed URL was issued to is rendered into the signed
/// payload — and, for an authenticated one, into the URL itself.
///
/// An anonymous issue is a distinct value rather than an empty one, so
/// dropping `actor=` from an authenticated URL changes the payload instead of
/// reproducing it. The `s`/`t`/`k` tag is inside the signature for the same
/// reason: the credentials are checked differently at redemption, so which one
/// this is must not be something a caller can swap. A deploy key renders with a
/// non-numeric prefix, so it can never read as an account id.
fn actor_token(actor: Option<LfsActor>) -> String {
    match actor {
        Some(LfsActor::User {
            user_id,
            credential: LfsCredential::Session { version },
        }) => format!("{user_id}@s{version}"),
        Some(LfsActor::User {
            user_id,
            credential: LfsCredential::Token { id },
        }) => format!("{user_id}@t{id}"),
        Some(LfsActor::User {
            user_id,
            credential: LfsCredential::SshKey { id },
        }) => format!("{user_id}@k{id}"),
        Some(LfsActor::DeployKey { key_id }) => format!("deploy@{key_id}"),
        None => "anon".to_string(),
    }
}

/// The inverse of [`actor_token`] for an authenticated actor. `anon` and
/// anything this server never renders are `None`.
fn parse_actor_token(token: &str) -> Option<LfsActor> {
    if let Some(key_id) = token.strip_prefix("deploy@") {
        return Some(LfsActor::DeployKey {
            key_id: parse_canonical_id(key_id)?,
        });
    }
    let (user_id, credential) = token.split_once('@')?;
    let user_id = parse_canonical_id(user_id)?;
    let credential = match credential.split_at_checked(1)? {
        ("s", version) => LfsCredential::Session {
            version: parse_canonical_id(version)?,
        },
        ("t", id) => LfsCredential::Token {
            id: parse_canonical_id(id)?,
        },
        ("k", id) => LfsCredential::SshKey {
            id: parse_canonical_id(id)?,
        },
        _ => return None,
    };
    Some(LfsActor::User {
        user_id,
        credential,
    })
}

/// A decimal id exactly as `format!("{}")` renders an `i64` — no sign, no
/// padding — so a parsed token re-renders to the bytes that were signed.
fn parse_canonical_id(text: &str) -> Option<i64> {
    let value = text.parse::<i64>().ok()?;
    (value.to_string() == text).then_some(value)
}

/// The signed payload.
///
/// `v3` folded the issuing session into the signature; `v4` replaced that with
/// the issuing *credential*, so a PAT-issued URL is checked against the PAT
/// rather than against a session generation that has nothing to do with it.
/// URLs minted under an older generation stop verifying, which for a revocation
/// change is the desired direction — a client that meets one re-requests a batch
/// and is handed a fresh URL.
fn action_signature_payload(
    action: LfsActionKind,
    repo_id: i64,
    oid: &str,
    expires_at: i64,
    actor: Option<LfsActor>,
) -> String {
    format!(
        "plombir-git-lfs-v4:{}:{}:{}:{}:{}",
        action.as_str(),
        repo_id,
        oid,
        expires_at,
        actor_token(actor)
    )
}

/// Append the actor a signed URL was issued to, when there was one.
///
/// Both halves are echoed, because both are covered by the HMAC and the
/// redeeming side needs them to recompute it — and needs the credential on its
/// own to answer "is the thing that asked for this still allowed to act?". The
/// second parameter is named for the credential (`session=`, `pat=` or
/// `ssh_key=`), so a URL says which question it expects to be asked. A deploy
/// key has no account, so it is echoed alone as `deploy_key=`.
///
/// Anonymous issues carry nothing: there is no account behind them to re-check,
/// and `anon` is already what the signature covers.
pub fn action_url_actor_param(actor: Option<LfsActor>) -> String {
    match actor {
        Some(LfsActor::User {
            user_id,
            credential: LfsCredential::Session { version },
        }) => format!("&actor={user_id}&session={version}"),
        Some(LfsActor::User {
            user_id,
            credential: LfsCredential::Token { id },
        }) => format!("&actor={user_id}&pat={id}"),
        Some(LfsActor::User {
            user_id,
            credential: LfsCredential::SshKey { id },
        }) => format!("&actor={user_id}&ssh_key={id}"),
        Some(LfsActor::DeployKey { key_id }) => format!("&deploy_key={key_id}"),
        None => String::new(),
    }
}

/// Sign an LFS action URL. The signature is bound to action, repository,
/// object, expiry and the actor it was issued to, so a URL cannot be reused
/// for another purpose — and the account behind it can still be re-checked
/// when the URL is redeemed.
pub fn sign_action_url(
    secret: &[u8],
    action: LfsActionKind,
    repo_id: i64,
    oid: &str,
    expires_at: i64,
    actor: Option<LfsActor>,
) -> Result<String> {
    let mut mac = HmacSha256::new_from_slice(secret)?;
    mac.update(action_signature_payload(action, repo_id, oid, expires_at, actor).as_bytes());
    Ok(hex::encode(mac.finalize().into_bytes()))
}

/// Verify a signed LFS action URL at a caller-provided timestamp.
/// Supplying `now` keeps expiry behavior deterministic in tests.
///
/// Verifying the signature answers "this URL was issued by us, for this action,
/// on this object, to this actor, under this session" — and nothing beyond that.
/// Whether the actor may *still* act, and whether that session is still the
/// current one, is the caller's question; see
/// `rg_http::api::lfs::authorize_signed_action`.
#[allow(clippy::too_many_arguments)]
pub fn verify_action_url(
    secret: &[u8],
    action: LfsActionKind,
    repo_id: i64,
    oid: &str,
    expires_at: i64,
    actor: Option<LfsActor>,
    signature: &str,
    now: i64,
) -> std::result::Result<(), LfsActionSignatureError> {
    if expires_at <= now {
        return Err(LfsActionSignatureError::Expired);
    }
    let signature = hex::decode(signature).map_err(|_| LfsActionSignatureError::Invalid)?;
    let mut mac =
        HmacSha256::new_from_slice(secret).map_err(|_| LfsActionSignatureError::Invalid)?;
    mac.update(action_signature_payload(action, repo_id, oid, expires_at, actor).as_bytes());
    mac.verify_slice(&signature)
        .map_err(|_| LfsActionSignatureError::Invalid)
}

// ── LFS over an SSH remote ────────────────────────────────────────────────

/// The `Authorization` scheme of the credential `git-lfs-authenticate` hands
/// out.
///
/// A scheme of its own rather than `Bearer`, so every other reader of the
/// header — the PAT bridge, the session-standing gate, the JWT extractors —
/// passes it by without trying it as one of theirs, and the only code that
/// honours it is the LFS batch handler it is minted for.
pub const SSH_GRANT_AUTH_SCHEME: &str = "SSH-LFS";

/// How long the credential from `git-lfs-authenticate` lasts. git-lfs caches it
/// by the `expires_in` it is told and runs the command again when it runs out,
/// so this bounds a leaked header, not a long transfer.
pub const SSH_GRANT_TTL_SECONDS: i64 = 10 * 60;

/// What the SSH port vouched for: one LFS operation, on one repository, for one
/// SSH-authenticated actor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SshLfsGrant {
    pub action: LfsActionKind,
    pub repo_id: i64,
    pub actor: LfsActor,
}

fn ssh_grant_payload(action: &str, repo_id: &str, expires_at: &str, actor: &str) -> String {
    format!("plombir-git-lfs-ssh-grant-v1:{action}:{repo_id}:{expires_at}:{actor}")
}

/// Mint the credential `git-lfs-authenticate` returns in its `header`.
///
/// The SSH port has already asked its own gate — the one `git-upload-pack` /
/// `git-receive-pack` ask — before it calls this. The grant carries the answer
/// to the HTTP side, bound to the operation, the repository and the actor, and
/// signed under a payload prefix no action URL uses, so neither can be replayed
/// as the other.
pub fn sign_ssh_grant(
    secret: &[u8],
    action: LfsActionKind,
    repo_id: i64,
    expires_at: i64,
    actor: LfsActor,
) -> Result<String> {
    let (action, repo_id, expires_at, actor) = (
        action.as_str(),
        repo_id.to_string(),
        expires_at.to_string(),
        actor_token(Some(actor)),
    );
    let mut mac = HmacSha256::new_from_slice(secret)?;
    mac.update(ssh_grant_payload(action, &repo_id, &expires_at, &actor).as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());
    Ok(format!(
        "{action}.{repo_id}.{expires_at}.{actor}.{signature}"
    ))
}

/// Check a credential minted by [`sign_ssh_grant`] at a caller-provided time.
///
/// Like [`verify_action_url`] this proves only that the server issued the grant
/// and that it has not run out. Whether the actor still stands and still has
/// access is the redeeming side's question, asked again on every batch.
pub fn verify_ssh_grant(
    secret: &[u8],
    token: &str,
    now: i64,
) -> std::result::Result<SshLfsGrant, LfsActionSignatureError> {
    let invalid = LfsActionSignatureError::Invalid;
    let mut parts = token.split('.');
    let (Some(action), Some(repo_id), Some(expires_at), Some(actor), Some(signature), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(invalid);
    };
    let signature = hex::decode(signature).map_err(|_| invalid)?;
    let mut mac = HmacSha256::new_from_slice(secret).map_err(|_| invalid)?;
    mac.update(ssh_grant_payload(action, repo_id, expires_at, actor).as_bytes());
    mac.verify_slice(&signature).map_err(|_| invalid)?;

    // Signed, so every field below is one this server wrote; a field that does
    // not parse is still refused rather than trusted.
    let action = LfsActionKind::from_operation(action).ok_or(invalid)?;
    let repo_id = parse_canonical_id(repo_id).ok_or(invalid)?;
    let expires_at = parse_canonical_id(expires_at).ok_or(invalid)?;
    let actor = parse_actor_token(actor).ok_or(invalid)?;
    if expires_at <= now {
        return Err(LfsActionSignatureError::Expired);
    }
    Ok(SshLfsGrant {
        action,
        repo_id,
        actor,
    })
}

/// Git LFS SHA-256 object identifiers are exactly 64 lowercase hex bytes.
pub fn is_valid_oid(oid: &str) -> bool {
    oid.len() == 64
        && oid
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

// ── LFS API types ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct LfsBatchRequest {
    pub operation: String, // "upload" or "download"
    pub objects: Vec<LfsObjectRequest>,
    pub transfers: Option<Vec<String>>, // e.g. ["basic"]
    pub refname: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LfsObjectRequest {
    pub oid: String,
    pub size: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LfsBatchResponse {
    pub transfer: String,
    pub objects: Vec<LfsObjectResponse>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LfsObjectResponse {
    pub oid: String,
    pub size: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actions: Option<LfsActions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<LfsError>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LfsActions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download: Option<LfsAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload: Option<LfsAction>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LfsAction {
    pub href: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<std::collections::HashMap<String, String>>,
    #[serde(rename = "expires_in")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<i64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LfsError {
    pub code: i32,
    pub message: String,
}

// ── LFS service ───────────────────────────────────────────────────────────

/// Get the storage path for an LFS object.
fn lfs_object_path(lfs_root: &std::path::Path, oid: &str) -> Result<PathBuf> {
    if !is_valid_oid(oid) {
        anyhow::bail!("invalid LFS object identifier");
    }
    // Shard by first 2 hex chars: <lfs_root>/ab/<full-oid>
    let prefix = &oid[..2];
    Ok(lfs_root.join(prefix).join(oid))
}

/// Stable storage key for an LFS object. New objects use backend-neutral keys;
/// the historical `<owner>.lfs/<repo>` layout remains a read/delete fallback.
pub fn lfs_object_key(owner: &str, repo: &str, oid: &str, compressed: bool) -> Result<BlobKey> {
    if !is_valid_oid(oid) {
        anyhow::bail!("invalid LFS object identifier");
    }
    let filename = if compressed {
        format!("{oid}.zst")
    } else {
        oid.to_string()
    };
    BlobKey::from_segments(["lfs", owner, repo, &oid[..2], &filename]).map_err(Into::into)
}

/// Get the LFS root directory for a repository.
pub fn lfs_root(repo_root: &std::path::Path, owner: &str, repo: &str) -> PathBuf {
    repo_root.join(format!("{}.lfs", owner)).join(repo)
}

fn lfs_request_error(operation: &str, oid: &str, size: i64) -> Option<LfsError> {
    let upload_too_large = operation == "upload" && size > LFS_OBJECT_MAX_BYTES as i64;
    if is_valid_oid(oid) && size >= 0 && !upload_too_large {
        return None;
    }

    Some(LfsError {
        code: if upload_too_large { 413 } else { 422 },
        message: if upload_too_large {
            format!("LFS object exceeds the {LFS_OBJECT_MAX_BYTES}-byte upload limit")
        } else {
            "invalid LFS object identifier or size".to_string()
        },
    })
}

/// The only transfer adapter this server implements: plain HTTP `PUT`/`GET`
/// against the action `href`.
const BASIC_TRANSFER: &str = "basic";

/// Pick the transfer adapter the batch response names.
///
/// The client lists the adapters *it* can run, most preferred first, and the
/// server must answer with one of them that it can serve too — the response's
/// `transfer` is what the client hands every action to. This used to echo the
/// client's first choice. git-lfs 3.x lists `lfs-standalone-file` first, an
/// adapter for local file remotes that an HTTP server never serves, so the
/// response named it and `git lfs push` died on a nil adapter before
/// uploading a byte. An absent list means `basic` (the batch API spec); a list
/// without it has nothing this server can serve, and saying so is better than
/// naming an adapter the client then cannot drive.
fn negotiate_transfer(offered: Option<&[String]>) -> Result<&'static str> {
    match offered {
        None => Ok(BASIC_TRANSFER),
        Some(offered) if offered.iter().any(|name| name == BASIC_TRANSFER) => Ok(BASIC_TRANSFER),
        Some(offered) => Err(crate::error::invalid_request(format!(
            "no supported LFS transfer adapter offered (server supports `{BASIC_TRANSFER}`, \
             client offered {offered:?})"
        ))),
    }
}

/// Handle a batch upload/download request.
/// Processes all objects concurrently using `join_all` to avoid serial DB round-trips.
#[allow(clippy::too_many_arguments)]
pub async fn batch(
    db: &DatabaseConnection,
    repo_id: i64,
    storage: &dyn BlobStorage,
    lfs_root: &std::path::Path,
    base_url: &str,
    owner: &str,
    repo: &str,
    req: &LfsBatchRequest,
    signing_secret: &[u8],
    actor: Option<LfsActor>,
) -> Result<LfsBatchResponse> {
    let transfer = negotiate_transfer(req.transfers.as_deref())?.to_string();

    let operation = req.operation.as_str();
    let futures: Vec<_> = req
        .objects
        .iter()
        .map(|obj_req| {
            let oid = &obj_req.oid;
            let size = obj_req.size;
            async move {
                if let Some(error) = lfs_request_error(operation, oid, size) {
                    return Ok(LfsObjectResponse {
                        oid: oid.to_string(),
                        size,
                        actions: None,
                        error: Some(error),
                    });
                }
                match operation {
                    "upload" => {
                        handle_upload(
                            db,
                            repo_id,
                            storage,
                            lfs_root,
                            base_url,
                            owner,
                            repo,
                            oid,
                            size,
                            signing_secret,
                            actor,
                        )
                        .await
                    }
                    "download" => {
                        handle_download(
                            db,
                            repo_id,
                            lfs_root,
                            base_url,
                            owner,
                            repo,
                            oid,
                            size,
                            signing_secret,
                            actor,
                        )
                        .await
                    }
                    _ => Ok(LfsObjectResponse {
                        oid: oid.to_string(),
                        size,
                        actions: None,
                        error: Some(LfsError {
                            code: 422,
                            message: format!("unsupported operation: {}", operation),
                        }),
                    }),
                }
            }
        })
        .collect();

    let objects = futures::future::join_all(futures)
        .await
        .into_iter()
        .collect::<Result<Vec<_>>>()?;

    Ok(LfsBatchResponse { transfer, objects })
}

#[allow(clippy::too_many_arguments)]
async fn handle_upload(
    db: &DatabaseConnection,
    repo_id: i64,
    storage: &dyn BlobStorage,
    lfs_root: &std::path::Path,
    base_url: &str,
    owner: &str,
    repo: &str,
    oid: &str,
    size: i64,
    signing_secret: &[u8],
    actor: Option<LfsActor>,
) -> Result<LfsObjectResponse> {
    // Check if object already exists
    let existing = lfs_object_ops::find_by_repo_and_oid(db, repo_id, oid).await?;

    if let Some(obj) = &existing {
        if obj.uploaded {
            let compressed_key = lfs_object_key(owner, repo, oid, true)?;
            let raw_key = lfs_object_key(owner, repo, oid, false)?;
            let obj_path = lfs_object_path(lfs_root, oid)?;
            if storage.exists(&compressed_key).await?
                || storage.exists(&raw_key).await?
                || obj_path.exists()
                || obj_path.with_extension("zst").exists()
            {
                // Already uploaded — no action needed
                return Ok(LfsObjectResponse {
                    oid: oid.to_string(),
                    size,
                    actions: None,
                    error: None,
                });
            }
        }
    }

    // Register object if not yet tracked. Two clients batching the same new
    // object at once both see `existing == None`; the unique index lets exactly
    // one insert land, and the loser wants that row rather than a failed batch.
    if existing.is_none() {
        find_or_register_object(db, repo_id, oid, size).await?;
    }

    // Return upload URL
    let expires_at = Utc::now().timestamp() + LfsActionKind::Upload.ttl_seconds();
    let signature = sign_action_url(
        signing_secret,
        LfsActionKind::Upload,
        repo_id,
        oid,
        expires_at,
        actor,
    )?;
    let upload_href = format!(
        "{}/api/v1/repos/{}/{}/lfs/objects/{}?expires={}&signature={}{}",
        base_url,
        owner,
        repo,
        oid,
        expires_at,
        signature,
        action_url_actor_param(actor)
    );

    Ok(LfsObjectResponse {
        oid: oid.to_string(),
        size,
        actions: Some(LfsActions {
            download: None,
            upload: Some(LfsAction {
                href: upload_href,
                header: None,
                expires_in: Some(UPLOAD_URL_TTL_SECONDS),
            }),
        }),
        error: None,
    })
}

#[allow(clippy::too_many_arguments)]
async fn handle_download(
    db: &DatabaseConnection,
    repo_id: i64,
    _lfs_root: &std::path::Path,
    base_url: &str,
    owner: &str,
    repo: &str,
    oid: &str,
    size: i64,
    signing_secret: &[u8],
    actor: Option<LfsActor>,
) -> Result<LfsObjectResponse> {
    let Some(existing) = lfs_object_ops::find_by_repo_and_oid(db, repo_id, oid).await? else {
        return Ok(LfsObjectResponse {
            oid: oid.to_string(),
            size,
            actions: None,
            error: Some(LfsError {
                code: 404,
                message: "object not found".to_string(),
            }),
        });
    };

    if !existing.uploaded {
        return Ok(LfsObjectResponse {
            oid: oid.to_string(),
            size,
            actions: None,
            error: Some(LfsError {
                code: 404,
                message: "object not uploaded yet".to_string(),
            }),
        });
    }

    let expires_at = Utc::now().timestamp() + LfsActionKind::Download.ttl_seconds();
    let signature = sign_action_url(
        signing_secret,
        LfsActionKind::Download,
        repo_id,
        oid,
        expires_at,
        actor,
    )?;
    let download_href = format!(
        "{}/api/v1/repos/{}/{}/lfs/objects/{}?expires={}&signature={}{}",
        base_url,
        owner,
        repo,
        oid,
        expires_at,
        signature,
        action_url_actor_param(actor)
    );

    Ok(LfsObjectResponse {
        oid: oid.to_string(),
        size,
        actions: Some(LfsActions {
            download: Some(LfsAction {
                href: download_href,
                header: None,
                expires_in: Some(DOWNLOAD_URL_TTL_SECONDS),
            }),
            upload: None,
        }),
        error: None,
    })
}

/// Buffered publication path used to exercise the same lease/rollback logic as
/// the production file-backed upload without staging another temporary file.
#[cfg(test)]
async fn store_object(
    db: &DatabaseConnection,
    repo_id: i64,
    storage: &dyn BlobStorage,
    owner: &str,
    repo: &str,
    oid: &str,
    data: &[u8],
) -> Result<()> {
    // Find or create the DB record first
    let object = find_or_register_object(db, repo_id, oid, data.len() as i64).await?;

    // Compress data with zstd
    let compressed = compress_data(data)?;
    let compressed_size = compressed.len() as i64;

    let key = lfs_object_key(owner, repo, oid, true)?;

    publish_object(
        db,
        storage,
        PublicationRequest {
            object_id: object.id,
            repo_id,
            oid,
            key: &key,
            source: PublicationSource::Buffered(&compressed),
        },
    )
    .await?;

    // After the publication, not before it: the line says the object is stored,
    // and a request that waits for a lease or rolls its blob back never got
    // there.
    tracing::info!(
        oid = %oid,
        original_size = data.len(),
        compressed_size = compressed_size,
        ratio = format!("{:.1}%", (compressed_size as f64 / data.len() as f64) * 100.0),
        "LFS object compressed and stored"
    );
    Ok(())
}

/// Fetch the object's row, registering it if this is the first anyone hears of
/// it.
///
/// The unique index on `(repo_id, oid)` makes exactly one of two racing first
/// uploads lose the insert. Losing the insert is not losing the upload: the row
/// the winner created is the row this request wanted, so re-read it instead of
/// failing an upload that has nothing wrong with it. Matching on the driver's
/// constraint-violation error would tie this to one backend, so the retry is
/// the plain "look again" that every backend agrees on.
async fn find_or_register_object(
    db: &DatabaseConnection,
    repo_id: i64,
    oid: &str,
    size: i64,
) -> Result<lfs_object::Model> {
    if let Some(obj) = lfs_object_ops::find_by_repo_and_oid(db, repo_id, oid).await? {
        return Ok(obj);
    }

    let model = lfs_object::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: sea_orm::Set(repo_id),
        oid: sea_orm::Set(oid.to_string()),
        size: sea_orm::Set(size),
        uploaded: sea_orm::Set(false),
        created_at: sea_orm::Set(Utc::now()),
        publisher_token: sea_orm::Set(None),
        publisher_since: sea_orm::Set(None),
    };
    match lfs_object_ops::create(db, model).await {
        Ok(obj) => Ok(obj),
        Err(error) => match lfs_object_ops::find_by_repo_and_oid(db, repo_id, oid).await? {
            Some(obj) => Ok(obj),
            None => Err(error),
        },
    }
}

/// Whether this call put new bytes under the content-addressed key.
///
/// The distinction cannot be reconstructed after `put`: a retry and the first
/// publication use the same key, but only the latter owns bytes a failed DB
/// update may roll back.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BlobPublication {
    Published,
    Reused,
}

/// Where the bytes of a publication come from.
enum PublicationSource<'a> {
    Buffered(&'a [u8]),
    File(&'a std::path::Path),
    /// Another repository's stored object, published by sharing its bytes
    /// ([`BlobStorage::put_file_shared`]) rather than copying them.
    Shared(&'a std::path::Path),
}

/// Everything one publication of one LFS object needs to know about itself.
struct PublicationRequest<'a> {
    object_id: i64,
    repo_id: i64,
    oid: &'a str,
    key: &'a BlobKey,
    source: PublicationSource<'a>,
}

/// How long a publication lease stays valid before another request may take it
/// over.
///
/// The ceiling has to clear the longest legitimate hold — one `put` of an
/// already-compressed object plus one row update — because taking over the
/// lease of a holder that is merely slow, rather than dead, is precisely the
/// case where two requests both believe they own the same bytes.
const PUBLICATION_LEASE_TTL_SECONDS: i64 = 15 * 60;

/// How long a request waits for a competing publication of the same object.
///
/// Waiting is the cheap outcome: the holder is writing the very bytes this
/// request wants under the very key it would use, so the waiter usually
/// inherits a finished object and does nothing.
const PUBLICATION_LEASE_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

const PUBLICATION_LEASE_POLL_MIN: std::time::Duration = std::time::Duration::from_millis(25);
const PUBLICATION_LEASE_POLL_MAX: std::time::Duration = std::time::Duration::from_millis(500);

/// Another request is publishing this object's blob and did not let go in time.
///
/// A distinct type rather than a bare message because the distinction matters
/// to the caller: nothing is wrong with the request and nothing is wrong with
/// the server, so answering `500` would tell a client to stop when what it
/// should do is come back. See `rg_http::error::AppError`, which downcasts to
/// this and answers `503`.
#[derive(Clone, Debug, thiserror::Error)]
#[error("another upload of LFS object {oid} is still publishing after {waited_seconds}s")]
pub struct LfsPublicationBusy {
    pub oid: String,
    pub waited_seconds: u64,
}

/// The right to publish one LFS object's content-addressed key.
///
/// Held from before the `exists` probe until after the metadata commit, so the
/// whole decision — reuse or publish, commit or roll back — happens with no
/// other publisher able to interleave.
struct PublicationLease {
    object_id: i64,
    token: String,
    /// Whether this lease was granted rather than taken over from an expired
    /// holder. A taken-over lease cannot prove the previous holder is gone, so
    /// bytes found under the key may still be theirs.
    exclusive: bool,
}

/// Publish the compressed bytes and commit the metadata under an exclusive
/// lease on the object's key.
async fn publish_object(
    db: &DatabaseConnection,
    storage: &dyn BlobStorage,
    request: PublicationRequest<'_>,
) -> Result<()> {
    let lease = acquire_publication_lease(db, request.object_id, request.oid).await?;
    let published = publish_under_lease(db, storage, &request, &lease).await;
    release_publication_lease(db, &lease, request.oid).await;
    published
}

/// Wait for, then take, the object's publication lease.
///
/// Polling rather than blocking a database transaction is deliberate: the lease
/// is held across a blob write that can take as long as the object is large,
/// and holding a connection open for that would starve the pool long before it
/// protected anything.
async fn acquire_publication_lease(
    db: &DatabaseConnection,
    object_id: i64,
    oid: &str,
) -> Result<PublicationLease> {
    let token = uuid::Uuid::new_v4().to_string();
    let deadline = std::time::Instant::now() + PUBLICATION_LEASE_WAIT;
    let mut backoff = PUBLICATION_LEASE_POLL_MIN;

    loop {
        let stale_before = Utc::now() - chrono::Duration::seconds(PUBLICATION_LEASE_TTL_SECONDS);
        match lfs_object_ops::bid_for_publication_lease(db, object_id, &token, stale_before).await?
        {
            lfs_object_ops::PublicationLeaseBid::Granted => {
                return Ok(PublicationLease {
                    object_id,
                    token,
                    exclusive: true,
                })
            }
            lfs_object_ops::PublicationLeaseBid::TakenOver => {
                tracing::warn!(
                    oid = %oid,
                    object_id,
                    "took over an expired LFS publication lease — the previous publisher never released it, so this request will keep any blob it cannot prove is its own"
                );
                return Ok(PublicationLease {
                    object_id,
                    token,
                    exclusive: false,
                });
            }
            lfs_object_ops::PublicationLeaseBid::Busy => {}
        }

        if std::time::Instant::now() >= deadline {
            return Err(LfsPublicationBusy {
                oid: oid.to_string(),
                waited_seconds: PUBLICATION_LEASE_WAIT.as_secs(),
            }
            .into());
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(PUBLICATION_LEASE_POLL_MAX);
    }
}

/// Give the lease back. A release that finds nothing to release means the lease
/// had already been taken over, which is worth saying out loud — the request
/// that took it over may have published bytes this one still believes are its.
async fn release_publication_lease(db: &DatabaseConnection, lease: &PublicationLease, oid: &str) {
    match lfs_object_ops::release_publication_lease(db, lease.object_id, &lease.token).await {
        Ok(true) => {}
        Ok(false) => tracing::warn!(
            oid = %oid,
            object_id = lease.object_id,
            "LFS publication lease was taken over before this request released it"
        ),
        Err(error) => tracing::warn!(
            oid = %oid,
            object_id = lease.object_id,
            error = %format!("{error:#}"),
            "failed to release the LFS publication lease — concurrent uploads of this object wait until it expires"
        ),
    }
}

async fn publish_under_lease(
    db: &DatabaseConnection,
    storage: &dyn BlobStorage,
    request: &PublicationRequest<'_>,
    lease: &PublicationLease,
) -> Result<()> {
    let publication = if storage.exists(request.key).await? {
        BlobPublication::Reused
    } else {
        match request.source {
            PublicationSource::Buffered(bytes) => storage.put(request.key, bytes).await?,
            PublicationSource::File(path) => storage.put_file(request.key, path).await?,
            PublicationSource::Shared(path) => storage.put_file_shared(request.key, path).await?,
        };
        BlobPublication::Published
    };

    mark_uploaded(db, storage, request, publication, lease).await
}

/// Mark a stored LFS object uploaded, rolling back only bytes this call owns.
///
/// Between the `put` and this update the blob is in storage while its row still
/// reads `uploaded = false`: downloads refuse it and retention — which walks
/// rows — never comes back for it. That makes compensation necessary for every
/// failure after a new publication, but destructive for a retry that reused a
/// blob already claimed by an earlier successful upload.
async fn mark_uploaded(
    db: &DatabaseConnection,
    storage: &dyn BlobStorage,
    request: &PublicationRequest<'_>,
    publication: BlobPublication,
    lease: &PublicationLease,
) -> Result<()> {
    let obj = match lfs_object_ops::find_by_repo_and_oid(db, request.repo_id, request.oid).await {
        Ok(Some(obj)) => obj,
        Ok(None) => {
            discard_stored_blob(db, storage, request, publication, lease).await;
            anyhow::bail!("LFS object {} not found after create", request.oid);
        }
        Err(error) => {
            discard_stored_blob(db, storage, request, publication, lease).await;
            return Err(error).context("db: reload LFS object after store");
        }
    };

    let mut model: lfs_object::ActiveModel = obj.into();
    model.uploaded = sea_orm::Set(true);
    if let Err(error) = model.update(db).await {
        discard_stored_blob(db, storage, request, publication, lease).await;
        return Err(error).context("db: update LFS object after store");
    }

    Ok(())
}

/// Roll back a blob whose row will never claim it as uploaded.
///
/// Three things have to hold before the delete is safe, and each of them is a
/// way an earlier version of this code lost live data:
///
/// * the bytes were published by *this* request, not reused from an earlier one;
/// * the lease was granted, not taken over — a taken-over lease means another
///   process may still be publishing under the same key;
/// * no row currently claims the object as uploaded.
///
/// Anything short of all three leaves the blob in place. An orphan costs disk
/// and is collectable; deleting an object a live row points at is not
/// recoverable at all. The caller must still report the failure that got us
/// here, so a failed rollback can only be logged, never returned.
async fn discard_stored_blob(
    db: &DatabaseConnection,
    storage: &dyn BlobStorage,
    request: &PublicationRequest<'_>,
    publication: BlobPublication,
    lease: &PublicationLease,
) {
    if publication == BlobPublication::Reused {
        return;
    }
    if !lease.exclusive {
        tracing::warn!(
            oid = %request.oid,
            repo_id = request.repo_id,
            blob_key = %request.key,
            "orphaned LFS blob: this request published under a taken-over lease and cannot prove the stored bytes are its own — the blob stays in storage rather than risk deleting a live object"
        );
        return;
    }
    match object_claims_upload(db, request.repo_id, request.oid).await {
        Ok(false) => {}
        Ok(true) => {
            tracing::warn!(
                oid = %request.oid,
                repo_id = request.repo_id,
                blob_key = %request.key,
                "kept the LFS blob after a failed metadata commit: the object's row already reads uploaded=true, so these bytes are serving a live publication"
            );
            return;
        }
        Err(error) => {
            tracing::warn!(
                oid = %request.oid,
                repo_id = request.repo_id,
                blob_key = %request.key,
                error = %format!("{error:#}"),
                "orphaned LFS blob: the rollback could not read the object's row to check for a live publication, so the blob stays in storage"
            );
            return;
        }
    }
    if let Err(cleanup_error) = storage.delete(request.key).await {
        tracing::warn!(
            oid = %request.oid,
            repo_id = request.repo_id,
            blob_key = %request.key,
            error = %cleanup_error,
            "orphaned LFS blob: marking the object uploaded failed and the rollback delete failed too — the blob stays in storage while its row still reads uploaded=false"
        );
    }
}

/// Whether a row currently points at the object's blob as a live upload.
pub async fn object_claims_upload(
    db: &DatabaseConnection,
    repo_id: i64,
    oid: &str,
) -> Result<bool> {
    Ok(lfs_object_ops::find_by_repo_and_oid(db, repo_id, oid)
        .await?
        .is_some_and(|obj| obj.uploaded))
}

/// Store an LFS object from an uncompressed file on disk.
/// Streams the file through zstd compression—never loads the entire
/// object into memory.
///
/// Ownership of `uncompressed_path` transfers here: the caller stages the
/// upload and this function retires it, whichever way it ends.
#[allow(clippy::too_many_arguments)]
pub async fn store_object_from_file(
    db: &DatabaseConnection,
    repo_id: i64,
    storage: &dyn BlobStorage,
    owner: &str,
    repo: &str,
    oid: &str,
    uncompressed_path: &std::path::Path,
    original_size: i64,
) -> Result<()> {
    // Two files are staged in the repository's LFS root for the duration of
    // this call — the upload itself (full object size) and the compressed copy
    // — and no DB row points at either, so nothing ever comes back for them.
    // Hence one cleanup tail over the whole body instead of a discard on the
    // one failure that happened to be noticed: a compression error in the
    // middle used to leave both files behind for good.
    let staged = StagedLfsPublication {
        uncompressed: uncompressed_path.to_path_buf(),
        compressed: uncompressed_path.with_extension(format!("{}.zst", uuid::Uuid::new_v4())),
    };
    stream_compress_and_store(
        db,
        repo_id,
        storage,
        owner,
        repo,
        oid,
        staged,
        original_size,
    )
    .await
}

/// Own both request-private files across the blocking and async halves of an
/// LFS publication. A dropped `spawn_blocking` join future does not stop its
/// closure; moving this guard into that closure keeps cleanup attached to the
/// work even when the request is cancelled while compression is still running.
#[derive(Debug)]
struct StagedLfsPublication {
    uncompressed: PathBuf,
    compressed: PathBuf,
}

impl Drop for StagedLfsPublication {
    fn drop(&mut self) {
        discard_file("uncompressed LFS upload", &self.uncompressed);
        discard_file("compressed LFS object", &self.compressed);
    }
}

#[allow(clippy::too_many_arguments)]
async fn stream_compress_and_store(
    db: &DatabaseConnection,
    repo_id: i64,
    storage: &dyn BlobStorage,
    owner: &str,
    repo: &str,
    oid: &str,
    staged: StagedLfsPublication,
    original_size: i64,
) -> Result<()> {
    // Find or create the DB record first
    let object = find_or_register_object(db, repo_id, oid, original_size).await?;

    // The client controls the object size, so this entire file traversal and
    // zstd phase belongs on the blocking pool. The staging guard moves with the
    // work and comes back for publication, keeping cleanup alive across a
    // cancelled join future.
    let (staged, measured) = run_blocking_lfs_compression(move || {
        let src_file = std::fs::File::open(&staged.uncompressed)
            .with_context(|| format!("open uncompressed file {:?}", staged.uncompressed))?;
        let dst_file = std::fs::File::create(&staged.compressed)
            .with_context(|| format!("create compressed file {:?}", staged.compressed))?;

        let mut encoder = zstd::stream::Encoder::new(dst_file, ZSTD_LEVEL)
            .with_context(|| format!("create zstd stream encoder for {:?}", staged.compressed))?;
        std::io::copy(&mut std::io::BufReader::new(src_file), &mut encoder).with_context(|| {
            format!(
                "stream-compress LFS object from {:?} to {:?}",
                staged.uncompressed, staged.compressed
            )
        })?;
        let finished = encoder
            .finish()
            .with_context(|| format!("finish zstd stream encoding at {:?}", staged.compressed))?;
        let measured = finished.metadata().map(|metadata| metadata.len());
        drop(finished);

        Ok((staged, measured))
    })
    .await?;

    let key = lfs_object_key(owner, repo, oid, true)?;

    publish_compressed_object(
        db,
        storage,
        PublicationRequest {
            object_id: object.id,
            repo_id,
            oid,
            key: &key,
            source: PublicationSource::File(&staged.compressed),
        },
        &staged.compressed,
        original_size,
        measured,
    )
    .await
}

/// Run one complete LFS compression phase without occupying a Tokio worker.
///
/// The operation owns the paths it traverses. Its inner IO error is preserved,
/// while a panic or cancellation receives enough context to identify the
/// publication phase that failed.
async fn run_blocking_lfs_compression<T, F>(operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .context("LFS compression blocking task failed")?
}

/// Commit a staged, already-compressed LFS object under the size it actually
/// has.
///
/// Split out of [`stream_compress_and_store`] so that measuring the finished
/// file is a step of the publication rather than a decoration on the log line.
/// The size used to be taken as `finished.metadata().map(|m| m.len()).unwrap_or(0)`:
/// an `fstat` that failed on the live handle — a yanked bind-mount, a stale NFS
/// descriptor — collapsed into a plausible zero, the object was published on
/// top of it, and the operator was told a `0`-byte object at `0.0%` ratio had
/// been stored. That sends them after a data loss that never happened, while
/// the io error that did happen is dropped without a line anywhere.
///
/// Measuring therefore has to succeed *before* anything is committed — the same
/// ordering the download path needs before it promises a `Content-Length`. The
/// measurement arrives as a parameter because `fstat` on a live descriptor
/// cannot be made to fail on demand: the regression hands this a real staged
/// file together with an injected `io::Error`. `compressed_path` names the file
/// that measurement is about, which the request itself only carries as the
/// source it is about to read.
async fn publish_compressed_object(
    db: &DatabaseConnection,
    storage: &dyn BlobStorage,
    request: PublicationRequest<'_>,
    compressed_path: &std::path::Path,
    original_size: i64,
    measured: std::io::Result<u64>,
) -> Result<()> {
    let oid = request.oid;
    let compressed_size = measured
        .map(|len| len as i64)
        .with_context(|| format!("measure compressed LFS object {compressed_path:?}"))?;

    publish_object(db, storage, request).await?;

    // After the publication, for the same reason as the buffered path.
    tracing::info!(
        oid = %oid,
        original_size = original_size,
        compressed_size = compressed_size,
        ratio = format!("{:.1}%", if original_size > 0 { (compressed_size as f64 / original_size as f64) * 100.0 } else { 0.0 }),
        "LFS object stream-compressed and stored"
    );
    Ok(())
}

/// Locate the legacy on-disk copy of an object for streaming.
///
/// Returns `(file_path, is_compressed)`; the caller stream-decompresses when
/// `is_compressed`. Private because the only legitimate entry point is
/// [`read_object_source`], which asks the blob storage first and falls back
/// here — a caller that reached straight for this one would answer `404` for
/// every object stored on a non-local backend.
fn read_object_path(lfs_root: &std::path::Path, oid: &str) -> Result<(PathBuf, bool)> {
    let obj_path = lfs_object_path(lfs_root, oid)?;

    // Try compressed version first (.zst)
    let compressed_path = obj_path.with_extension("zst");
    if legacy_object_present(&compressed_path)? {
        return Ok((compressed_path, true));
    }

    // Fallback to uncompressed (legacy)
    if legacy_object_present(&obj_path)? {
        return Ok((obj_path, false));
    }

    // Typed, not a message: this is the one branch that proves the object is
    // gone, and the caller has to answer `404` to it and `5xx` to everything
    // above. Flattened into a string, the two were indistinguishable and the
    // download handler answered `404` to both — telling `git lfs pull` that an
    // unreachable blob store had deleted the objects. `NotFound`'s own
    // `Display` is also the fixed text that reaches the client, so unlike the
    // old `bail!` it carries neither the oid nor a path (H-05).
    Err(crate::error::not_found("LFS object"))
}

/// Whether a legacy on-disk object is there — keeping "it is not" apart from
/// "could not tell".
///
/// `Path::exists` answers `false` to both, so an LFS root the server cannot
/// stat (a bind-mount owned by another uid, a plain file where the shard
/// directory belongs) read as an absent object. That is the same collapse the
/// blob-storage half of [`read_object_source`] avoids by returning its error,
/// and leaving it here would let the fallback path re-introduce it.
fn legacy_object_present(path: &std::path::Path) -> Result<bool> {
    path.try_exists().map_err(|error| {
        anyhow::anyhow!(crate::platform::fs::describe_path_error(
            "legacy LFS object",
            path,
            &error,
            crate::platform::fs::LFS_STORAGE_HINT,
        ))
    })
}

/// Backend-neutral download source. Local storage retains streaming file I/O;
/// remote backends may return bytes until STORAGE-002 adds signed/streamed reads.
pub enum LfsObjectSource {
    Local { path: PathBuf, compressed: bool },
    Bytes { data: Vec<u8>, compressed: bool },
}

/// Resolve where an object's bytes can be read from, blob storage first and the
/// legacy on-disk layout second.
///
/// The error half is a contract the HTTP layer depends on: "this repository
/// does not have the object" arrives as [`crate::error::NotFound`] (or a
/// backend [`crate::blob_storage::BlobStorageError::NotFound`], when it goes
/// missing between the `exists` check and the read), and *every other* failure
/// — an unreachable backend, an unreadable LFS root — keeps its own type so it
/// can be answered as the server's.
pub async fn read_object_source(
    storage: &dyn BlobStorage,
    legacy_lfs_root: &std::path::Path,
    owner: &str,
    repo: &str,
    oid: &str,
) -> Result<LfsObjectSource> {
    for compressed in [true, false] {
        let key = lfs_object_key(owner, repo, oid, compressed)?;
        if storage.exists(&key).await? {
            if let Some(path) = storage.local_path(&key) {
                return Ok(LfsObjectSource::Local { path, compressed });
            }
            return Ok(LfsObjectSource::Bytes {
                data: storage.get(&key).await?,
                compressed,
            });
        }
    }

    let (path, compressed) = read_object_path(legacy_lfs_root, oid)?;
    Ok(LfsObjectSource::Local { path, compressed })
}

/// A repository as the LFS store addresses it: rows by id, bytes by the
/// `<owner>/<name>` pair the storage keys are built from.
#[derive(Clone, Copy, Debug)]
pub struct LfsRepository<'a> {
    pub id: i64,
    pub owner: &'a str,
    pub name: &'a str,
}

/// What [`copy_repository_objects`] carried over.
#[derive(Debug, Default)]
pub struct LfsObjectsCopied {
    /// Objects the destination now serves as its own.
    pub copied: usize,
    /// Objects the source has an uploaded row for but no bytes. The source
    /// answers `404` to them already, so the destination does too: copying
    /// the row would promise an object nobody can serve.
    pub missing: Vec<String>,
}

/// Give `destination` every stored LFS object of `source`, as its own.
///
/// Both halves of an object are per repository — the `(repo_id, oid)` row the
/// batch API looks up and the bytes under a key built from `<owner>/<name>` —
/// so a repository that merely shares history with another (a fork) has
/// neither until they are made for it. They are made as copies rather than as
/// a pointer back to the source, because every lifecycle operation on the
/// source moves or retires its whole prefix: a fork that read through to the
/// source would lose its objects to the source's deletion, rename or transfer,
/// and would keep serving them to people the source has since locked out.
///
/// The bytes are shared where the backend can share them
/// ([`BlobStorage::put_file_shared`]), so on local storage a copy is one hard
/// link per object rather than a second set of gigabytes. The rows go in one
/// transaction after every object is in place, and a failure removes the keys
/// this call published: the destination ends with all of the source's objects
/// or none of them, never a partial set that answers `404` at random.
pub async fn copy_repository_objects(
    db: &DatabaseConnection,
    storage: &dyn BlobStorage,
    repo_root: &std::path::Path,
    source: LfsRepository<'_>,
    destination: LfsRepository<'_>,
) -> Result<LfsObjectsCopied> {
    let objects = lfs_object_ops::list_uploaded_by_repo(db, source.id).await?;
    let legacy_root = lfs_root(repo_root, source.owner, source.name);

    let mut published: Vec<BlobKey> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let result = async {
        let mut rows = Vec::with_capacity(objects.len());
        for object in &objects {
            let Some(key) =
                copy_object_bytes(storage, &legacy_root, source, destination, &object.oid).await?
            else {
                missing.push(object.oid.clone());
                continue;
            };
            published.push(key);
            rows.push(lfs_object::ActiveModel {
                id: sea_orm::NotSet,
                repo_id: sea_orm::Set(destination.id),
                oid: sea_orm::Set(object.oid.clone()),
                size: sea_orm::Set(object.size),
                uploaded: sea_orm::Set(true),
                created_at: sea_orm::Set(Utc::now()),
                publisher_token: sea_orm::Set(None),
                publisher_since: sea_orm::Set(None),
            });
        }
        lfs_object_ops::create_many(db, rows).await
    }
    .await;

    if let Err(error) = result {
        for key in &published {
            if let Err(cleanup) = storage.delete(key).await {
                tracing::warn!(
                    key = %key,
                    error = %cleanup,
                    "an LFS object copied for a repository that did not get its objects could \
                     not be removed; it is unreferenced and safe to delete"
                );
            }
        }
        return Err(error.context(format!(
            "failed to copy the LFS objects of {}/{} to {}/{}",
            source.owner, source.name, destination.owner, destination.name
        )));
    }

    Ok(LfsObjectsCopied {
        copied: published.len(),
        missing,
    })
}

/// Put one object's bytes under `destination`'s key. `Ok(None)` is the one
/// outcome that proves the source has no bytes for `oid`; every other failure
/// is an error.
async fn copy_object_bytes(
    storage: &dyn BlobStorage,
    legacy_root: &std::path::Path,
    source: LfsRepository<'_>,
    destination: LfsRepository<'_>,
    oid: &str,
) -> Result<Option<BlobKey>> {
    let Some(bytes) = locate_object_bytes(storage, legacy_root, source, oid).await? else {
        return Ok(None);
    };
    let to = lfs_object_key(destination.owner, destination.name, oid, bytes.compressed)?;
    match &bytes.location {
        StoredObjectLocation::Local(path) => storage.put_file_shared(&to, path).await?,
        StoredObjectLocation::Remote(from) => storage.put(&to, &storage.get(from).await?).await?,
    };
    Ok(Some(to))
}

/// Where one repository keeps the bytes of one object.
struct StoredObject {
    location: StoredObjectLocation,
    /// Whether the bytes are zstd-compressed, which decides the key suffix
    /// whoever shares them has to publish them under.
    compressed: bool,
}

enum StoredObjectLocation {
    /// A file that can be shared by link.
    Local(std::path::PathBuf),
    /// A key on a backend with no local files: the bytes have to be read
    /// whole, as [`read_object_source`] does for the same backend.
    Remote(BlobKey),
}

/// Find `oid`'s bytes in `source` — blob storage first, the legacy on-disk
/// layout second, the same order [`read_object_source`] reads in. `Ok(None)`
/// means the source has no bytes for it.
async fn locate_object_bytes(
    storage: &dyn BlobStorage,
    legacy_root: &std::path::Path,
    source: LfsRepository<'_>,
    oid: &str,
) -> Result<Option<StoredObject>> {
    for compressed in [true, false] {
        let key = lfs_object_key(source.owner, source.name, oid, compressed)?;
        if !storage.exists(&key).await? {
            continue;
        }
        let location = match storage.local_path(&key) {
            Some(path) => StoredObjectLocation::Local(path),
            None => StoredObjectLocation::Remote(key),
        };
        return Ok(Some(StoredObject {
            location,
            compressed,
        }));
    }

    match read_object_path(legacy_root, oid) {
        Ok((path, compressed)) => Ok(Some(StoredObject {
            location: StoredObjectLocation::Local(path),
            compressed,
        })),
        Err(error) if error.downcast_ref::<crate::error::NotFound>().is_some() => Ok(None),
        Err(error) => Err(error),
    }
}

/// What [`adopt_objects`] found for the oids it was handed.
#[derive(Debug, Default)]
pub struct LfsObjectsAdopted {
    /// Objects the destination did not have and now serves as its own.
    pub adopted: usize,
    /// Objects neither repository stores. The destination would answer `404`
    /// for them, and nothing here can change that.
    pub missing: Vec<String>,
}

/// Make sure `destination` serves each of `oids`, taking from `source` the
/// ones it does not have yet.
///
/// This is how LFS content follows history from one repository into another
/// that already exists — a pull request from a fork being merged. Unlike
/// [`copy_repository_objects`], which fills a repository nobody else can see
/// yet, the destination here is live: its users may be uploading the very same
/// object right now. So every object goes through the same per-object
/// publication lease an upload takes, registered and marked uploaded the way an
/// upload would be, and an object that arrives is never taken away again. A
/// failure halfway leaves the destination with some extra objects nothing
/// points at yet — exactly what an upload whose push never came leaves.
///
/// An object the destination already records as uploaded is left alone, which
/// is the same test its batch API answers downloads by.
pub async fn adopt_objects(
    db: &DatabaseConnection,
    storage: &dyn BlobStorage,
    repo_root: &std::path::Path,
    source: LfsRepository<'_>,
    destination: LfsRepository<'_>,
    oids: &[String],
) -> Result<LfsObjectsAdopted> {
    let legacy_root = lfs_root(repo_root, source.owner, source.name);
    let mut outcome = LfsObjectsAdopted::default();
    for oid in oids {
        if object_claims_upload(db, destination.id, oid).await? {
            continue;
        }
        let stored = match lfs_object_ops::find_by_repo_and_oid(db, source.id, oid).await? {
            Some(row) if row.uploaded => locate_object_bytes(storage, &legacy_root, source, oid)
                .await?
                .map(|bytes| (row.size, bytes)),
            _ => None,
        };
        let Some((size, bytes)) = stored else {
            outcome.missing.push(oid.clone());
            continue;
        };

        let key = lfs_object_key(destination.owner, destination.name, oid, bytes.compressed)?;
        let object = find_or_register_object(db, destination.id, oid, size).await?;
        let buffered;
        let publication_source = match &bytes.location {
            StoredObjectLocation::Local(path) => PublicationSource::Shared(path),
            StoredObjectLocation::Remote(from) => {
                buffered = storage.get(from).await?;
                PublicationSource::Buffered(&buffered)
            }
        };
        publish_object(
            db,
            storage,
            PublicationRequest {
                object_id: object.id,
                repo_id: destination.id,
                oid,
                key: &key,
                source: publication_source,
            },
        )
        .await
        .with_context(|| {
            format!(
                "failed to give {}/{} the LFS object {oid} of {}/{}",
                destination.owner, destination.name, source.owner, source.name
            )
        })?;
        outcome.adopted += 1;
    }
    Ok(outcome)
}

// ── Compression helpers ───────────────────────────────────────────────────────

/// Compress data using zstd.
#[cfg(test)]
fn compress_data(data: &[u8]) -> Result<Vec<u8>> {
    let mut compressed = Vec::with_capacity(data.len());
    let mut encoder =
        zstd::Encoder::new(&mut compressed, ZSTD_LEVEL).context("failed to create zstd encoder")?;
    encoder
        .write_all(data)
        .context("failed to write data to zstd encoder")?;
    encoder.finish().context("failed to finish zstd encoding")?;
    Ok(compressed)
}

/// Decompress zstd data.
///
/// Test-only: production decompression happens at the streaming edge in
/// `rg_http::api::lfs`, which decodes on a blocking thread straight into the
/// response body rather than buffering a whole object here. This one exists so
/// the publication tests can read back what they stored.
#[cfg(test)]
fn decompress_data(compressed: &[u8]) -> Result<Vec<u8>> {
    let mut decompressed = Vec::new();
    let mut decoder = zstd::Decoder::new(compressed).context("failed to create zstd decoder")?;
    std::io::copy(&mut decoder, &mut decompressed).context("failed to decompress zstd data")?;
    Ok(decompressed)
}

#[cfg(test)]
mod blob_publication_tests {
    use super::{
        compress_data, decompress_data, find_or_register_object, lfs_object_key,
        publish_compressed_object, run_blocking_lfs_compression, store_object,
        store_object_from_file, PublicationRequest, PublicationSource, StagedLfsPublication,
    };
    use crate::blob_storage::{
        BlobKey, BlobMetadata, BlobStorage, LocalBlobStorage, Result as BlobResult,
    };
    use futures::future::BoxFuture;
    use rg_db::entities::lfs_object;
    use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, NotSet, Set};
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    /// Saturating every async worker with an LFS compression phase must still
    /// leave a worker available for a cheap request. Running `operation`
    /// directly makes the timing tooth fail when both workers reach the
    /// barrier and remain there until the release thread wakes them.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn lfs_compression_does_not_occupy_async_workers() {
        use std::sync::{Arc, Barrier};
        use std::time::{Duration, Instant};

        const PARALLEL_COMPRESSIONS: usize = 2;
        let release = Arc::new(Barrier::new(PARALLEL_COMPRESSIONS + 1));
        let release_thread = {
            let release = Arc::clone(&release);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(250));
                release.wait();
            })
        };

        let started_at = Instant::now();
        let (operations, entered): (Vec<_>, Vec<_>) = (0..PARALLEL_COMPRESSIONS)
            .map(|marker| {
                let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
                let release = Arc::clone(&release);
                let operation = tokio::spawn(async move {
                    run_blocking_lfs_compression(move || {
                        entered_tx
                            .send(())
                            .expect("test still awaits the compression start signal");
                        release.wait();
                        Ok(marker)
                    })
                    .await
                });
                (operation, entered_rx)
            })
            .unzip();

        for entered in entered {
            entered.await.expect("LFS compression starts");
        }
        let cheap_task = tokio::spawn(async { tokio::task::yield_now().await });
        tokio::time::timeout(Duration::from_millis(100), cheap_task)
            .await
            .expect("a cheap async task was delayed by LFS compression")
            .expect("cheap async task joins");
        assert!(
            started_at.elapsed() < Duration::from_millis(200),
            "LFS compression occupied every async worker"
        );

        for (expected, operation) in operations.into_iter().enumerate() {
            assert_eq!(
                operation
                    .await
                    .expect("LFS compression task joins")
                    .expect("LFS compression succeeds"),
                expected
            );
        }
        release_thread.join().expect("release thread joins");
    }

    #[tokio::test]
    async fn a_panicked_lfs_compression_keeps_its_operation_context() {
        let error = run_blocking_lfs_compression(|| -> anyhow::Result<()> {
            panic!("injected LFS compression panic")
        })
        .await
        .expect_err("a panicked compression task must fail the caller");

        assert!(
            error
                .to_string()
                .contains("LFS compression blocking task failed"),
            "JoinError lost the LFS compression context: {error:#}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_lfs_compression_keeps_staging_cleanup_attached_to_the_work() {
        use std::sync::{Arc, Barrier};
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let staged = StagedLfsPublication {
            uncompressed: dir.path().join("cancelled.upload"),
            compressed: dir.path().join("cancelled.zst"),
        };
        std::fs::write(&staged.uncompressed, b"source").unwrap();
        std::fs::write(&staged.compressed, b"compressed").unwrap();
        let uncompressed = staged.uncompressed.clone();
        let compressed = staged.compressed.clone();

        let release = Arc::new(Barrier::new(2));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let operation_release = Arc::clone(&release);
        let operation = tokio::spawn(async move {
            run_blocking_lfs_compression(move || {
                entered_tx
                    .send(())
                    .expect("test still awaits the compression start signal");
                operation_release.wait();
                Ok(staged)
            })
            .await
        });

        entered_rx.await.expect("LFS compression starts");
        operation.abort();
        assert!(
            operation
                .await
                .expect_err("the request task was aborted")
                .is_cancelled(),
            "the request must be cancelled while blocking work still owns its staging files"
        );
        release.wait();

        for _ in 0..100 {
            if !uncompressed.exists() && !compressed.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "cancelled LFS compression leaked staging files: source={}, compressed={}",
            uncompressed.exists(),
            compressed.exists()
        );
    }

    /// The helper probe proves its scheduling contract; this guard proves that
    /// the production upload actually nests every synchronous traversal inside
    /// that boundary rather than merely calling the helper elsewhere.
    #[test]
    fn lfs_file_publication_uses_the_blocking_boundary() {
        let source = include_str!("service.rs");
        let boundary = rust_source::production_function_call_sites(
            source,
            "stream_compress_and_store",
            &["run_blocking_lfs_compression"],
        );
        assert_eq!(
            boundary.len(),
            1,
            "LFS publication must have one compression boundary, found {boundary:?}"
        );

        for blocking_call in [
            "std::fs::File::open",
            "std::fs::File::create",
            "zstd::stream::Encoder::new",
            "std::io::copy",
            "finish",
            "metadata",
        ] {
            let calls = rust_source::production_function_call_sites(
                source,
                "stream_compress_and_store",
                &[blocking_call],
            );
            assert_eq!(
                calls.len(),
                1,
                "LFS publication must make one `{blocking_call}` call, found {calls:?}"
            );
            assert!(
                rust_source::call_site_contains(source, boundary[0], calls[0]),
                "LFS publication's `{blocking_call}` call is outside its blocking boundary"
            );
        }
    }

    /// A rendezvous point a request can be held at, and the test can observe.
    ///
    /// Two semaphores rather than a `Notify` because both edges have to survive
    /// being signalled before anyone waits: the test must be able to ask "has it
    /// parked yet?" without racing the answer.
    #[derive(Clone)]
    struct Gate {
        reached: Arc<Semaphore>,
        resume: Arc<Semaphore>,
    }

    impl Gate {
        fn new() -> Self {
            Self {
                reached: Arc::new(Semaphore::new(0)),
                resume: Arc::new(Semaphore::new(0)),
            }
        }

        /// Announce arrival, then wait to be let go.
        async fn park(&self) {
            self.reached.add_permits(1);
            self.resume
                .acquire()
                .await
                .expect("the gate outlives the parked request")
                .forget();
        }

        fn has_parked(&self) -> bool {
            self.reached.available_permits() > 0
        }

        /// Wait for the request to arrive. Returns false on timeout so a broken
        /// protocol fails an assertion instead of hanging the suite.
        async fn await_arrival(&self) -> bool {
            match tokio::time::timeout(std::time::Duration::from_secs(10), self.reached.acquire())
                .await
            {
                Ok(permit) => {
                    permit
                        .expect("the gate outlives the parked request")
                        .forget();
                    true
                }
                Err(_) => false,
            }
        }

        fn release(&self) {
            self.resume.add_permits(1);
        }
    }

    /// What a storage backend does once the bytes are on the shared key but the
    /// metadata commit has not run yet — the window every concurrent-publication
    /// hazard lives in.
    enum PutHook {
        None,
        /// Hold the request there so the test can drive a second one into the
        /// same window.
        Park(Gate),
        /// Stand in for a concurrent publisher that reused these very bytes and
        /// got its own commit in first.
        ClaimRow {
            db: DatabaseConnection,
            oid: String,
        },
    }

    impl PutHook {
        async fn run(&self) {
            match self {
                PutHook::None => {}
                PutHook::Park(gate) => gate.park().await,
                PutHook::ClaimRow { db, oid } => {
                    // The stand-in is a different request, so the fault injected
                    // into *this* one must not swallow its commit.
                    set_lfs_metadata_commits(db, false).await;
                    db.execute_unprepared(&format!(
                        "UPDATE lfs_objects SET uploaded = 1 WHERE oid = '{oid}'"
                    ))
                    .await
                    .expect("the stand-in publisher commits");
                    set_lfs_metadata_commits(db, true).await;
                }
            }
        }
    }

    /// Backend-shaped proxy with no `local_path` shortcut. The production tree
    /// currently ships a local backend, but LFS ownership must use only the
    /// portable object-store contract so an S3-like backend gets the same
    /// compensation semantics.
    struct RemoteBlobStorage {
        inner: LocalBlobStorage,
        after_put: PutHook,
    }

    impl RemoteBlobStorage {
        fn new(root: &std::path::Path) -> Self {
            Self {
                inner: LocalBlobStorage::new(root),
                after_put: PutHook::None,
            }
        }

        fn with_hook(root: &std::path::Path, after_put: PutHook) -> Self {
            Self {
                inner: LocalBlobStorage::new(root),
                after_put,
            }
        }
    }

    impl BlobStorage for RemoteBlobStorage {
        fn backend_name(&self) -> &'static str {
            "remote-test"
        }

        fn put<'a>(
            &'a self,
            key: &'a BlobKey,
            data: &'a [u8],
        ) -> BoxFuture<'a, BlobResult<BlobMetadata>> {
            Box::pin(async move {
                let metadata = self.inner.put(key, data).await?;
                self.after_put.run().await;
                Ok(metadata)
            })
        }

        fn put_file<'a>(
            &'a self,
            key: &'a BlobKey,
            source: &'a std::path::Path,
        ) -> BoxFuture<'a, BlobResult<BlobMetadata>> {
            Box::pin(async move {
                let metadata = self.inner.put_file(key, source).await?;
                self.after_put.run().await;
                Ok(metadata)
            })
        }

        fn get<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, BlobResult<Vec<u8>>> {
            self.inner.get(key)
        }

        fn metadata<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, BlobResult<BlobMetadata>> {
            self.inner.metadata(key)
        }

        fn exists<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, BlobResult<bool>> {
            self.inner.exists(key)
        }

        fn delete<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, BlobResult<bool>> {
            self.inner.delete(key)
        }

        fn list<'a>(
            &'a self,
            prefix: Option<&'a BlobKey>,
        ) -> BoxFuture<'a, BlobResult<Vec<BlobMetadata>>> {
            self.inner.list(prefix)
        }
    }

    async fn setup_db() -> DatabaseConnection {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = Database::connect(options).await.unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        db
    }

    /// Break the metadata commit and nothing else, from now until told otherwise.
    ///
    /// `UPDATE OF uploaded` is the point of the injection: the fault under test
    /// is "the row never records the upload", not "no statement may touch this
    /// table". A blanket trigger would also break the publication lease's own
    /// bookkeeping, and the rollback would then be exercised in a state no
    /// production failure produces.
    ///
    /// The switch table exists so a test can let exactly one of two racing
    /// commits through — the shape the concurrency hazard needs.
    async fn fail_lfs_metadata_commits(db: &DatabaseConnection) {
        db.execute_unprepared(
            "CREATE TABLE lfs_commit_switch (fail INTEGER NOT NULL); \
             INSERT INTO lfs_commit_switch (fail) VALUES (1); \
             CREATE TRIGGER fail_lfs_updates \
             BEFORE UPDATE OF uploaded ON lfs_objects \
             WHEN (SELECT fail FROM lfs_commit_switch) = 1 \
             BEGIN SELECT RAISE(FAIL, 'injected lfs update failure'); END",
        )
        .await
        .unwrap();
    }

    async fn set_lfs_metadata_commits(db: &DatabaseConnection, fail: bool) {
        db.execute_unprepared(&format!(
            "UPDATE lfs_commit_switch SET fail = {}",
            i32::from(fail)
        ))
        .await
        .unwrap();
    }

    fn oid(payload: &[u8]) -> String {
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(payload))
    }

    async fn insert_uploaded_object(
        db: &DatabaseConnection,
        repo_id: i64,
        oid: &str,
        payload: &[u8],
    ) {
        rg_db::ops::lfs_object_ops::create(
            db,
            lfs_object::ActiveModel {
                id: NotSet,
                repo_id: Set(repo_id),
                oid: Set(oid.to_string()),
                size: Set(payload.len() as i64),
                uploaded: Set(true),
                created_at: Set(chrono::Utc::now()),
                publisher_token: Set(None),
                publisher_since: Set(None),
            },
        )
        .await
        .unwrap();
    }

    /// Both publication sources must carry the publication outcome to
    /// `mark_uploaded`: the buffered source exercises the same rollback branch
    /// without pretending to be a second production upload API.
    #[tokio::test]
    async fn failed_retries_keep_reused_blobs_for_buffered_and_file_stores() {
        let dir = tempfile::tempdir().unwrap();
        let db = setup_db().await;
        let storage = RemoteBlobStorage::new(&dir.path().join("remote"));

        let buffered_payload = b"buffered LFS retry";
        let buffered_oid = oid(buffered_payload);
        let buffered_key = lfs_object_key("owner", "repo", &buffered_oid, true).unwrap();
        let buffered_compressed = compress_data(buffered_payload).unwrap();
        storage
            .put(&buffered_key, &buffered_compressed)
            .await
            .unwrap();
        insert_uploaded_object(&db, 1, &buffered_oid, buffered_payload).await;

        let file_payload = b"file-backed LFS retry";
        let file_oid = oid(file_payload);
        let file_key = lfs_object_key("owner", "repo", &file_oid, true).unwrap();
        let file_compressed = compress_data(file_payload).unwrap();
        storage.put(&file_key, &file_compressed).await.unwrap();
        insert_uploaded_object(&db, 1, &file_oid, file_payload).await;

        fail_lfs_metadata_commits(&db).await;

        let buffered_error = store_object(
            &db,
            1,
            &storage,
            "owner",
            "repo",
            &buffered_oid,
            buffered_payload,
        )
        .await
        .expect_err("the injected buffered metadata failure must land");
        assert!(buffered_error.to_string().contains("update LFS object"));

        let source = dir.path().join("file-retry.upload");
        std::fs::write(&source, file_payload).unwrap();
        let file_error = store_object_from_file(
            &db,
            1,
            &storage,
            "owner",
            "repo",
            &file_oid,
            &source,
            file_payload.len() as i64,
        )
        .await
        .expect_err("the injected file metadata failure must land");
        assert!(file_error.to_string().contains("update LFS object"));

        assert_eq!(
            storage.get(&buffered_key).await.unwrap(),
            buffered_compressed
        );
        assert_eq!(storage.get(&file_key).await.unwrap(), file_compressed);
    }

    /// Compressing the object and measuring the result are two failures, not
    /// one. The size was read as `finished.metadata().map(|m| m.len()).unwrap_or(0)`,
    /// so an `fstat` that failed on the live handle published the object under
    /// a plausible zero and announced `compressed_size = 0` at `0.0%` ratio —
    /// a data loss the operator never had, in place of the io error they did.
    #[tokio::test]
    async fn an_unmeasurable_compressed_object_is_not_published() {
        let dir = tempfile::tempdir().unwrap();
        let db = setup_db().await;
        let storage = RemoteBlobStorage::new(&dir.path().join("remote"));

        let payload = b"stream-compressed LFS object";
        let oid = oid(payload);
        let key = lfs_object_key("owner", "repo", &oid, true).unwrap();
        let object = find_or_register_object(&db, 1, &oid, payload.len() as i64)
            .await
            .unwrap();
        // The staged file is real and readable: the only thing failing here is
        // the measurement of it.
        let staged = dir.path().join("staged.zst");
        std::fs::write(&staged, compress_data(payload).unwrap()).unwrap();

        let refusal = publish_compressed_object(
            &db,
            &storage,
            PublicationRequest {
                object_id: object.id,
                repo_id: 1,
                oid: &oid,
                key: &key,
                source: PublicationSource::File(&staged),
            },
            &staged,
            payload.len() as i64,
            Err(std::io::Error::other("stale NFS file handle")),
        )
        .await
        .expect_err("an object whose size cannot be told must not be published");

        let rendered = format!("{refusal:#}");
        assert!(
            rendered.contains(&staged.display().to_string()),
            "the refusal has to name the file it could not measure: {rendered}"
        );
        assert!(
            rendered.contains("stale NFS file handle"),
            "the refusal has to carry the original io error: {rendered}"
        );

        assert!(
            !storage.exists(&key).await.unwrap(),
            "the object was published even though its size could not be told"
        );
        let row = rg_db::ops::lfs_object_ops::find_by_repo_and_oid(&db, 1, &oid)
            .await
            .unwrap()
            .expect("the row registered before the publication stays");
        assert!(
            !row.uploaded,
            "the refusal left the object claiming to be uploaded"
        );
    }

    /// The other half of the same contract: a measurement that succeeds still
    /// publishes, and the bytes under the key are the real compressed object
    /// rather than the zero the log line used to invent.
    #[tokio::test]
    async fn a_measured_compressed_object_is_published_at_its_real_size() {
        let dir = tempfile::tempdir().unwrap();
        let db = setup_db().await;
        let storage = RemoteBlobStorage::new(&dir.path().join("remote"));

        let payload = b"stream-compressed LFS object that really is stored";
        let oid = oid(payload);
        let key = lfs_object_key("owner", "repo", &oid, true).unwrap();
        let source = dir.path().join("upload.bin");
        std::fs::write(&source, payload).unwrap();

        store_object_from_file(
            &db,
            1,
            &storage,
            "owner",
            "repo",
            &oid,
            &source,
            payload.len() as i64,
        )
        .await
        .expect("a measurable object publishes");

        let stored = storage.get(&key).await.unwrap();
        assert!(
            !stored.is_empty(),
            "the published object is the empty one the swallowed measurement described"
        );
        assert_eq!(decompress_data(&stored).unwrap(), payload);
        let row = rg_db::ops::lfs_object_ops::find_by_repo_and_oid(&db, 1, &oid)
            .await
            .unwrap()
            .expect("the published object has a row");
        assert!(row.uploaded);
        assert!(!source.exists(), "the staged upload was left on disk");
    }

    /// The ownership guard must not turn into a blanket "never clean up": a
    /// request that actually published a new blob still owns its rollback.
    #[tokio::test]
    async fn failed_first_publications_remove_blobs_for_buffered_and_file_stores() {
        let dir = tempfile::tempdir().unwrap();
        let db = setup_db().await;
        let storage = RemoteBlobStorage::new(&dir.path().join("remote"));
        fail_lfs_metadata_commits(&db).await;

        let buffered_payload = b"new buffered LFS object";
        let buffered_oid = oid(buffered_payload);
        let buffered_key = lfs_object_key("owner", "repo", &buffered_oid, true).unwrap();
        store_object(
            &db,
            1,
            &storage,
            "owner",
            "repo",
            &buffered_oid,
            buffered_payload,
        )
        .await
        .expect_err("the injected buffered metadata failure must land");
        assert!(!storage.exists(&buffered_key).await.unwrap());

        let file_payload = b"new file-backed LFS object";
        let file_oid = oid(file_payload);
        let file_key = lfs_object_key("owner", "repo", &file_oid, true).unwrap();
        let source = dir.path().join("new-file.upload");
        std::fs::write(&source, file_payload).unwrap();
        store_object_from_file(
            &db,
            1,
            &storage,
            "owner",
            "repo",
            &file_oid,
            &source,
            file_payload.len() as i64,
        )
        .await
        .expect_err("the injected file metadata failure must land");
        assert!(!storage.exists(&file_key).await.unwrap());
    }

    /// A database with a real connection pool: two racing publications have to
    /// contend over separate connections, or the serialisation being tested is
    /// the pool's rather than the protocol's.
    async fn pooled_db(dir: &std::path::Path) -> DatabaseConnection {
        let url = format!("sqlite://{}?mode=rwc", dir.join("lfs.db").display());
        let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
            .await
            .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        db
    }

    /// Two first publications of one `oid`, one commit landing and one failing,
    /// must leave the object downloadable.
    ///
    /// The dangerous interleaving is the one this test makes impossible: a
    /// second request observing the first request's not-yet-committed bytes,
    /// adopting them, committing — and then watching the first request's
    /// rollback delete the object its own row now points at. The publication
    /// lease is what keeps the second request out of that window, so it finds
    /// either a finished object or a clean slate and never an ambiguous one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_first_publications_leave_the_object_downloadable() {
        let dir = tempfile::tempdir().unwrap();
        let db = pooled_db(dir.path()).await;
        fail_lfs_metadata_commits(&db).await;

        let payload = b"two clients push the same LFS object at the same moment";
        let oid = oid(payload);
        let key = lfs_object_key("owner", "repo", &oid, true).unwrap();
        let root = dir.path().join("remote");
        let observer = RemoteBlobStorage::new(&root);

        let first_gate = Gate::new();
        let second_gate = Gate::new();
        let first_storage = Arc::new(RemoteBlobStorage::with_hook(
            &root,
            PutHook::Park(first_gate.clone()),
        ));
        let second_storage = Arc::new(RemoteBlobStorage::with_hook(
            &root,
            PutHook::Park(second_gate.clone()),
        ));

        let first = tokio::spawn({
            let db = db.clone();
            let storage = Arc::clone(&first_storage);
            let oid = oid.clone();
            async move { store_object(&db, 1, storage.as_ref(), "owner", "repo", &oid, payload).await }
        });
        assert!(
            first_gate.await_arrival().await,
            "the first request must reach the window between its blob write and its commit"
        );

        // Only now does the second request start, with the first one holding the
        // lease and its bytes already under the shared key.
        let source = dir.path().join("second.upload");
        std::fs::write(&source, payload).unwrap();
        let second = tokio::spawn({
            let db = db.clone();
            let storage = Arc::clone(&second_storage);
            let oid = oid.clone();
            let source = source.clone();
            async move {
                store_object_from_file(
                    &db,
                    1,
                    storage.as_ref(),
                    "owner",
                    "repo",
                    &oid,
                    &source,
                    payload.len() as i64,
                )
                .await
            }
        });

        // It must not get as far as writing bytes while the first request still
        // owns the key — adopting those bytes is the whole hazard.
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        assert!(
            !second_gate.has_parked(),
            "the second publication reached the shared key while the first still held the lease"
        );

        first_gate.release();
        let first_error = first
            .await
            .unwrap()
            .expect_err("the injected metadata failure must land on the first publication");
        assert!(first_error.to_string().contains("update LFS object"));

        // The second request now owns the key. Let its commit through.
        assert!(
            second_gate.await_arrival().await,
            "the second publication never wrote its own bytes: it either adopted the first request's \
             withdrawn blob or gave up on the lease"
        );
        set_lfs_metadata_commits(&db, false).await;
        second_gate.release();
        second
            .await
            .unwrap()
            .expect("the second publication owns the key and must succeed");

        let row = rg_db::ops::lfs_object_ops::find_by_repo_and_oid(&db, 1, &oid)
            .await
            .unwrap()
            .expect("the object is registered");
        assert!(row.uploaded, "the surviving publication must be recorded");
        assert!(
            row.publisher_token.is_none(),
            "both publications must have released their lease"
        );
        assert!(
            observer.exists(&key).await.unwrap(),
            "a rollback deleted the blob the surviving row points at"
        );
        assert_eq!(
            decompress_data(&observer.get(&key).await.unwrap()).unwrap(),
            payload,
            "the row points at a blob that is missing or corrupt"
        );
    }

    /// A rollback must never delete bytes a live row already claims, whatever
    /// ordering produced that row.
    ///
    /// The lease keeps two publishers apart, but a lease taken over from a
    /// process that only *looked* dead does not, so the compensation carries its
    /// own proof: it re-reads the row and refuses to delete bytes an upload is
    /// already being served from. Losing disk to an orphan is recoverable;
    /// deleting a live object is not.
    #[tokio::test]
    async fn a_rollback_keeps_bytes_a_live_row_already_claims() {
        let dir = tempfile::tempdir().unwrap();
        let db = setup_db().await;
        fail_lfs_metadata_commits(&db).await;

        let payload = b"a competing publication got its commit in first";
        let oid = oid(payload);
        let key = lfs_object_key("owner", "repo", &oid, true).unwrap();
        let root = dir.path().join("remote");
        let observer = RemoteBlobStorage::new(&root);
        let storage = RemoteBlobStorage::with_hook(
            &root,
            PutHook::ClaimRow {
                db: db.clone(),
                oid: oid.clone(),
            },
        );

        let error = store_object(&db, 1, &storage, "owner", "repo", &oid, payload)
            .await
            .expect_err("the injected metadata failure must land");
        assert!(error.to_string().contains("update LFS object"));

        let row = rg_db::ops::lfs_object_ops::find_by_repo_and_oid(&db, 1, &oid)
            .await
            .unwrap()
            .expect("the object is registered");
        assert!(row.uploaded, "the competing publication stays recorded");
        assert!(
            observer.exists(&key).await.unwrap(),
            "a rollback deleted the blob the surviving row points at"
        );
        assert_eq!(
            decompress_data(&observer.get(&key).await.unwrap()).unwrap(),
            payload,
            "the rollback deleted bytes a live row points at"
        );
    }
}

#[cfg(test)]
mod transfer_negotiation_tests {
    use super::negotiate_transfer;

    fn offered(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    /// What git-lfs 3.x sends: its local-file adapter first, `basic` after it.
    #[test]
    fn basic_is_chosen_wherever_the_client_lists_it() {
        for list in [
            offered(&["lfs-standalone-file", "basic", "ssh"]),
            offered(&["basic"]),
            offered(&["tus", "basic"]),
        ] {
            assert_eq!(
                negotiate_transfer(Some(&list)).unwrap(),
                "basic",
                "{list:?}"
            );
        }
        assert_eq!(negotiate_transfer(None).unwrap(), "basic");
    }

    #[test]
    fn an_offer_without_basic_is_refused_as_the_clients_request() {
        let error = negotiate_transfer(Some(&offered(&["lfs-standalone-file", "ssh"])))
            .expect_err("nothing offered is served here");
        assert!(error
            .downcast_ref::<crate::error::InvalidRequest>()
            .is_some());
        let error = negotiate_transfer(Some(&[])).expect_err("an empty offer has no adapter");
        assert!(error
            .downcast_ref::<crate::error::InvalidRequest>()
            .is_some());
    }
}

#[cfg(test)]
mod oid_validation_tests {
    use super::{is_valid_oid, lfs_object_path, lfs_request_error, LFS_OBJECT_MAX_BYTES};

    // is_valid_oid is the input gate for `batch()` and for the object routes in
    // rg-http; it also protects lfs_object_path from path-traversal, so its
    // rejection behavior is security-relevant.

    #[test]
    fn accepts_canonical_64_lowercase_hex() {
        assert!(is_valid_oid(&"a".repeat(64)));
        assert!(is_valid_oid(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        ));
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(!is_valid_oid(""));
        assert!(!is_valid_oid(&"a".repeat(63)));
        assert!(!is_valid_oid(&"a".repeat(65)));
    }

    #[test]
    fn rejects_uppercase_and_non_hex() {
        assert!(
            !is_valid_oid(&"A".repeat(64)),
            "uppercase hex must be rejected"
        );
        assert!(
            !is_valid_oid(&"g".repeat(64)),
            "'g' is out of the hex range"
        );

        // A path-traversal attempt padded to length 64 must never validate.
        let mut traversal = "a".repeat(60);
        traversal.push_str("/../");
        assert_eq!(traversal.len(), 64);
        assert!(!is_valid_oid(&traversal));
    }

    #[test]
    fn object_path_rejects_invalid_oids_before_sharding() {
        let root = std::path::Path::new("lfs");
        let error = lfs_object_path(root, "x").expect_err("a short oid must be refused");

        assert!(error.to_string().contains("invalid LFS object identifier"));
        assert_eq!(
            lfs_object_path(root, &"a".repeat(64)).unwrap(),
            root.join("aa").join("a".repeat(64))
        );
    }

    #[test]
    fn upload_contract_refuses_a_declared_object_above_the_ceiling() {
        let oid = "a".repeat(64);
        let error = lfs_request_error("upload", &oid, LFS_OBJECT_MAX_BYTES as i64 + 1)
            .expect("an oversized upload must have a per-object error");

        assert_eq!(error.code, 413);
        assert!(error.message.contains(&LFS_OBJECT_MAX_BYTES.to_string()));
        assert!(lfs_request_error("upload", &oid, LFS_OBJECT_MAX_BYTES as i64).is_none());
        assert!(
            lfs_request_error("download", &oid, LFS_OBJECT_MAX_BYTES as i64 + 1).is_none(),
            "the upload ceiling must not hide a legacy object from downloads"
        );
    }
}

#[cfg(test)]
mod ssh_grant_tests {
    use super::{
        parse_actor_token, sign_ssh_grant, verify_ssh_grant, LfsActionKind,
        LfsActionSignatureError, LfsActor, LfsCredential, SshLfsGrant,
    };

    const SECRET: &[u8] = b"grant-secret";
    const EXPIRES: i64 = 2_000_000_000;

    fn key_user() -> LfsActor {
        LfsActor::User {
            user_id: 42,
            credential: LfsCredential::SshKey { id: 9 },
        }
    }

    #[test]
    fn a_grant_round_trips_for_every_actor_the_ssh_port_can_name() {
        for actor in [
            key_user(),
            LfsActor::User {
                user_id: 42,
                credential: LfsCredential::Session { version: 3 },
            },
            LfsActor::DeployKey { key_id: 5 },
        ] {
            for action in [LfsActionKind::Download, LfsActionKind::Upload] {
                let token = sign_ssh_grant(SECRET, action, 7, EXPIRES, actor).unwrap();
                assert_eq!(
                    verify_ssh_grant(SECRET, &token, EXPIRES - 1),
                    Ok(SshLfsGrant {
                        action,
                        repo_id: 7,
                        actor
                    }),
                    "{token}"
                );
            }
        }
    }

    #[test]
    fn a_grant_is_bound_to_its_operation_repository_actor_and_expiry() {
        let token =
            sign_ssh_grant(SECRET, LfsActionKind::Download, 7, EXPIRES, key_user()).unwrap();
        let fields: Vec<&str> = token.split('.').collect();
        assert_eq!(fields.len(), 5, "{token}");

        // Every signed field, edited alone, must stop the grant verifying.
        for (index, replacement) in [
            (0, "upload"),
            (1, "8"),
            (2, "2000000001"),
            (3, "43@k9"),
            (3, "42@s9"),
            (3, "deploy@9"),
        ] {
            let mut edited = fields.clone();
            edited[index] = replacement;
            assert_eq!(
                verify_ssh_grant(SECRET, &edited.join("."), EXPIRES - 1),
                Err(LfsActionSignatureError::Invalid),
                "editing field {index} to {replacement} must invalidate the grant"
            );
        }

        assert_eq!(
            verify_ssh_grant(b"another-secret", &token, EXPIRES - 1),
            Err(LfsActionSignatureError::Invalid)
        );
        assert_eq!(
            verify_ssh_grant(SECRET, &token, EXPIRES),
            Err(LfsActionSignatureError::Expired)
        );
        for malformed in ["", "a.b.c.d", &format!("{token}.extra"), "download.7"] {
            assert_eq!(
                verify_ssh_grant(SECRET, malformed, EXPIRES - 1),
                Err(LfsActionSignatureError::Invalid),
                "{malformed:?}"
            );
        }
    }

    #[test]
    fn an_action_url_signature_is_not_a_grant() {
        // Same secret, same fields — but the action-URL payload carries a
        // different prefix, so one capability cannot be replayed as the other.
        let oid = "a".repeat(64);
        let url_signature = super::sign_action_url(
            SECRET,
            LfsActionKind::Download,
            7,
            &oid,
            EXPIRES,
            Some(key_user()),
        )
        .unwrap();
        let forged = format!("download.7.{EXPIRES}.42@k9.{url_signature}");
        assert_eq!(
            verify_ssh_grant(SECRET, &forged, EXPIRES - 1),
            Err(LfsActionSignatureError::Invalid)
        );
    }

    #[test]
    fn only_canonical_actor_tokens_parse() {
        assert_eq!(parse_actor_token("42@k9"), Some(key_user()));
        assert_eq!(
            parse_actor_token("deploy@5"),
            Some(LfsActor::DeployKey { key_id: 5 })
        );
        for rejected in [
            "anon",
            "42",
            "42@",
            "42@x9",
            "042@k9",
            "+42@k9",
            "42@k09",
            "deploy@",
            "deploy@-1x",
            "@k9",
        ] {
            assert_eq!(parse_actor_token(rejected), None, "{rejected}");
        }
    }
}
