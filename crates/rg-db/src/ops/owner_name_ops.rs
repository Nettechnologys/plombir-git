//! `owner_names`: one row per owner name, account or organization.
//!
//! The database writes these rows itself, from triggers on `users` and
//! `organizations` (`m20261009_000001_owner_names_unique`), and keys each one by
//! its name folded to lower case (`m20261014_000001_owner_names_fold_case`).
//! Nothing here writes to the table; it answers the two questions the rule
//! raises.

use anyhow::{Context, Result};
use sea_orm::sea_query::{Alias, Expr, Query};
use sea_orm::*;

/// Whether an account or an organization holds `name` in any case.
///
/// Owner names are ASCII, so folding here with `to_ascii_lowercase` matches the
/// `LOWER()` the triggers fold the stored key with, on every backend.
pub async fn is_held_in_any_case(db: &DatabaseConnection, name: &str) -> Result<bool> {
    let query = Query::select()
        .expr(Expr::val(1))
        .from(Alias::new("owner_names"))
        .and_where(Expr::col(Alias::new("name_key")).eq(name.to_ascii_lowercase()))
        .limit(1)
        .to_owned();
    let found = db
        .query_one(db.get_database_backend().build(&query))
        .await
        .context("db: look an owner name up in any case")?;
    Ok(found.is_some())
}

/// Every owner name that predates the case rule and shares its key with
/// another holder, as `(name, holder of the key)`, sorted by name.
///
/// The upgrade gave the key to one holder and left the others keyless rather
/// than refuse to run: which of them to rename is the operator's decision.
pub async fn list_names_held_only_in_another_case(
    db: &DatabaseConnection,
) -> Result<Vec<(String, String)>> {
    let rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT l.name AS name, h.name AS holder FROM owner_names l \
             JOIN owner_names h ON h.name_key = LOWER(l.name) \
             WHERE l.name_key IS NULL ORDER BY l.name"
                .to_string(),
        ))
        .await
        .context("db: list owner names that differ only in case")?;
    rows.into_iter()
        .map(|row| {
            Ok((
                row.try_get::<String>("", "name")
                    .context("db: read owner name")?,
                row.try_get::<String>("", "holder")
                    .context("db: read owner name holder")?,
            ))
        })
        .collect()
}
