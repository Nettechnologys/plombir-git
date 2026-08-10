use crate::entities::{npm_dist_tag, npm_dist_tag_set, package_version};
use sea_orm::sea_query::OnConflict;
use sea_orm::*;

/// Establish the canonical tag-set marker and report whether this caller won
/// initialization.  The token comparison works across SQLite, PostgreSQL and
/// MySQL without relying on backend-specific affected-row semantics.
pub async fn ensure_initialized(
    db: &impl ConnectionTrait,
    package_id: i64,
    initialization_token: &str,
) -> Result<bool, DbErr> {
    npm_dist_tag_set::Entity::insert(npm_dist_tag_set::ActiveModel {
        package_id: Set(package_id),
        initialization_token: Set(initialization_token.to_string()),
        created_at: Set(chrono::Utc::now()),
    })
    .on_conflict(
        OnConflict::column(npm_dist_tag_set::Column::PackageId)
            // MySQL needs a harmless assignment for its DO NOTHING polyfill;
            // PostgreSQL and SQLite emit DO NOTHING for this target.
            .do_nothing_on([npm_dist_tag_set::Column::PackageId])
            .to_owned(),
    )
    .exec_without_returning(db)
    .await?;

    let marker = npm_dist_tag_set::Entity::find_by_id(package_id)
        .one(db)
        .await?
        .ok_or_else(|| DbErr::Custom("npm dist-tag initialization marker disappeared".into()))?;
    Ok(marker.initialization_token == initialization_token)
}

pub async fn is_initialized(db: &impl ConnectionTrait, package_id: i64) -> Result<bool, DbErr> {
    Ok(npm_dist_tag_set::Entity::find_by_id(package_id)
        .one(db)
        .await?
        .is_some())
}

/// Set or move one tag atomically.  Concurrent writers serialize at the
/// database's `(package_id, tag)` unique key; the last committed write wins.
pub async fn upsert(
    db: &impl ConnectionTrait,
    package_id: i64,
    tag: &str,
    version_id: i64,
) -> Result<(), DbErr> {
    npm_dist_tag::Entity::insert(npm_dist_tag::ActiveModel {
        id: NotSet,
        package_id: Set(package_id),
        tag: Set(tag.to_string()),
        version_id: Set(version_id),
        updated_at: Set(chrono::Utc::now()),
    })
    .on_conflict(
        OnConflict::columns([npm_dist_tag::Column::PackageId, npm_dist_tag::Column::Tag])
            .update_columns([
                npm_dist_tag::Column::VersionId,
                npm_dist_tag::Column::UpdatedAt,
            ])
            .to_owned(),
    )
    .exec_without_returning(db)
    .await?;
    Ok(())
}

pub async fn delete(db: &impl ConnectionTrait, package_id: i64, tag: &str) -> Result<bool, DbErr> {
    let result = npm_dist_tag::Entity::delete_many()
        .filter(npm_dist_tag::Column::PackageId.eq(package_id))
        .filter(npm_dist_tag::Column::Tag.eq(tag))
        .exec(db)
        .await?;
    Ok(result.rows_affected == 1)
}

/// Read every tag with the version row it names.  An absent related row is
/// corruption (or disabled foreign keys), not an absent tag, so it remains an
/// error instead of being silently omitted.
pub async fn list_by_package(
    db: &impl ConnectionTrait,
    package_id: i64,
) -> Result<Vec<(String, package_version::Model)>, DbErr> {
    npm_dist_tag::Entity::find()
        .filter(npm_dist_tag::Column::PackageId.eq(package_id))
        .order_by_asc(npm_dist_tag::Column::Tag)
        .find_also_related(package_version::Entity)
        .all(db)
        .await?
        .into_iter()
        .map(|(tag, version)| match version {
            Some(version) => Ok((tag.tag, version)),
            None => Err(DbErr::Custom(format!(
                "npm dist-tag {:?} for package {} points at missing version {}",
                tag.tag, tag.package_id, tag.version_id
            ))),
        })
        .collect()
}
