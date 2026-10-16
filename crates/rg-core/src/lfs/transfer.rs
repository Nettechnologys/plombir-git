//! `git-lfs-transfer`: Git LFS over the SSH connection itself
//! (card_f11136311b12).
//!
//! git-lfs 3.x, given an SSH remote, first runs
//! `ssh <host> git-lfs-transfer <path> <download|upload>` and speaks the
//! pkt-line protocol of `docs/proposals/ssh_adapter.md` over it. Only when that
//! command fails does it fall back to `git-lfs-authenticate` and HTTPS — three
//! more SSH sessions and three more authentications per push. Serving the
//! protocol here keeps a whole push or pull on one session.
//!
//! The SSH transport authenticates the key and authorizes the repository
//! before it calls [`serve`]: reading for `download`, writing for `upload`,
//! exactly as for `git-upload-pack` / `git-receive-pack`. Everything a batch
//! decides, and everything an uploaded object has to pass, is the HTTP batch
//! API's own: [`decide_object`] and [`crate::lfs::receive`].
//!
//! ## The conversation
//!
//! The server advertises `version=1` and `locking`, the client answers
//! `version 1`, and then sends requests until `quit`. A request is a command
//! line, `key=value` arguments, and for `batch` / `put-object` a delimiter
//! followed by object lines or the object's bytes; a flush ends it. Every
//! response starts with `status <http-code>`; an error carries its message
//! after a delimiter, which is where the client looks for it.
//!
//! An upload refused for one object (over the budget, too large) fails the
//! whole batch: the protocol has no per-object error, and the only other
//! answer for it, `noop`, tells the client the server already has the object —
//! which would end the push "successfully" without it. A download of an object
//! that is not here is `noop`, as the proposal says: the client reports it as
//! having no download action.

use std::path::Path;

use anyhow::{bail, Context as _, Result};
use sea_orm::DatabaseConnection;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, BufWriter};

use crate::blob_storage::BlobStorage;
use crate::lfs::locks::{self, CreateOutcome, LockFilter, LockView, UnlockOutcome};
use crate::lfs::receive::{
    accept_spooled, spool_path, LfsObjectTooLarge, ObjectSpool, UploadTarget,
};
use crate::lfs::service::{
    decide_object, is_valid_oid, lfs_root, read_object_source, LfsActionKind, LfsObjectSource,
    ObjectDecision, LFS_OBJECT_MAX_BYTES,
};

/// The largest pkt-line payload: a 4-byte length header of at most `ffff`.
const MAX_PAYLOAD: usize = 65_516;
/// Bytes per data packet sent back to a client.
const DATA_CHUNK: usize = 32 * 1024;
/// Objects one `batch` may name. git-lfs sends 100 at a time.
const MAX_BATCH_OBJECTS: usize = 1_000;
/// Arguments one request may carry; the protocol defines a handful.
const MAX_ARGUMENTS: usize = 64;

/// Who is on the other end, as far as LFS cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferActor {
    User {
        user_id: i64,
    },
    /// A deploy key moves objects but holds no lock: every lock has an owner.
    DeployKey,
}

/// What one `git-lfs-transfer` session serves.
pub struct TransferContext<'a> {
    pub db: &'a DatabaseConnection,
    pub storage: &'a dyn BlobStorage,
    pub repo_root: &'a Path,
    pub limits: &'a crate::storage_quota::StorageLimits,
    pub repo: &'a rg_db::entities::repository::Model,
    pub owner: &'a str,
    pub repo_name: &'a str,
    /// What the client asked for on the command line, and was authorized for.
    pub operation: LfsActionKind,
    pub actor: TransferActor,
    /// Whether the actor administers the repository, which `unlock` with
    /// `force` needs. Decided by the transport's gate with the rest of the
    /// session's access, not here.
    pub administers: bool,
}

/// One packet off the wire.
#[derive(Debug, PartialEq, Eq)]
enum Packet {
    Data(Vec<u8>),
    Flush,
    Delim,
}

/// How a request's argument section ended.
#[derive(Debug, PartialEq, Eq)]
enum Ending {
    Flush,
    Delim,
}

struct Request {
    command: String,
    args: Vec<String>,
    ending: Ending,
}

/// A refusal sent as `status <code>` with its message.
struct Refusal {
    code: u16,
    message: String,
}

impl Refusal {
    fn new(code: u16, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Serve one `git-lfs-transfer` session over `stream` until the client quits
/// or goes away.
///
/// `Err` is a broken conversation — malformed framing, a connection lost
/// mid-request, a server failure while bytes were already on the wire. A
/// refusal the client can be told about is an answer, not an error.
pub async fn serve<S>(stream: S, ctx: &TransferContext<'_>) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (reader, writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let mut out = Out {
        w: BufWriter::new(writer),
    };

    out.text("version=1").await?;
    out.text("locking").await?;
    out.flush().await?;

    let mut owner_name: Option<String> = None;
    loop {
        let Some(request) = read_request(&mut reader).await? else {
            // The client went away between requests: git-lfs quits politely,
            // but a closed pipe here loses nothing.
            return Ok(());
        };
        let command = request.command.clone();
        let (verb, operand) = command
            .split_once(' ')
            .map_or((command.as_str(), None), |(verb, rest)| (verb, Some(rest)));
        match verb {
            "version" => {
                drain_if_delimited(&mut reader, &request).await?;
                if operand == Some("1") {
                    out.status(200, &[]).await?;
                } else {
                    out.refuse(&Refusal::new(400, "only protocol version 1 is supported"))
                        .await?;
                }
            }
            "quit" => {
                drain_if_delimited(&mut reader, &request).await?;
                out.status(200, &[]).await?;
                return Ok(());
            }
            "batch" => batch(&mut reader, &mut out, ctx, &request).await?,
            "put-object" => put_object(&mut reader, &mut out, ctx, &request, operand).await?,
            "verify-object" => {
                drain_if_delimited(&mut reader, &request).await?;
                match verify_object(ctx, &request, operand).await? {
                    Ok(()) => out.status(200, &[]).await?,
                    Err(refusal) => out.refuse(&refusal).await?,
                }
            }
            "get-object" => {
                drain_if_delimited(&mut reader, &request).await?;
                get_object(&mut out, ctx, operand).await?;
            }
            "lock" => {
                drain_if_delimited(&mut reader, &request).await?;
                lock(&mut out, ctx, &request).await?;
            }
            "unlock" => {
                drain_if_delimited(&mut reader, &request).await?;
                unlock(&mut out, ctx, &request, operand).await?;
            }
            "list-lock" => {
                drain_if_delimited(&mut reader, &request).await?;
                list_lock(&mut out, ctx, &request, &mut owner_name).await?;
            }
            _ => {
                drain_if_delimited(&mut reader, &request).await?;
                out.refuse(&Refusal::new(400, format!("unknown command `{verb}`")))
                    .await?;
            }
        }
    }
}

// ── Requests ──────────────────────────────────────────────────────────────

async fn batch<R, W>(
    reader: &mut R,
    out: &mut Out<W>,
    ctx: &TransferContext<'_>,
    request: &Request,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // The object lines come after the delimiter; read them all first, so a
    // refusal below still leaves the stream at the next request.
    let mut lines = Vec::new();
    let mut overflow = false;
    if request.ending == Ending::Delim {
        loop {
            match read_packet(reader)
                .await?
                .context("connection closed inside a batch")?
            {
                Packet::Flush => break,
                Packet::Delim => bail!(protocol("a second delimiter inside a batch")),
                Packet::Data(data) if lines.len() < MAX_BATCH_OBJECTS => lines.push(text(data)?),
                Packet::Data(_) => overflow = true,
            }
        }
    }
    if overflow {
        return out
            .refuse(&Refusal::new(
                413,
                format!("a batch names at most {MAX_BATCH_OBJECTS} objects"),
            ))
            .await;
    }
    if let Some(algo) = argument(&request.args, "hash-algo") {
        if algo != "sha256" {
            return out
                .refuse(&Refusal::new(
                    409,
                    format!("unsupported hash algorithm `{algo}`; this server uses sha256"),
                ))
                .await;
        }
    }

    let lfs_root = lfs_root(ctx.repo_root, ctx.owner, ctx.repo_name);
    let mut answers = Vec::with_capacity(lines.len());
    for line in &lines {
        let mut fields = line.split(' ');
        let (Some(oid), Some(size)) = (fields.next(), fields.next()) else {
            return out
                .refuse(&Refusal::new(400, format!("malformed batch line `{line}`")))
                .await;
        };
        let Ok(size) = size.parse::<i64>() else {
            return out
                .refuse(&Refusal::new(
                    400,
                    format!("malformed object size in `{line}`"),
                ))
                .await;
        };
        let decision = decide_object(
            ctx.db,
            ctx.repo.id,
            ctx.storage,
            &lfs_root,
            ctx.owner,
            ctx.repo_name,
            ctx.operation,
            oid,
            size,
            ctx.limits,
        )
        .await?;
        let action = match (decision, ctx.operation) {
            (ObjectDecision::Upload, _) => "upload",
            (ObjectDecision::Download, _) => "download",
            (ObjectDecision::Present, _) => "noop",
            (ObjectDecision::Refused(_), LfsActionKind::Download) => "noop",
            (ObjectDecision::Refused(error), LfsActionKind::Upload) => {
                let code = u16::try_from(error.code).unwrap_or(500);
                return out
                    .refuse(&Refusal::new(code, format!("{oid}: {}", error.message)))
                    .await;
            }
        };
        answers.push(format!("{oid} {size} {action}"));
    }

    out.text("status 200").await?;
    out.text("hash-algo=sha256").await?;
    out.delim().await?;
    for answer in answers {
        out.text(&answer).await?;
    }
    out.flush().await
}

async fn put_object<R, W>(
    reader: &mut R,
    out: &mut Out<W>,
    ctx: &TransferContext<'_>,
    request: &Request,
    oid: Option<&str>,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if request.ending != Ending::Delim {
        // No data section at all: the request is over already.
        return out
            .refuse(&Refusal::new(
                400,
                "put-object carries the object after a delimiter",
            ))
            .await;
    }
    let refused = put_object_refusal(ctx, request, oid);
    let (oid, size) = match refused {
        Ok(target) => target,
        Err(refusal) => {
            drain_data(reader).await?;
            return out.refuse(&refusal).await;
        }
    };

    let lfs_root = lfs_root(ctx.repo_root, ctx.owner, ctx.repo_name);
    let path = spool_path(&lfs_root, oid);
    if let Some(parent) = path.parent() {
        if let Err(error) = tokio::fs::create_dir_all(parent).await {
            let error = crate::platform::fs::path_error(
                "LFS object directory",
                parent,
                &error,
                crate::platform::fs::LFS_STORAGE_HINT,
            );
            tracing::error!(error = %format!("{error:#}"), "git-lfs-transfer could not stage an object");
            drain_data(reader).await?;
            return out
                .refuse(&Refusal::new(500, "the server could not store the object"))
                .await;
        }
    }
    // Never more than the client said it would send: the size is in the
    // request, so the first byte past it is already wrong.
    let ceiling = usize::try_from(size)
        .unwrap_or(usize::MAX)
        .min(LFS_OBJECT_MAX_BYTES);
    let mut spool = ObjectSpool::create(path, ceiling).await?;
    let mut refusal = None;
    loop {
        let packet = match read_packet(reader).await {
            Ok(Some(packet)) => packet,
            // Connection lost mid-object: nothing of it may become an object.
            Ok(None) | Err(_) => {
                spool.discard().await;
                bail!("connection closed inside put-object {oid}");
            }
        };
        match packet {
            Packet::Flush => break,
            Packet::Delim => {
                spool.discard().await;
                bail!(protocol("a delimiter inside put-object data"));
            }
            Packet::Data(_) if refusal.is_some() => {}
            Packet::Data(data) => {
                if let Err(error) = spool.write(&data).await {
                    refusal = Some(if error.downcast_ref::<LfsObjectTooLarge>().is_some() {
                        Refusal::new(
                            400,
                            format!("object {oid} is longer than its declared {size} bytes"),
                        )
                    } else {
                        tracing::error!(error = %format!("{error:#}"), "git-lfs-transfer could not spool an object");
                        Refusal::new(500, "the server could not store the object")
                    });
                }
            }
        }
    }
    if let Some(refusal) = refusal {
        spool.discard().await;
        return out.refuse(&refusal).await;
    }
    if i64::try_from(spool.written()).ok() != Some(size) {
        let written = spool.written();
        spool.discard().await;
        return out
            .refuse(&Refusal::new(
                400,
                format!("object {oid} is {written} bytes, not its declared {size}"),
            ))
            .await;
    }
    let spooled = spool.finish().await?;
    match accept_spooled(
        ctx.db,
        ctx.storage,
        ctx.limits,
        UploadTarget {
            repo_id: ctx.repo.id,
            owner: ctx.owner,
            repo: ctx.repo_name,
            oid,
        },
        spooled,
    )
    .await
    {
        Ok(()) => out.status(200, &[]).await,
        Err(error) => out.refuse(&refusal_for(&error)).await,
    }
}

/// The object a `put-object` names, or why it cannot be accepted.
fn put_object_refusal<'r>(
    ctx: &TransferContext<'_>,
    request: &'r Request,
    oid: Option<&'r str>,
) -> std::result::Result<(&'r str, i64), Refusal> {
    if ctx.operation != LfsActionKind::Upload {
        return Err(Refusal::new(
            403,
            "this session was opened for download; uploading needs `git-lfs-transfer … upload`",
        ));
    }
    let oid = oid
        .filter(|oid| is_valid_oid(oid))
        .ok_or_else(|| Refusal::new(400, "invalid LFS object identifier"))?;
    let size = argument(&request.args, "size")
        .and_then(|size| size.parse::<i64>().ok())
        .filter(|size| *size >= 0)
        .ok_or_else(|| Refusal::new(400, "put-object needs a `size` argument"))?;
    if size > LFS_OBJECT_MAX_BYTES as i64 {
        return Err(Refusal::new(
            413,
            format!("LFS object exceeds the {LFS_OBJECT_MAX_BYTES}-byte upload limit"),
        ));
    }
    Ok((oid, size))
}

async fn verify_object(
    ctx: &TransferContext<'_>,
    request: &Request,
    oid: Option<&str>,
) -> Result<std::result::Result<(), Refusal>> {
    if ctx.operation != LfsActionKind::Upload {
        return Ok(Err(Refusal::new(
            403,
            "verify-object belongs to an upload session",
        )));
    }
    let Some(oid) = oid.filter(|oid| is_valid_oid(oid)) else {
        return Ok(Err(Refusal::new(400, "invalid LFS object identifier")));
    };
    let Some(size) = argument(&request.args, "size").and_then(|size| size.parse::<i64>().ok())
    else {
        return Ok(Err(Refusal::new(
            400,
            "verify-object needs a `size` argument",
        )));
    };
    let object = rg_db::ops::lfs_object_ops::find_by_repo_and_oid(ctx.db, ctx.repo.id, oid).await?;
    Ok(match object {
        Some(object) if object.uploaded && object.size == size => Ok(()),
        Some(object) if object.uploaded => Err(Refusal::new(
            409,
            format!(
                "object {oid} is stored with {} bytes, not {size}",
                object.size
            ),
        )),
        _ => Err(Refusal::new(
            404,
            format!("object {oid} is not on the server"),
        )),
    })
}

async fn get_object<W>(out: &mut Out<W>, ctx: &TransferContext<'_>, oid: Option<&str>) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let Some(oid) = oid.filter(|oid| is_valid_oid(oid)) else {
        return out
            .refuse(&Refusal::new(400, "invalid LFS object identifier"))
            .await;
    };
    let object = rg_db::ops::lfs_object_ops::find_by_repo_and_oid(ctx.db, ctx.repo.id, oid).await?;
    let Some(object) = object.filter(|object| object.uploaded) else {
        return out
            .refuse(&Refusal::new(
                404,
                format!("object {oid} is not on the server"),
            ))
            .await;
    };
    let lfs_root = lfs_root(ctx.repo_root, ctx.owner, ctx.repo_name);
    let source = match read_object_source(ctx.storage, &lfs_root, ctx.owner, ctx.repo_name, oid)
        .await
    {
        Ok(source) => source,
        Err(error) => {
            let refusal = refusal_for(&error);
            if refusal.code >= 500 {
                tracing::error!(error = %format!("{error:#}"), %oid, "git-lfs-transfer could not read an object");
            }
            return out.refuse(&refusal).await;
        }
    };
    // Opened before the status goes out: once `status 200` is on the wire, a
    // failure has no way left to reach the client but a broken connection.
    let body = match ObjectBody::open(source).await {
        Ok(body) => body,
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), %oid, "git-lfs-transfer could not open an object");
            return out
                .refuse(&Refusal::new(500, "the server could not read the object"))
                .await;
        }
    };
    out.text("status 200").await?;
    out.text(&format!("size={}", object.size)).await?;
    out.delim().await?;
    body.send(out).await?;
    out.flush().await
}

async fn lock<W>(out: &mut Out<W>, ctx: &TransferContext<'_>, request: &Request) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let user_id = match lock_holder(ctx) {
        Ok(user_id) => user_id,
        Err(refusal) => return out.refuse(&refusal).await,
    };
    let Some(path) = argument(&request.args, "path") else {
        return out
            .refuse(&Refusal::new(400, "lock needs a `path` argument"))
            .await;
    };
    let ref_name = argument(&request.args, "refname");
    match locks::create_lock(ctx.db, ctx.repo.id, user_id, path, ref_name).await {
        Ok(CreateOutcome::Created(lock)) => out.status(201, &lock_args(&lock)).await,
        Ok(CreateOutcome::AlreadyLocked(lock)) => {
            let message = format!("`{}` is already locked by {}", lock.path, lock.owner.name);
            out.refuse_with(409, &lock_args(&lock), &message).await
        }
        Err(error) => out.refuse(&refusal_for(&error)).await,
    }
}

async fn unlock<W>(
    out: &mut Out<W>,
    ctx: &TransferContext<'_>,
    request: &Request,
    id: Option<&str>,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let user_id = match lock_holder(ctx) {
        Ok(user_id) => user_id,
        Err(refusal) => return out.refuse(&refusal).await,
    };
    let force = request
        .args
        .iter()
        .any(|arg| arg == "force" || arg == "force=true");
    // `force` is an administrator's word whether or not the lock turns out to
    // be someone else's — the same rule as `POST …/locks/{id}/unlock`.
    if force && !ctx.administers {
        return out
            .refuse(&Refusal::new(
                403,
                "only a repository administrator can force an unlock",
            ))
            .await;
    }
    let Some(id) = id.and_then(|id| locks::parse_lock_id(id, "id").ok()) else {
        return out.refuse(&Refusal::new(404, "lock not found")).await;
    };
    match locks::unlock(ctx.db, ctx.repo.id, user_id, id, force).await? {
        UnlockOutcome::Unlocked(lock) => out.status(200, &lock_args(&lock)).await,
        UnlockOutcome::NotFound => out.refuse(&Refusal::new(404, "lock not found")).await,
        UnlockOutcome::OwnedByAnother(lock) => {
            let message = format!(
                "`{}` is locked by {}; only a repository administrator can unlock it, with --force",
                lock.path, lock.owner.name
            );
            out.refuse_with(403, &lock_args(&lock), &message).await
        }
    }
}

async fn list_lock<W>(
    out: &mut Out<W>,
    ctx: &TransferContext<'_>,
    request: &Request,
    owner_name: &mut Option<String>,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let limit = match argument(&request.args, "limit").map(str::parse::<u64>) {
        None => None,
        Some(Ok(limit)) => Some(limit),
        Some(Err(_)) => {
            return out
                .refuse(&Refusal::new(400, "`limit` is not a number"))
                .await
        }
    };
    let filter = LockFilter {
        path: argument(&request.args, "path"),
        id: argument(&request.args, "id"),
        cursor: argument(&request.args, "cursor"),
        limit,
    };
    let page = match locks::list_locks(ctx.db, ctx.repo.id, filter).await {
        Ok(page) => page,
        Err(error) => return out.refuse(&refusal_for(&error)).await,
    };
    // Whose locks are "ours" — what `git lfs locks --verify` and the pre-push
    // check need. A deploy key holds none, so every lock is someone else's.
    if owner_name.is_none() {
        if let TransferActor::User { user_id } = ctx.actor {
            *owner_name = rg_db::ops::user_ops::find_by_id(ctx.db, user_id)
                .await?
                .map(|user| user.username);
        }
    }

    out.text("status 200").await?;
    if let Some(cursor) = &page.next_cursor {
        out.text(&format!("next-cursor={cursor}")).await?;
    }
    out.delim().await?;
    for lock in &page.locks {
        let id = &lock.id;
        out.text(&format!("lock {id}")).await?;
        out.text(&format!("path {id} {}", lock.path)).await?;
        out.text(&format!("locked-at {id} {}", rfc3339(lock)))
            .await?;
        out.text(&format!("ownername {id} {}", lock.owner.name))
            .await?;
        let who = if owner_name.as_deref() == Some(lock.owner.name.as_str()) {
            "ours"
        } else {
            "theirs"
        };
        out.text(&format!("owner {id} {who}")).await?;
    }
    out.flush().await
}

/// The account a lock or unlock is for, or why there is none.
fn lock_holder(ctx: &TransferContext<'_>) -> std::result::Result<i64, Refusal> {
    if ctx.operation != LfsActionKind::Upload {
        return Err(Refusal::new(
            403,
            "locking needs write access; open the session with `git-lfs-transfer … upload`",
        ));
    }
    match ctx.actor {
        TransferActor::User { user_id } => Ok(user_id),
        TransferActor::DeployKey => Err(Refusal::new(
            403,
            "a deploy key cannot hold an LFS lock; lock files with a user account",
        )),
    }
}

fn lock_args(lock: &LockView) -> Vec<String> {
    vec![
        format!("id={}", lock.id),
        format!("path={}", lock.path),
        format!("locked-at={}", rfc3339(lock)),
        format!("ownername={}", lock.owner.name),
    ]
}

fn rfc3339(lock: &LockView) -> String {
    lock.locked_at
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The status a core error is answered with, the way the HTTP API answers it.
fn refusal_for(error: &anyhow::Error) -> Refusal {
    use crate::error::{Conflict, Forbidden, InvalidRequest, NotFound, PayloadTooLarge};
    if let Some(invalid) = error.downcast_ref::<InvalidRequest>() {
        return Refusal::new(400, invalid.message.clone());
    }
    if let Some(too_large) = error.downcast_ref::<PayloadTooLarge>() {
        return Refusal::new(413, too_large.message.clone());
    }
    if let Some(forbidden) = error.downcast_ref::<Forbidden>() {
        return Refusal::new(403, forbidden.message.clone());
    }
    if let Some(conflict) = error.downcast_ref::<Conflict>() {
        return Refusal::new(409, conflict.message.clone());
    }
    if error.downcast_ref::<NotFound>().is_some()
        || matches!(
            error.downcast_ref::<crate::blob_storage::BlobStorageError>(),
            Some(crate::blob_storage::BlobStorageError::NotFound(_))
        )
    {
        return Refusal::new(404, "object not found");
    }
    tracing::error!(error = %format!("{error:#}"), "git-lfs-transfer request failed");
    Refusal::new(500, "internal server error")
}

// ── Object bytes ──────────────────────────────────────────────────────────

/// A stored object opened for reading, decompressed as it is sent.
enum ObjectBody {
    File(tokio::fs::File),
    Compressed(std::fs::File),
    Bytes(Vec<u8>),
}

impl ObjectBody {
    async fn open(source: LfsObjectSource) -> Result<Self> {
        Ok(match source {
            LfsObjectSource::Local {
                path,
                compressed: false,
            } => Self::File(
                tokio::fs::File::open(&path)
                    .await
                    .with_context(|| format!("open LFS object {}", path.display()))?,
            ),
            LfsObjectSource::Local {
                path,
                compressed: true,
            } => Self::Compressed(
                tokio::fs::File::open(&path)
                    .await
                    .with_context(|| format!("open LFS object {}", path.display()))?
                    .into_std()
                    .await,
            ),
            LfsObjectSource::Bytes {
                data,
                compressed: false,
            } => Self::Bytes(data),
            LfsObjectSource::Bytes {
                data,
                compressed: true,
            } => Self::Bytes(
                tokio::task::spawn_blocking(move || zstd::decode_all(data.as_slice()))
                    .await
                    .context("decompress LFS object")??,
            ),
        })
    }

    async fn send<W: AsyncWrite + Unpin>(self, out: &mut Out<W>) -> Result<()> {
        match self {
            Self::File(mut file) => {
                let mut buf = vec![0u8; DATA_CHUNK];
                loop {
                    let n = file.read(&mut buf).await.context("read LFS object")?;
                    if n == 0 {
                        return Ok(());
                    }
                    out.data(&buf[..n]).await?;
                }
            }
            Self::Bytes(data) => {
                for chunk in data.chunks(DATA_CHUNK) {
                    out.data(chunk).await?;
                }
                Ok(())
            }
            Self::Compressed(file) => {
                // zstd is synchronous: decode on the blocking pool and hand
                // the chunks over. A client that stops reading stops the
                // decoder at the next send.
                let (tx, mut rx) = tokio::sync::mpsc::channel::<std::io::Result<Vec<u8>>>(8);
                let decoder = tokio::task::spawn_blocking(move || {
                    use std::io::Read;
                    let mut decoder = match zstd::stream::Decoder::new(file) {
                        Ok(decoder) => decoder,
                        Err(error) => {
                            if tx.blocking_send(Err(error)).is_err() {
                                // The session ended before it could be told.
                            }
                            return;
                        }
                    };
                    let mut buf = vec![0u8; DATA_CHUNK];
                    loop {
                        match decoder.read(&mut buf) {
                            Ok(0) => return,
                            Ok(n) => {
                                if tx.blocking_send(Ok(buf[..n].to_vec())).is_err() {
                                    return;
                                }
                            }
                            Err(error) => {
                                if tx.blocking_send(Err(error)).is_err() {
                                    // The session ended before it could be told.
                                }
                                return;
                            }
                        }
                    }
                });
                while let Some(chunk) = rx.recv().await {
                    out.data(&chunk.context("decompress LFS object")?).await?;
                }
                decoder.await.context("LFS object decoder")?;
                Ok(())
            }
        }
    }
}

// ── Framing ───────────────────────────────────────────────────────────────

struct Out<W> {
    w: BufWriter<W>,
}

impl<W: AsyncWrite + Unpin> Out<W> {
    async fn data(&mut self, payload: &[u8]) -> Result<()> {
        debug_assert!(payload.len() <= MAX_PAYLOAD);
        self.w
            .write_all(format!("{:04x}", payload.len() + 4).as_bytes())
            .await?;
        self.w.write_all(payload).await?;
        Ok(())
    }

    async fn text(&mut self, line: &str) -> Result<()> {
        let mut payload = Vec::with_capacity(line.len() + 1);
        payload.extend_from_slice(line.as_bytes());
        payload.push(b'\n');
        if payload.len() > MAX_PAYLOAD {
            bail!(
                "a protocol line of {} bytes does not fit a packet",
                payload.len()
            );
        }
        self.data(&payload).await
    }

    async fn delim(&mut self) -> Result<()> {
        self.w.write_all(b"0001").await?;
        Ok(())
    }

    /// End a response and put it on the wire.
    async fn flush(&mut self) -> Result<()> {
        self.w.write_all(b"0000").await?;
        self.w.flush().await?;
        Ok(())
    }

    /// `status <code>`, its arguments, and nothing else.
    async fn status(&mut self, code: u16, args: &[String]) -> Result<()> {
        self.text(&format!("status {code}")).await?;
        for arg in args {
            self.text(arg).await?;
        }
        self.flush().await
    }

    async fn refuse(&mut self, refusal: &Refusal) -> Result<()> {
        self.refuse_with(refusal.code, &[], &refusal.message).await
    }

    /// An error status: its arguments, then the message after a delimiter,
    /// which is where every reader of the protocol looks for it.
    async fn refuse_with(&mut self, code: u16, args: &[String], message: &str) -> Result<()> {
        self.text(&format!("status {code}")).await?;
        for arg in args {
            self.text(arg).await?;
        }
        self.delim().await?;
        // One line: the client shows the first.
        self.text(&message.replace('\n', " ")).await?;
        self.flush().await
    }
}

/// Read one packet; `None` is a connection closed before a header began.
async fn read_packet<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Option<Packet>> {
    let mut header = [0u8; 4];
    let mut filled = 0;
    while filled < header.len() {
        let n = reader.read(&mut header[filled..]).await?;
        if n == 0 {
            if filled == 0 {
                return Ok(None);
            }
            bail!("connection closed inside a pkt-line header");
        }
        filled += n;
    }
    let len = std::str::from_utf8(&header)
        .ok()
        .and_then(|header| usize::from_str_radix(header, 16).ok())
        .ok_or_else(|| protocol(format!("invalid pkt-line header {header:?}")))?;
    match len {
        0 => return Ok(Some(Packet::Flush)),
        1 => return Ok(Some(Packet::Delim)),
        2 | 3 => bail!(protocol(format!("unexpected pkt-line length {len}"))),
        _ => {}
    }
    let mut payload = vec![0u8; len - 4];
    reader
        .read_exact(&mut payload)
        .await
        .context("connection closed inside a pkt-line")?;
    Ok(Some(Packet::Data(payload)))
}

/// Read a command and its arguments, up to the delimiter or the flush.
async fn read_request<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Option<Request>> {
    let command = match read_packet(reader).await? {
        None => return Ok(None),
        Some(Packet::Data(data)) => text(data)?,
        Some(other) => bail!(protocol(format!("expected a command, got {other:?}"))),
    };
    let mut args = Vec::new();
    loop {
        match read_packet(reader)
            .await?
            .with_context(|| format!("connection closed inside `{command}`"))?
        {
            Packet::Flush => {
                return Ok(Some(Request {
                    command,
                    args,
                    ending: Ending::Flush,
                }))
            }
            Packet::Delim => {
                return Ok(Some(Request {
                    command,
                    args,
                    ending: Ending::Delim,
                }))
            }
            Packet::Data(_) if args.len() >= MAX_ARGUMENTS => {
                bail!(protocol(format!(
                    "`{command}` carries more than {MAX_ARGUMENTS} arguments"
                )))
            }
            Packet::Data(data) => args.push(text(data)?),
        }
    }
}

/// Skip a data section the command does not take, so the stream stays at the
/// next request.
async fn drain_if_delimited<R: AsyncRead + Unpin>(reader: &mut R, request: &Request) -> Result<()> {
    if request.ending == Ending::Delim {
        drain_data(reader).await?;
    }
    Ok(())
}

async fn drain_data<R: AsyncRead + Unpin>(reader: &mut R) -> Result<()> {
    loop {
        match read_packet(reader)
            .await?
            .context("connection closed inside a request")?
        {
            Packet::Flush => return Ok(()),
            Packet::Delim => bail!(protocol("a second delimiter inside a request")),
            Packet::Data(_) => {}
        }
    }
}

fn text(data: Vec<u8>) -> Result<String> {
    let mut line = String::from_utf8(data).map_err(|_| protocol("a protocol line is not UTF-8"))?;
    if line.ends_with('\n') {
        line.pop();
    }
    Ok(line)
}

fn argument<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    args.iter()
        .find_map(|arg| arg.strip_prefix(key)?.strip_prefix('='))
}

/// A malformed conversation: the client's fault, as git's own pkt-line reader
/// classifies it.
fn protocol(message: impl Into<String>) -> anyhow::Error {
    rg_git::protocol::ClientRefusal::new(format!("protocol error: {}", message.into())).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob_storage::LocalBlobStorage;
    use sea_orm::{ActiveValue::Set, ConnectOptions, Database, NotSet};
    use tokio::io::{DuplexStream, ReadHalf, WriteHalf};

    struct Fixture {
        db: DatabaseConnection,
        dir: tempfile::TempDir,
        storage: LocalBlobStorage,
        repo: rg_db::entities::repository::Model,
        user_id: i64,
        limits: crate::storage_quota::StorageLimits,
    }

    async fn fixture() -> Fixture {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = Database::connect(options).await.unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user =
            rg_db::ops::user_ops::create_user(&db, "alice", "alice@example.test", "", "Alice")
                .await
                .unwrap();
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set("assets".to_string()),
                description: Set(None),
                is_private: Set(true),
                default_branch: Set("main".to_string()),
                fork_id: Set(None),
                stars_count: Set(0),
                forks_count: Set(0),
                org_id: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                deleted_at: Set(None),
                origin_repo_id: Set(None),
            },
        )
        .await
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let storage = LocalBlobStorage::new(dir.path().to_path_buf());
        Fixture {
            db,
            storage,
            repo,
            user_id: user.id,
            limits: Default::default(),
            dir,
        }
    }

    impl Fixture {
        fn context(&self, operation: LfsActionKind, actor: TransferActor) -> TransferContext<'_> {
            TransferContext {
                db: &self.db,
                storage: &self.storage,
                repo_root: self.dir.path(),
                limits: &self.limits,
                repo: &self.repo,
                owner: "alice",
                repo_name: "assets",
                operation,
                actor,
                administers: false,
            }
        }

        fn user(&self) -> TransferActor {
            TransferActor::User {
                user_id: self.user_id,
            }
        }

        /// Every spool left under the repository's LFS root.
        fn spools(&self) -> Vec<String> {
            let root = lfs_root(self.dir.path(), "alice", "assets");
            std::fs::read_dir(root)
                .map(|entries| {
                    entries
                        .filter_map(|entry| entry.ok())
                        .map(|entry| entry.file_name().to_string_lossy().into_owned())
                        .filter(|name| name.starts_with(".tmp_"))
                        .collect()
                })
                .unwrap_or_default()
        }
    }

    fn oid_of(payload: &[u8]) -> String {
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(payload))
    }

    /// The client half: writes requests, reads responses.
    struct Client {
        r: BufReader<ReadHalf<DuplexStream>>,
        w: WriteHalf<DuplexStream>,
    }

    /// One response: the lines before the delimiter (status first), and the
    /// packets after it, if there was one.
    #[derive(Debug)]
    struct Response {
        head: Vec<String>,
        body: Option<Vec<Vec<u8>>>,
    }

    impl Response {
        fn status(&self) -> &str {
            &self.head[0]
        }

        fn body_text(&self) -> Vec<String> {
            self.body
                .iter()
                .flatten()
                .map(|line| {
                    String::from_utf8(line.clone())
                        .unwrap()
                        .trim_end()
                        .to_string()
                })
                .collect()
        }
    }

    fn frame(payload: &[u8]) -> Vec<u8> {
        let mut out = format!("{:04x}", payload.len() + 4).into_bytes();
        out.extend_from_slice(payload);
        out
    }

    impl Client {
        async fn raw(&mut self, bytes: &[u8]) {
            self.w.write_all(bytes).await.unwrap();
        }

        async fn line(&mut self, line: &str) {
            self.raw(&frame(format!("{line}\n").as_bytes())).await;
        }

        /// A command, its arguments and a flush.
        async fn send(&mut self, command: &str, args: &[&str]) -> Response {
            self.line(command).await;
            for arg in args {
                self.line(arg).await;
            }
            self.raw(b"0000").await;
            self.read().await
        }

        /// A command, its arguments, a delimiter, `body` as packets, a flush.
        async fn send_with(&mut self, command: &str, args: &[&str], body: &[&[u8]]) -> Response {
            self.line(command).await;
            for arg in args {
                self.line(arg).await;
            }
            self.raw(b"0001").await;
            for part in body {
                self.raw(&frame(part)).await;
            }
            self.raw(b"0000").await;
            self.read().await
        }

        async fn read(&mut self) -> Response {
            let mut head = Vec::new();
            let mut body: Option<Vec<Vec<u8>>> = None;
            loop {
                match read_packet(&mut self.r)
                    .await
                    .unwrap()
                    .expect("server hung up")
                {
                    Packet::Flush => return Response { head, body },
                    Packet::Delim => body = Some(Vec::new()),
                    Packet::Data(data) => match body.as_mut() {
                        Some(body) => body.push(data),
                        None => head.push(text(data).unwrap()),
                    },
                }
            }
        }

        async fn handshake(&mut self) {
            let advertised = self.read().await;
            assert!(
                advertised.head.contains(&"version=1".to_string()),
                "{advertised:?}"
            );
            assert!(
                advertised.head.contains(&"locking".to_string()),
                "{advertised:?}"
            );
            assert_eq!(self.send("version 1", &[]).await.status(), "status 200");
        }

        async fn quit(&mut self) {
            assert_eq!(self.send("quit", &[]).await.status(), "status 200");
        }
    }

    /// Run `script` as the client of one session; the server's own result too.
    async fn session<F, Fut>(ctx: TransferContext<'_>, script: F) -> Result<()>
    where
        F: FnOnce(Client) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let (client, server) = tokio::io::duplex(256 * 1024);
        let (r, w) = tokio::io::split(client);
        let client = Client {
            r: BufReader::new(r),
            w,
        };
        let (served, ()) = tokio::join!(serve(server, &ctx), script(client));
        served
    }

    #[tokio::test]
    async fn an_object_goes_up_and_comes_back_each_on_one_session() {
        let f = fixture().await;
        let payload = b"large file content, in two packets".to_vec();
        let oid = oid_of(&payload);
        let size = payload.len().to_string();
        let (oid, size, payload) = (&oid, &size, &payload);

        session(
            f.context(LfsActionKind::Upload, f.user()),
            |mut c| async move {
                c.handshake().await;
                let batch = c
                    .send_with(
                        "batch",
                        &["transfer=ssh", "hash-algo=sha256"],
                        &[format!("{oid} {size}").as_bytes()],
                    )
                    .await;
                assert_eq!(batch.status(), "status 200", "{batch:?}");
                assert_eq!(batch.body_text(), vec![format!("{oid} {size} upload")]);

                let (first, second) = payload.split_at(10);
                let put = c
                    .send_with(
                        &format!("put-object {oid}"),
                        &[&format!("size={size}")],
                        &[first, second],
                    )
                    .await;
                assert_eq!(put.status(), "status 200", "{put:?}");
                let verify = c
                    .send(&format!("verify-object {oid}"), &[&format!("size={size}")])
                    .await;
                assert_eq!(verify.status(), "status 200", "{verify:?}");

                // Asked again, the object is already here.
                let again = c
                    .send_with("batch", &[], &[format!("{oid} {size}").as_bytes()])
                    .await;
                assert_eq!(again.body_text(), vec![format!("{oid} {size} noop")]);
                c.quit().await;
            },
        )
        .await
        .unwrap();
        assert!(f.spools().is_empty(), "spools left: {:?}", f.spools());

        session(
            f.context(LfsActionKind::Download, f.user()),
            |mut c| async move {
                c.handshake().await;
                let missing = oid_of(b"never uploaded");
                let batch = c
                    .send_with(
                        "batch",
                        &[],
                        &[
                            format!("{oid} {size}").as_bytes(),
                            format!("{missing} 3").as_bytes(),
                        ],
                    )
                    .await;
                assert_eq!(
                    batch.body_text(),
                    vec![
                        format!("{oid} {size} download"),
                        format!("{missing} 3 noop")
                    ]
                );
                let got = c
                    .send(&format!("get-object {oid}"), &[&format!("size={size}")])
                    .await;
                assert_eq!(
                    got.head,
                    vec!["status 200".to_string(), format!("size={size}")]
                );
                assert_eq!(&got.body.unwrap().concat(), payload);
                let absent = c.send(&format!("get-object {missing}"), &[]).await;
                assert_eq!(absent.status(), "status 404");
                assert!(absent.body_text()[0].contains(&missing), "{absent:?}");
                c.quit().await;
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn an_object_longer_than_declared_or_not_matching_its_oid_is_refused_and_stored_nowhere()
    {
        let f = fixture().await;
        let payload = b"abcde";
        let oid = oid_of(payload);
        let oid = &oid;

        session(
            f.context(LfsActionKind::Upload, f.user()),
            |mut c| async move {
                c.handshake().await;
                let long = c
                    .send_with(&format!("put-object {oid}"), &["size=3"], &[payload])
                    .await;
                assert_eq!(long.status(), "status 400", "{long:?}");
                assert!(
                    long.body_text()[0].contains("longer than its declared 3 bytes"),
                    "{long:?}"
                );

                let short = c
                    .send_with(&format!("put-object {oid}"), &["size=9"], &[payload])
                    .await;
                assert_eq!(short.status(), "status 400", "{short:?}");

                let other = oid_of(b"something else");
                let forged = c
                    .send_with(&format!("put-object {other}"), &["size=5"], &[payload])
                    .await;
                assert_eq!(forged.status(), "status 400", "{forged:?}");
                assert!(
                    forged.body_text()[0].contains("digest does not match"),
                    "{forged:?}"
                );

                let invalid = c
                    .send_with("put-object not-an-oid", &["size=5"], &[payload])
                    .await;
                assert_eq!(invalid.status(), "status 400", "{invalid:?}");
                let batch = c.send_with("batch", &[], &[b"not-an-oid 5"]).await;
                assert_eq!(batch.status(), "status 422", "{batch:?}");

                let verify = c.send(&format!("verify-object {oid}"), &["size=5"]).await;
                assert_eq!(verify.status(), "status 404", "{verify:?}");
                c.quit().await;
            },
        )
        .await
        .unwrap();

        let row = rg_db::ops::lfs_object_ops::find_by_repo_and_oid(&f.db, f.repo.id, oid)
            .await
            .unwrap();
        assert!(
            row.is_none_or(|row| !row.uploaded),
            "a refused object was stored"
        );
        assert!(f.spools().is_empty(), "spools left: {:?}", f.spools());
    }

    #[tokio::test]
    async fn a_download_session_cannot_upload_or_lock() {
        let f = fixture().await;
        let payload = b"abc";
        let oid = oid_of(payload);
        session(
            f.context(LfsActionKind::Download, f.user()),
            |mut c| async move {
                c.handshake().await;
                let put = c
                    .send_with(&format!("put-object {oid}"), &["size=3"], &[payload])
                    .await;
                assert_eq!(put.status(), "status 403", "{put:?}");
                let lock = c.send("lock", &["path=a.psd"]).await;
                assert_eq!(lock.status(), "status 403", "{lock:?}");
                // The listing is a read, and stays open to it.
                assert_eq!(c.send("list-lock", &[]).await.status(), "status 200");
                c.quit().await;
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_connection_lost_inside_put_object_leaves_no_object_behind() {
        let f = fixture().await;
        let payload = vec![7u8; 4096];
        let oid = oid_of(&payload);
        let served = session(
            f.context(LfsActionKind::Upload, f.user()),
            |mut c| async move {
                c.handshake().await;
                c.line(&format!("put-object {oid}")).await;
                c.line(&format!("size={}", payload.len())).await;
                c.raw(b"0001").await;
                c.raw(&frame(&payload[..1024])).await;
                // The client goes away: both halves of its pipe close.
                drop(c);
            },
        )
        .await;
        assert!(
            served.is_err(),
            "a cut put-object must not read as a clean end"
        );
        let row = rg_db::ops::lfs_object_ops::find_by_repo_and_oid(
            &f.db,
            f.repo.id,
            &oid_of(&vec![7u8; 4096]),
        )
        .await
        .unwrap();
        assert!(
            row.is_none_or(|row| !row.uploaded),
            "half an object was stored"
        );
        assert!(f.spools().is_empty(), "spools left: {:?}", f.spools());
    }

    #[tokio::test]
    async fn locks_are_taken_listed_and_released_and_a_deploy_key_holds_none() {
        let f = fixture().await;
        session(
            f.context(LfsActionKind::Upload, f.user()),
            |mut c| async move {
                c.handshake().await;
                let lock = c
                    .send("lock", &["path=art/hero.psd", "refname=refs/heads/main"])
                    .await;
                assert_eq!(lock.status(), "status 201", "{lock:?}");
                let id = lock
                    .head
                    .iter()
                    .find_map(|arg| arg.strip_prefix("id="))
                    .unwrap()
                    .to_string();
                for key in ["path=art/hero.psd", "ownername=alice"] {
                    assert!(lock.head.contains(&key.to_string()), "{lock:?}");
                }
                assert!(
                    lock.head.iter().any(|arg| arg.starts_with("locked-at=")),
                    "{lock:?}"
                );

                let again = c.send("lock", &["path=art/hero.psd"]).await;
                assert_eq!(again.status(), "status 409", "{again:?}");
                assert!(
                    again.head.contains(&format!("id={id}")),
                    "the 409 names the holding lock"
                );

                let listed = c.send("list-lock", &["refname=refs/heads/main"]).await;
                assert_eq!(listed.status(), "status 200");
                assert_eq!(
                    listed.body_text(),
                    vec![
                        format!("lock {id}"),
                        format!("path {id} art/hero.psd"),
                        listed.body_text()[2].clone(),
                        format!("ownername {id} alice"),
                        format!("owner {id} ours"),
                    ]
                );
                assert!(listed.body_text()[2].starts_with(&format!("locked-at {id} ")));

                let unlocked = c.send(&format!("unlock {id}"), &[]).await;
                assert_eq!(unlocked.status(), "status 200", "{unlocked:?}");
                assert_eq!(
                    c.send(&format!("unlock {id}"), &[]).await.status(),
                    "status 404"
                );
                c.quit().await;
            },
        )
        .await
        .unwrap();

        session(
            f.context(LfsActionKind::Upload, TransferActor::DeployKey),
            |mut c| async move {
                c.handshake().await;
                let lock = c.send("lock", &["path=b.psd"]).await;
                assert_eq!(lock.status(), "status 403", "{lock:?}");
                assert!(lock.body_text()[0].contains("deploy key"), "{lock:?}");
                c.quit().await;
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn someone_elses_lock_goes_only_with_force_and_only_for_an_administrator() {
        let f = fixture().await;
        let bob = rg_db::ops::user_ops::create_user(&f.db, "bob", "bob@example.test", "", "Bob")
            .await
            .unwrap();
        let CreateOutcome::Created(lock) =
            locks::create_lock(&f.db, f.repo.id, bob.id, "maps/castle.level", None)
                .await
                .unwrap()
        else {
            panic!("bob's lock was not created");
        };
        let id = lock.id.clone();

        session(
            f.context(LfsActionKind::Upload, f.user()),
            |mut c| async move {
                c.handshake().await;
                let mine = c.send(&format!("unlock {id}"), &[]).await;
                assert_eq!(mine.status(), "status 403", "{mine:?}");
                assert!(mine.head.contains(&"ownername=bob".to_string()), "{mine:?}");
                let forced = c.send(&format!("unlock {id}"), &["force=true"]).await;
                assert_eq!(forced.status(), "status 403", "{forced:?}");
                assert!(
                    forced.body_text()[0].contains("administrator"),
                    "{forced:?}"
                );
                c.quit().await;
            },
        )
        .await
        .unwrap();

        let administrator = TransferContext {
            administers: true,
            ..f.context(LfsActionKind::Upload, f.user())
        };
        let id = lock.id.clone();
        session(administrator, |mut c| async move {
            c.handshake().await;
            let forced = c.send(&format!("unlock {id}"), &["force=true"]).await;
            assert_eq!(forced.status(), "status 200", "{forced:?}");
            c.quit().await;
        })
        .await
        .unwrap();
        let page = locks::list_locks(&f.db, f.repo.id, LockFilter::default())
            .await
            .unwrap();
        assert!(page.locks.is_empty(), "the forced unlock left the lock");
    }

    #[tokio::test]
    async fn an_upload_over_the_budget_fails_the_batch_instead_of_reading_as_present() {
        let mut f = fixture().await;
        f.limits.repo_quota_bytes = Some(4);
        let oid = oid_of(b"too big");
        session(
            f.context(LfsActionKind::Upload, f.user()),
            |mut c| async move {
                c.handshake().await;
                let batch = c
                    .send_with("batch", &[], &[format!("{oid} 7").as_bytes()])
                    .await;
                assert_eq!(batch.status(), "status 507", "{batch:?}");
                assert!(batch.body_text()[0].starts_with(&oid), "{batch:?}");
                c.quit().await;
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn an_unknown_command_and_a_wrong_version_are_answered_not_dropped() {
        let f = fixture().await;
        session(
            f.context(LfsActionKind::Download, f.user()),
            |mut c| async move {
                let _advertisement = c.read().await;
                assert_eq!(c.send("version 2", &[]).await.status(), "status 400");
                assert_eq!(c.send("version 1", &[]).await.status(), "status 200");
                let unknown = c.send_with("frobnicate", &["x=1"], &[b"data"]).await;
                assert_eq!(unknown.status(), "status 400");
                c.quit().await;
            },
        )
        .await
        .unwrap();
    }
}
