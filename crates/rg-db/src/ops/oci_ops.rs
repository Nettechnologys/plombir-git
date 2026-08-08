//! OCI Registry database operations.
//!
//! Covers oci_repository, oci_manifest, oci_blob, oci_upload and
//! oci_publication_lease tables.

use crate::entities::{oci_blob, oci_manifest, oci_publication_lease, oci_repository, oci_upload};
use chrono::Utc;
use sea_orm::prelude::DateTimeUtc;
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::*;

// ── OCI Repository ─────────────────────────────────────────

/// Find an OCI repository by ForgeKeep repo_id.
pub async fn find_repo_by_id(
    db: &DatabaseConnection,
    repo_id: i64,
) -> Result<Option<oci_repository::Model>, DbErr> {
    use oci_repository::Entity as OciRepo;
    OciRepo::find()
        .filter(oci_repository::Column::RepoId.eq(repo_id))
        .one(db)
        .await
}

/// Find or create an OCI repository.
#[allow(clippy::too_many_arguments)]
pub async fn find_or_create_repo(
    db: &DatabaseConnection,
    repo_id: i64,
    namespace: &str,
    owner_id: i64,
) -> Result<oci_repository::Model, DbErr> {
    use oci_repository::Entity as OciRepo;

    let find_existing = || {
        OciRepo::find()
            .filter(oci_repository::Column::RepoId.eq(repo_id))
            .filter(oci_repository::Column::Namespace.eq(namespace))
            .one(db)
    };
    if let Some(r) = find_existing().await? {
        return Ok(r);
    }
    let now = Utc::now();
    let m = oci_repository::ActiveModel {
        id: NotSet,
        repo_id: Set(repo_id),
        namespace: Set(namespace.to_string()),
        owner_id: Set(owner_id),
        is_public: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
    };
    OciRepo::insert(m)
        .on_conflict(
            OnConflict::columns([
                oci_repository::Column::RepoId,
                oci_repository::Column::Namespace,
            ])
            // MySQL needs a harmless assignment for its DO NOTHING polyfill;
            // PostgreSQL and SQLite emit DO NOTHING for this conflict target.
            .do_nothing_on([oci_repository::Column::Id])
            .to_owned(),
        )
        .do_nothing()
        .exec(db)
        .await?;

    find_existing().await?.ok_or_else(|| {
        DbErr::Custom(format!(
            "OCI repository {namespace} was absent after conflict-safe creation"
        ))
    })
}

// ── OCI Manifest ────────────────────────────────────────────

/// Whether a content-addressed manifest row was created by this request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestInsertOutcome {
    Inserted,
    Existing,
}

#[allow(clippy::too_many_arguments)]
fn manifest_model(
    oci_repo_id: i64,
    digest: &str,
    tag: Option<&str>,
    media_type: &str,
    size: i64,
    manifest_json: &str,
    schema_version: i32,
    push_by: Option<i64>,
) -> oci_manifest::ActiveModel {
    let now = Utc::now();
    oci_manifest::ActiveModel {
        id: NotSet,
        oci_repository_id: Set(oci_repo_id),
        digest: Set(digest.to_string()),
        tag: Set(tag.map(str::to_owned)),
        media_type: Set(media_type.to_string()),
        size: Set(size),
        manifest_json: Set(manifest_json.to_string()),
        schema_version: Set(schema_version),
        push_by: Set(push_by),
        created_at: Set(now),
        updated_at: Set(now),
    }
}

/// Find a manifest by digest.
pub async fn find_manifest_by_digest(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    digest: &str,
) -> Result<Option<oci_manifest::Model>, DbErr> {
    use oci_manifest::Entity as Manifest;
    Manifest::find()
        .filter(oci_manifest::Column::OciRepositoryId.eq(oci_repo_id))
        .filter(oci_manifest::Column::Digest.eq(digest))
        .one(db)
        .await
}

/// Find a manifest by tag.
pub async fn find_manifest_by_tag(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    tag: &str,
) -> Result<Option<oci_manifest::Model>, DbErr> {
    use oci_manifest::Entity as Manifest;
    Manifest::find()
        .filter(oci_manifest::Column::OciRepositoryId.eq(oci_repo_id))
        .filter(oci_manifest::Column::Tag.eq(tag))
        .one(db)
        .await
}

/// List all tags for an OCI repository.
pub async fn list_tags(db: &DatabaseConnection, oci_repo_id: i64) -> Result<Vec<String>, DbErr> {
    use oci_manifest::Entity as Manifest;
    let manifests = Manifest::find()
        .filter(oci_manifest::Column::OciRepositoryId.eq(oci_repo_id))
        .filter(oci_manifest::Column::Tag.is_not_null())
        .all(db)
        .await?;
    Ok(manifests.into_iter().filter_map(|m| m.tag).collect())
}

/// Insert a new manifest.
#[allow(clippy::too_many_arguments)]
pub async fn insert_manifest(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    digest: &str,
    tag: Option<&str>,
    media_type: &str,
    size: i64,
    manifest_json: &str,
    schema_version: i32,
    push_by: Option<i64>,
) -> Result<oci_manifest::Model, DbErr> {
    manifest_model(
        oci_repo_id,
        digest,
        tag,
        media_type,
        size,
        manifest_json,
        schema_version,
        push_by,
    )
    .insert(db)
    .await
}

/// Insert a digest-addressed manifest and claim its blob references exactly once.
///
/// A client retry or two concurrent PUTs can legitimately reach the unique
/// `(repository, digest)` key. The conflict is a successful no-op; only the
/// request that inserts the row claims the blobs. Keeping both in one
/// transaction is what makes a manifest row and the layers it names arrive
/// together — a row committed ahead of a failed claim is one every later retry
/// would mistake for a fully recorded image.
#[allow(clippy::too_many_arguments)]
pub async fn insert_digest_manifest(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    digest: &str,
    media_type: &str,
    size: i64,
    manifest_json: &str,
    schema_version: i32,
    push_by: Option<i64>,
    referenced_blob_digests: &[String],
) -> Result<ManifestInsertOutcome, DbErr> {
    use oci_manifest::Entity as Manifest;

    let transaction = db.begin().await?;
    let inserted = Manifest::insert(manifest_model(
        oci_repo_id,
        digest,
        None,
        media_type,
        size,
        manifest_json,
        schema_version,
        push_by,
    ))
    .on_conflict(
        OnConflict::columns([
            oci_manifest::Column::OciRepositoryId,
            oci_manifest::Column::Digest,
        ])
        .do_nothing_on([oci_manifest::Column::Id])
        .to_owned(),
    )
    .do_nothing()
    .exec(&transaction)
    .await?;

    let outcome = match inserted {
        TryInsertResult::Inserted(_) => {
            claim_referenced_blobs(&transaction, oci_repo_id, referenced_blob_digests).await?;
            ManifestInsertOutcome::Inserted
        }
        TryInsertResult::Conflicted => ManifestInsertOutcome::Existing,
        TryInsertResult::Empty => {
            return Err(DbErr::Custom(
                "digest manifest insert unexpectedly contained no values".to_string(),
            ));
        }
    };

    transaction.commit().await?;
    Ok(outcome)
}

/// Write a tagged manifest once, reporting whether its INSERT path failed.
///
/// The UPDATE comes first so an existing tag is never deleted just to be
/// replaced. If no row matches, the INSERT can race with another first push;
/// only that path needs the caller's UNIQUE-conflict classification.
#[allow(clippy::too_many_arguments)]
async fn upsert_tag_manifest_once(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    tag: &str,
    new_digest: &str,
    new_media_type: &str,
    new_size: i64,
    new_manifest_json: &str,
    new_schema_version: i32,
    push_by: Option<i64>,
    referenced_blob_digests: &[String],
) -> Result<oci_manifest::Model, (DbErr, bool)> {
    use oci_manifest::Entity as Manifest;

    let transaction = db.begin().await.map_err(|error| (error, false))?;
    Manifest::update_many()
        .col_expr(oci_manifest::Column::Digest, Expr::value(new_digest))
        .col_expr(oci_manifest::Column::MediaType, Expr::value(new_media_type))
        .col_expr(oci_manifest::Column::Size, Expr::value(new_size))
        .col_expr(
            oci_manifest::Column::ManifestJson,
            Expr::value(new_manifest_json),
        )
        .col_expr(
            oci_manifest::Column::SchemaVersion,
            Expr::value(new_schema_version),
        )
        .col_expr(oci_manifest::Column::PushBy, Expr::value(push_by))
        .col_expr(oci_manifest::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(oci_manifest::Column::OciRepositoryId.eq(oci_repo_id))
        .filter(oci_manifest::Column::Tag.eq(tag))
        .exec(&transaction)
        .await
        .map_err(|error| (error, false))?;

    // `rows_affected` is not a presence test: MySQL reports zero when all
    // assigned values were already equal. Read back through this transaction
    // so an idempotent tag PUT does not fall into INSERT and misclassify its
    // own tag constraint as a digest conflict.
    let existing = Manifest::find()
        .filter(oci_manifest::Column::OciRepositoryId.eq(oci_repo_id))
        .filter(oci_manifest::Column::Tag.eq(tag))
        .one(&transaction)
        .await
        .map_err(|error| (error, false))?;
    let started_without_tag = existing.is_none();

    let manifest = if let Some(existing) = existing {
        existing
    } else {
        manifest_model(
            oci_repo_id,
            new_digest,
            Some(tag),
            new_media_type,
            new_size,
            new_manifest_json,
            new_schema_version,
            push_by,
        )
        .insert(&transaction)
        .await
        .map_err(|error| (error, true))?
    };

    claim_referenced_blobs(&transaction, oci_repo_id, referenced_blob_digests)
        .await
        .map_err(|error| (error, started_without_tag))?;

    transaction
        .commit()
        .await
        .map_err(|error| (error, started_without_tag))?;
    Ok(manifest)
}

/// Insert a tagged manifest or atomically move an existing tag to it.
///
/// The tag lookup, replacement and blob-reference claims are one database
/// transaction. A failed lookup must not be treated as an absent tag, and a
/// failed replacement must leave the old tag live rather than deleting it
/// before the new row can be written.
///
/// Concurrent first pushes need one extra recovery path.  If this transaction
/// read no tag and lost the INSERT to a UNIQUE constraint, a fresh tag lookup
/// distinguishes the two unique keys: a row at this tag means another push won
/// the tag race, so retrying gives this request normal last-writer-wins
/// semantics.  No tag row means the collision was on the separate digest key,
/// which must remain an error rather than becoming a fabricated success.
#[allow(clippy::too_many_arguments)]
pub async fn upsert_tag_manifest(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    tag: &str,
    new_digest: &str,
    new_media_type: &str,
    new_size: i64,
    new_manifest_json: &str,
    new_schema_version: i32,
    push_by: Option<i64>,
    referenced_blob_digests: &[String],
) -> Result<oci_manifest::Model, DbErr> {
    let write_once = || {
        upsert_tag_manifest_once(
            db,
            oci_repo_id,
            tag,
            new_digest,
            new_media_type,
            new_size,
            new_manifest_json,
            new_schema_version,
            push_by,
            referenced_blob_digests,
        )
    };

    match write_once().await {
        Ok(manifest) => Ok(manifest),
        Err((error, true)) if crate::is_unique_violation(&error) => {
            // The failed transaction is gone before this query.  PostgreSQL
            // aborts a transaction after a constraint error, so re-reading in
            // it would turn a recoverable tag race into another database error.
            if find_manifest_by_tag(db, oci_repo_id, tag).await?.is_none() {
                return Err(error);
            }

            // A winner at the same tag proves this was the tag constraint,
            // not the digest constraint.  The retry still has a complete
            // transaction, so a failed replacement cannot make the tag vanish.
            write_once().await.map_err(|(retry_error, _)| retry_error)
        }
        Err((error, _)) => Err(error),
    }
}

// ── OCI Blob ────────────────────────────────────────────────

/// Find a blob by digest.
pub async fn find_blob(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    digest: &str,
) -> Result<Option<oci_blob::Model>, DbErr> {
    use oci_blob::Entity as Blob;
    Blob::find()
        .filter(oci_blob::Column::OciRepositoryId.eq(oci_repo_id))
        .filter(oci_blob::Column::Digest.eq(digest))
        .one(db)
        .await
}

/// Insert a blob record unless this repository already owns the digest.
///
/// OCI blob uploads are content-addressed and idempotent: a client retry or two
/// concurrent finalizers can legitimately reach this write for the same
/// `(repository, digest)`. Keep that conflict inside the statement so both
/// requests succeed atomically; every other database failure still propagates.
pub async fn insert_blob(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    digest: &str,
    media_type: &str,
    size: i64,
    storage_path: &str,
) -> Result<(), DbErr> {
    let now = Utc::now();
    let m = oci_blob::ActiveModel {
        id: NotSet,
        oci_repository_id: Set(oci_repo_id),
        digest: Set(digest.to_string()),
        media_type: Set(media_type.to_string()),
        size: Set(size),
        storage_path: Set(storage_path.to_string()),
        created_at: Set(now),
    };
    oci_blob::Entity::insert(m)
        .on_conflict(
            OnConflict::columns([oci_blob::Column::OciRepositoryId, oci_blob::Column::Digest])
                // MySQL has no conflict target and needs a harmless assignment as
                // its DO NOTHING polyfill. PostgreSQL and SQLite emit DO NOTHING
                // for the two columns above.
                .do_nothing_on([oci_blob::Column::Id])
                .to_owned(),
        )
        .exec_without_returning(db)
        .await?;
    Ok(())
}

/// Prove, inside the caller's transaction, that this repository owns every blob
/// the manifest about to be committed references.
///
/// A manifest whose layers are missing is an image that pulls a 404 for one of
/// its own parts, and the client that pushed it is long gone by the time anyone
/// finds out — so the push has to fail while it can still be retried. Both
/// manifest writers claim through here, so the digest-addressed path and the
/// tagged path cannot disagree about what a valid manifest is.
///
/// This used to be a side effect of bumping `oci_blob.ref_count`, a counter no
/// reader ever had: the registry exposes no delete of any kind, so a reference
/// could never be released and nothing collected the blobs (card_e9b4da7bf8ca).
/// The column is gone; the guarantee it was accidentally providing is not.
///
/// The claim is a read, which is enough while nothing can delete a blob row.
/// Whoever adds the first delete path owes this call a row lock — otherwise a
/// collector can remove a layer between this check and the commit that depends
/// on it.
async fn claim_referenced_blobs<C: ConnectionTrait>(
    txn: &C,
    oci_repo_id: i64,
    referenced_blob_digests: &[String],
) -> Result<(), DbErr> {
    use oci_blob::Entity as Blob;
    for blob_digest in referenced_blob_digests {
        let present = Blob::find()
            .filter(oci_blob::Column::OciRepositoryId.eq(oci_repo_id))
            .filter(oci_blob::Column::Digest.eq(blob_digest))
            .count(txn)
            .await?;
        if present != 1 {
            return Err(DbErr::Custom(format!(
                "referenced OCI blob {blob_digest} is missing from repository {oci_repo_id}"
            )));
        }
    }
    Ok(())
}

// ── OCI Upload ──────────────────────────────────────────────

/// Create a new upload session.
///
/// `digest` is born `NULL` and stays that way: finalizing a push writes the
/// digest onto the `oci_blob` row and drops the session, so the session's own
/// column is never filled in. It used to have a writer — `complete_upload`,
/// which had no callers and matched on the uuid alone, i.e. the one primitive
/// left that could rewrite another repository's session (card_07c571dfdf95).
/// Nothing reads the column either, so the writer was removed rather than
/// given the `oci_repo_id` its neighbours take.
pub async fn create_upload(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    uuid: &str,
    upload_path: &str,
) -> Result<oci_upload::Model, DbErr> {
    let now = Utc::now();
    let expires = now + chrono::Duration::hours(24);
    let m = oci_upload::ActiveModel {
        id: NotSet,
        oci_repository_id: Set(oci_repo_id),
        uuid: Set(uuid.to_string()),
        digest: Set(None),
        bytes_uploaded: Set(0),
        upload_path: Set(upload_path.to_string()),
        created_at: Set(now),
        expires_at: Set(expires),
    };
    m.insert(db).await
}

/// Find an upload by UUID.
pub async fn find_upload(
    db: &DatabaseConnection,
    uuid: &str,
) -> Result<Option<oci_upload::Model>, DbErr> {
    use oci_upload::Entity as Upload;
    Upload::find()
        .filter(oci_upload::Column::Uuid.eq(uuid))
        .one(db)
        .await
}

/// Update upload progress of a session **inside** `oci_repo_id`.
///
/// The uuid is instance-wide while the caller of the HTTP layer was only ever
/// admitted to one `{owner}/{repo}`, so the repository is part of the `WHERE`
/// rather than an argument the caller may forget: an update filtered on the
/// uuid alone rewrote another repository's session offset, and the victim's
/// client resumed its push from a position nobody agreed on.
///
/// Returns the number of rows written, so a caller that matched nothing learns
/// it instead of reading `Ok(())` as "recorded".
pub async fn update_upload_progress(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    uuid: &str,
    bytes_uploaded: i64,
) -> Result<u64, DbErr> {
    use oci_upload::Entity as Upload;
    let result = Upload::update_many()
        .col_expr(
            oci_upload::Column::BytesUploaded,
            Expr::value(bytes_uploaded),
        )
        .filter(oci_upload::Column::Uuid.eq(uuid))
        .filter(oci_upload::Column::OciRepositoryId.eq(oci_repo_id))
        .exec(db)
        .await?;
    Ok(result.rows_affected)
}

/// Delete an upload session **inside** `oci_repo_id`.
///
/// Same reason as [`update_upload_progress`]: finalizing a push ends with the
/// session row being dropped, and a delete keyed on the instance-wide uuid
/// alone dropped whichever repository's session happened to carry it.
pub async fn delete_upload(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    uuid: &str,
) -> Result<u64, DbErr> {
    use oci_upload::Entity as Upload;
    let result = Upload::delete_many()
        .filter(oci_upload::Column::Uuid.eq(uuid))
        .filter(oci_upload::Column::OciRepositoryId.eq(oci_repo_id))
        .exec(db)
        .await?;
    Ok(result.rows_affected)
}

/// Upload sessions whose 24h TTL has passed, each with the repository it
/// belongs to.
///
/// Returns rows rather than deleting them, and that is the whole point. The
/// predecessor was a bulk `DELETE ... WHERE expires_at < now` — which nobody
/// ever called, and which would have been wrong if they had: an abandoned
/// `docker push` leaves both a row and a staging directory of already-uploaded
/// layer bytes, and dropping the row first strands the bytes with nothing left
/// in the database that names them (card_487dc1247247). The sweep needs the
/// `oci_repository` beside each row because the staging path is keyed by
/// `{owner}/{repo}/{uuid}`, not by the row's id.
pub async fn list_expired_uploads(
    db: &DatabaseConnection,
) -> Result<Vec<(oci_upload::Model, oci_repository::Model)>, DbErr> {
    use oci_repository::Entity as OciRepo;
    use oci_upload::Entity as Upload;

    let expired = Upload::find()
        .filter(oci_upload::Column::ExpiresAt.lt(Utc::now()))
        .order_by_asc(oci_upload::Column::Id)
        .all(db)
        .await?;

    let mut rows = Vec::with_capacity(expired.len());
    for upload in expired {
        // A session whose repository row is gone is skipped rather than
        // reported: the repository deletion already retired the whole
        // `oci-uploads/{owner}/{repo}` directory, chunks in flight included,
        // so there is no path left to build and nothing left to remove.
        if let Some(repository) = OciRepo::find_by_id(upload.oci_repository_id)
            .one(db)
            .await?
        {
            rows.push((upload, repository));
        }
    }
    Ok(rows)
}

// ── OCI publication lease ───────────────────────────────────

/// Outcome of a bid for the publication lease over one storage key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationLeaseBid {
    /// Nobody held the key; it is now held by the bidding token.
    Granted,
    /// The previous holder's lease had expired and was taken over. Expiry is a
    /// guess about a dead process, not a fact — the old holder may still be
    /// running, so a taker cannot claim exclusivity over the stored bytes.
    TakenOver,
    /// Another request holds a live lease. The caller polls.
    Busy,
}

/// Bid for the exclusive right to publish the bytes under one storage key.
///
/// The key is content-addressed and stable, so blob storage cannot tell two
/// concurrent first publications apart — `exists` answers the same for the
/// request that wrote the bytes and the request that merely found them. This
/// row is the arbiter instead, and it arbitrates across processes because the
/// winner is decided by the database.
///
/// Claiming is an insert rather than an update: unlike an LFS object, an OCI
/// blob has no row of its own until the publication it is protecting has
/// already succeeded. The unique key on `storage_key` makes exactly one
/// concurrent inserter the holder, and the conflict is resolved inside the
/// statement so the losers never have to recognise a driver's duplicate-key
/// error — the same reason [`insert_blob`] handles its conflict this way.
pub async fn bid_for_publication_lease(
    db: &DatabaseConnection,
    storage_key: &str,
    token: &str,
    stale_before: DateTimeUtc,
) -> Result<PublicationLeaseBid, DbErr> {
    use oci_publication_lease::Entity as Lease;
    let now = Utc::now();

    Lease::insert(oci_publication_lease::ActiveModel {
        id: NotSet,
        storage_key: Set(storage_key.to_string()),
        token: Set(token.to_string()),
        since: Set(now),
    })
    .on_conflict(
        OnConflict::column(oci_publication_lease::Column::StorageKey)
            // MySQL has no conflict target and needs a harmless assignment as
            // its DO NOTHING polyfill. PostgreSQL and SQLite emit DO NOTHING
            // for the column above.
            .do_nothing_on([oci_publication_lease::Column::Id])
            .to_owned(),
    )
    .exec_without_returning(db)
    .await?;

    // Whether the insert above landed is not something every backend will say,
    // so ask the row who holds it.
    let Some(held) = Lease::find()
        .filter(oci_publication_lease::Column::StorageKey.eq(storage_key))
        .one(db)
        .await?
    else {
        // The holder released between the insert and this read, taking the row
        // with it. The key is free but this token does not hold it, and saying
        // otherwise would hand out a lease nothing records — the caller bids
        // again.
        return Ok(PublicationLeaseBid::Busy);
    };
    if held.token == token {
        return Ok(PublicationLeaseBid::Granted);
    }

    // Someone else holds it. Only an expired hold may be taken over, and only
    // from the exact holder this read saw: filtering on the old token is what
    // keeps two waiters from both believing they took over the same lease.
    let taken_over = Lease::update_many()
        .col_expr(oci_publication_lease::Column::Token, Expr::value(token))
        .col_expr(oci_publication_lease::Column::Since, Expr::value(now))
        .filter(oci_publication_lease::Column::StorageKey.eq(storage_key))
        .filter(oci_publication_lease::Column::Token.eq(held.token))
        .filter(oci_publication_lease::Column::Since.lt(stale_before))
        .exec(db)
        .await?;
    if taken_over.rows_affected > 0 {
        return Ok(PublicationLeaseBid::TakenOver);
    }

    Ok(PublicationLeaseBid::Busy)
}

/// Release a publication lease. Returns whether this token still held it —
/// `false` means the lease had already been taken over, which is exactly the
/// case where the holder must not assume its stored bytes are still its own.
pub async fn release_publication_lease(
    db: &DatabaseConnection,
    storage_key: &str,
    token: &str,
) -> Result<bool, DbErr> {
    use oci_publication_lease::Entity as Lease;
    let released = Lease::delete_many()
        .filter(oci_publication_lease::Column::StorageKey.eq(storage_key))
        .filter(oci_publication_lease::Column::Token.eq(token))
        .exec(db)
        .await?;
    Ok(released.rows_affected > 0)
}
