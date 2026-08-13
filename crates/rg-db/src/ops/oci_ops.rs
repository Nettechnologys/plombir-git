//! OCI Registry database operations.
//!
//! Covers oci_repository, oci_manifest, oci_blob, oci_upload and
//! oci_publication_lease tables.

use crate::entities::{
    oci_blob, oci_manifest, oci_publication_lease, oci_repository, oci_tag, oci_upload,
};
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

/// Find the tag row that names an image, inside one repository.
async fn find_tag<C: ConnectionTrait>(
    db: &C,
    oci_repo_id: i64,
    tag: &str,
) -> Result<Option<oci_tag::Model>, DbErr> {
    use oci_tag::Entity as Tag;
    Tag::find()
        .filter(oci_tag::Column::OciRepositoryId.eq(oci_repo_id))
        .filter(oci_tag::Column::Tag.eq(tag))
        .one(db)
        .await
}

/// Find the manifest a tag currently names.
///
/// Two reads rather than a join: the tag row is the mapping and the manifest
/// row is the image, and they are separate precisely so that several tags can
/// name one image (card_56f118bbe845).
pub async fn find_manifest_by_tag(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    tag: &str,
) -> Result<Option<oci_manifest::Model>, DbErr> {
    use oci_manifest::Entity as Manifest;
    let Some(named) = find_tag(db, oci_repo_id, tag).await? else {
        return Ok(None);
    };
    Manifest::find_by_id(named.oci_manifest_id).one(db).await
}

/// List tags for an OCI repository in the order used by marker pagination.
///
/// `limit` is the page size, and it is the database that applies it. Tagging
/// every commit is ordinary CI practice, so the number of tags grows with the
/// repository's age; reading the whole table to hand back one row would make
/// the cost of a page a function of that age rather than of what was asked
/// for, and `n` exists precisely so the client can bound it.
///
/// A caller that also needs the "there is a next page" signal asks for one row
/// more than it will serve: the surplus row *is* the signal, and it costs one
/// row instead of the whole tail.
pub async fn list_tags(
    db: &DatabaseConnection,
    oci_repo_id: i64,
    last: Option<&str>,
    limit: Option<u64>,
) -> Result<Vec<String>, DbErr> {
    use oci_tag::Entity as Tag;
    let mut query = Tag::find().filter(oci_tag::Column::OciRepositoryId.eq(oci_repo_id));
    if let Some(last) = last {
        query = query.filter(oci_tag::Column::Tag.gt(last));
    }
    if let Some(limit) = limit {
        query = query.limit(limit);
    }
    let tags = query.order_by_asc(oci_tag::Column::Tag).all(db).await?;
    Ok(tags.into_iter().map(|named| named.tag).collect())
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

/// Publish a manifest under a tag once, reporting whether the tag INSERT failed.
///
/// Two writes in one transaction, in this order:
///
/// 1. the image itself, keyed by `(repository, digest)`. The same bytes pushed
///    under a second name are the same row, so the conflict is settled inside
///    the statement rather than raised at the caller — that collision *is* the
///    ordinary `docker push $SHA && docker push latest` and used to answer 500.
/// 2. the name, keyed by `(repository, tag)`. The UPDATE comes first so an
///    existing tag is never deleted just to be replaced; only the INSERT that
///    follows an absent tag can race another first push, and only that path
///    needs the caller's UNIQUE-conflict classification.
///
/// A digest already present keeps the row it already has. The bytes decide the
/// media type, size and schema version, and the recorded pusher stays whoever
/// first published them — re-tagging an image is not a re-publication of it.
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
    use oci_tag::Entity as Tag;

    let transaction = db.begin().await.map_err(|error| (error, false))?;
    Manifest::insert(manifest_model(
        oci_repo_id,
        new_digest,
        new_media_type,
        new_size,
        new_manifest_json,
        new_schema_version,
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
    .await
    .map_err(|error| (error, false))?;

    let manifest = Manifest::find()
        .filter(oci_manifest::Column::OciRepositoryId.eq(oci_repo_id))
        .filter(oci_manifest::Column::Digest.eq(new_digest))
        .one(&transaction)
        .await
        .map_err(|error| (error, false))?
        .ok_or_else(|| {
            (
                DbErr::Custom(format!(
                    "OCI manifest {new_digest} was absent after conflict-safe insertion"
                )),
                false,
            )
        })?;

    let now = Utc::now();
    Tag::update_many()
        .col_expr(oci_tag::Column::OciManifestId, Expr::value(manifest.id))
        .col_expr(oci_tag::Column::UpdatedAt, Expr::value(now))
        .filter(oci_tag::Column::OciRepositoryId.eq(oci_repo_id))
        .filter(oci_tag::Column::Tag.eq(tag))
        .exec(&transaction)
        .await
        .map_err(|error| (error, false))?;

    // `rows_affected` is not a presence test: MySQL reports zero when the tag
    // already named this image. Read back through this transaction so an
    // idempotent re-push does not fall into INSERT and collide with itself.
    let started_without_tag = find_tag(&transaction, oci_repo_id, tag)
        .await
        .map_err(|error| (error, false))?
        .is_none();

    if started_without_tag {
        oci_tag::ActiveModel {
            id: NotSet,
            oci_repository_id: Set(oci_repo_id),
            tag: Set(tag.to_string()),
            oci_manifest_id: Set(manifest.id),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&transaction)
        .await
        .map_err(|error| (error, true))?;
    }

    claim_referenced_blobs(&transaction, oci_repo_id, referenced_blob_digests)
        .await
        .map_err(|error| (error, started_without_tag))?;

    transaction
        .commit()
        .await
        .map_err(|error| (error, started_without_tag))?;
    Ok(manifest)
}

/// Record a manifest and atomically point a tag at it.
///
/// The image insert, the tag replacement and the blob-reference claims are one
/// database transaction. A failed lookup must not be treated as an absent tag,
/// and a failed replacement must leave the old tag live rather than deleting it
/// before the new row can be written.
///
/// Concurrent first pushes need one extra recovery path.  If this transaction
/// read no tag and lost the INSERT to a UNIQUE constraint, only one key can
/// have been violated — the image itself resolves its conflict in-statement —
/// so a fresh tag lookup confirms what happened: a row at this tag means
/// another push won the race, and retrying gives this request normal
/// last-writer-wins semantics.  No tag row means something other than that race
/// failed the write, which must remain an error rather than a fabricated
/// success.
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
            if find_tag(db, oci_repo_id, tag).await?.is_none() {
                return Err(error);
            }

            // A winner at the same tag proves this was the tag constraint.
            // The retry still has a complete transaction, so a failed
            // replacement cannot make the tag vanish.
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
