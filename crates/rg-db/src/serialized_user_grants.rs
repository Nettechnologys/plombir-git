//! Compatibility-mirror cleanup for user ids stored inside JSON text columns.
//!
//! Normalized grant rows now carry the enforcing foreign keys, but a cascade
//! cannot rewrite their historical JSON response mirrors. Leaving a deleted id
//! in a mirror would make it disagree with authorization state and fail every
//! verified read. Keep the inventory explicit and perform every rewrite in the
//! transaction that removes the user row.

use std::collections::HashSet;

use anyhow::{bail, Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QuerySelect};

use crate::entities::{ci_environment, protected_branch, protected_tag, user};

/// Source-level inventory guarded by [`tests::every_serialized_user_grant_is_registered`].
#[cfg(test)]
pub(crate) const SERIALIZED_USER_ID_GRANTS: &[(&str, &str)] = &[
    ("ci_environment.rs", "allowed_approver_ids"),
    ("protected_branch.rs", "allowed_push_user_ids"),
    ("protected_tag.rs", "allowed_user_ids"),
];

#[derive(Clone, Copy)]
enum GrantLocation {
    ProtectedBranch,
    ProtectedTag,
    CiEnvironment,
}

impl GrantLocation {
    fn table(self) -> &'static str {
        match self {
            Self::ProtectedBranch => "protected_branches",
            Self::ProtectedTag => "protected_tags",
            Self::CiEnvironment => "ci_environments",
        }
    }

    fn column(self) -> &'static str {
        match self {
            Self::ProtectedBranch => "allowed_push_user_ids",
            Self::ProtectedTag => "allowed_user_ids",
            Self::CiEnvironment => "allowed_approver_ids",
        }
    }
}

#[derive(Clone, Copy)]
enum KeepIds<'a> {
    Except(i64),
    Existing(&'a HashSet<i64>),
}

impl KeepIds<'_> {
    fn contains(self, id: i64) -> bool {
        match self {
            Self::Except(deleted_id) => id != deleted_id,
            Self::Existing(existing) => existing.contains(&id),
        }
    }
}

struct PendingUpdate {
    location: GrantLocation,
    row_id: i64,
    original: String,
    replacement: String,
}

fn plan_one(
    pending: &mut Vec<PendingUpdate>,
    location: GrantLocation,
    row_id: i64,
    original: String,
    keep: KeepIds<'_>,
) -> Result<()> {
    let mut ids: Vec<i64> = serde_json::from_str(&original).with_context(|| {
        format!(
            "db: {} row {row_id} has unreadable {}",
            location.table(),
            location.column()
        )
    })?;
    let old_len = ids.len();
    ids.retain(|id| keep.contains(*id));
    if ids.len() == old_len {
        return Ok(());
    }
    let replacement = serde_json::to_string(&ids).with_context(|| {
        format!(
            "db: serialize cleaned {}.{}",
            location.table(),
            location.column()
        )
    })?;
    pending.push(PendingUpdate {
        location,
        row_id,
        original,
        replacement,
    });
    Ok(())
}

async fn plan_updates(db: &impl ConnectionTrait, keep: KeepIds<'_>) -> Result<Vec<PendingUpdate>> {
    let branches: Vec<(i64, Option<String>)> = protected_branch::Entity::find()
        .select_only()
        .columns([
            protected_branch::Column::Id,
            protected_branch::Column::AllowedPushUserIds,
        ])
        .filter(protected_branch::Column::AllowedPushUserIds.is_not_null())
        .into_tuple()
        .all(db)
        .await
        .context("db: inventory protected branch user grants")?;
    let tags: Vec<(i64, Option<String>)> = protected_tag::Entity::find()
        .select_only()
        .columns([
            protected_tag::Column::Id,
            protected_tag::Column::AllowedUserIds,
        ])
        .filter(protected_tag::Column::AllowedUserIds.is_not_null())
        .into_tuple()
        .all(db)
        .await
        .context("db: inventory protected tag user grants")?;
    let environments: Vec<(i64, Option<String>)> = ci_environment::Entity::find()
        .select_only()
        .columns([
            ci_environment::Column::Id,
            ci_environment::Column::AllowedApproverIds,
        ])
        .filter(ci_environment::Column::AllowedApproverIds.is_not_null())
        .into_tuple()
        .all(db)
        .await
        .context("db: inventory environment user grants")?;

    // Validate every stored value before the first write. Migration runners are
    // not transactional on every backend, and account deletion should not
    // partially clean two rule kinds before discovering bad JSON in the third.
    let mut pending = Vec::new();
    for (row_id, original) in branches {
        if let Some(original) = original {
            plan_one(
                &mut pending,
                GrantLocation::ProtectedBranch,
                row_id,
                original,
                keep,
            )?;
        }
    }
    for (row_id, original) in tags {
        if let Some(original) = original {
            plan_one(
                &mut pending,
                GrantLocation::ProtectedTag,
                row_id,
                original,
                keep,
            )?;
        }
    }
    for (row_id, original) in environments {
        if let Some(original) = original {
            plan_one(
                &mut pending,
                GrantLocation::CiEnvironment,
                row_id,
                original,
                keep,
            )?;
        }
    }
    Ok(pending)
}

async fn apply_updates(db: &impl ConnectionTrait, pending: Vec<PendingUpdate>) -> Result<()> {
    let now = chrono::Utc::now();
    for update in pending {
        let result = match update.location {
            GrantLocation::ProtectedBranch => protected_branch::Entity::update_many()
                .col_expr(
                    protected_branch::Column::AllowedPushUserIds,
                    Expr::value(Some(update.replacement)),
                )
                .col_expr(protected_branch::Column::UpdatedAt, Expr::value(now))
                .filter(protected_branch::Column::Id.eq(update.row_id))
                .filter(protected_branch::Column::AllowedPushUserIds.eq(Some(update.original)))
                .exec(db)
                .await
                .context("db: clean protected branch user grant")?,
            GrantLocation::ProtectedTag => protected_tag::Entity::update_many()
                .col_expr(
                    protected_tag::Column::AllowedUserIds,
                    Expr::value(Some(update.replacement)),
                )
                .col_expr(protected_tag::Column::UpdatedAt, Expr::value(now))
                .filter(protected_tag::Column::Id.eq(update.row_id))
                .filter(protected_tag::Column::AllowedUserIds.eq(Some(update.original)))
                .exec(db)
                .await
                .context("db: clean protected tag user grant")?,
            GrantLocation::CiEnvironment => ci_environment::Entity::update_many()
                .col_expr(
                    ci_environment::Column::AllowedApproverIds,
                    Expr::value(Some(update.replacement)),
                )
                .col_expr(ci_environment::Column::UpdatedAt, Expr::value(now))
                .filter(ci_environment::Column::Id.eq(update.row_id))
                .filter(ci_environment::Column::AllowedApproverIds.eq(Some(update.original)))
                .exec(db)
                .await
                .context("db: clean environment user grant")?,
        };
        if result.rows_affected != 1 {
            bail!(
                "db: {} row {} changed while cleaning {}",
                update.location.table(),
                update.row_id,
                update.location.column()
            );
        }
    }
    Ok(())
}

async fn rewrite(db: &impl ConnectionTrait, keep: KeepIds<'_>) -> Result<()> {
    let pending = plan_updates(db, keep).await?;
    apply_updates(db, pending).await
}

/// Remove one account from every serialized authorization grant.
pub(crate) async fn remove_user(db: &impl ConnectionTrait, user_id: i64) -> Result<()> {
    rewrite(db, KeepIds::Except(user_id)).await
}

/// Migration backfill: remove ids that no current user row resolves.
pub(crate) async fn prune_missing_users(db: &impl ConnectionTrait) -> Result<()> {
    let existing: HashSet<i64> = user::Entity::find()
        .select_only()
        .column(user::Column::Id)
        .into_tuple()
        .all(db)
        .await
        .context("db: inventory live users for serialized grant cleanup")?
        .into_iter()
        .collect();
    rewrite(db, KeepIds::Existing(&existing)).await
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::*;

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    type GrantKey = (String, String);

    fn serialized_user_grants(file: &str, source: &str) -> BTreeMap<GrantKey, usize> {
        let code = rust_source::production_rust_code_only(source);
        let mut discovered = BTreeMap::new();

        for (line_number, line) in code.lines().enumerate() {
            let Some(field) = line.trim().strip_prefix("pub ") else {
                continue;
            };
            let Some((name, ty)) = field.split_once(':') else {
                continue;
            };
            let name = name.trim();
            let serialized_id_list = ty.contains("String")
                && name.ends_with("_ids")
                && (name.contains("user")
                    || name.contains("approver")
                    || name.contains("reviewer")
                    || name.starts_with("allowed_"));
            if serialized_id_list {
                discovered.insert((file.to_owned(), name.to_owned()), line_number + 1);
            }
        }

        discovered
    }

    fn assert_registry_matches(
        discovered: &BTreeMap<GrantKey, usize>,
        registered: &BTreeSet<GrantKey>,
    ) {
        let unregistered: Vec<_> = discovered
            .iter()
            .filter(|(grant, _)| !registered.contains(*grant))
            .map(|((file, field), line)| format!("{file}:{line} `{field}`"))
            .collect();
        let without_field: Vec<_> = registered
            .iter()
            .filter(|grant| !discovered.contains_key(*grant))
            .map(|(file, field)| format!("{file} `{field}`"))
            .collect();

        assert!(
            unregistered.is_empty() && without_field.is_empty(),
            "a serialized user-id grant was added or removed without updating its deletion cleanup\n\
             unregistered production fields: {unregistered:#?}\n\
             registrations without a production field: {without_field:#?}"
        );
    }

    #[test]
    fn serialized_grant_census_reads_only_production_fields() {
        const SAMPLE: &str = r####"
// pub allowed_line_comment_ids: String,
/*
pub allowed_block_comment_ids: String,
*/
const NORMAL: &str = "
pub allowed_normal_literal_ids: String,
";
const RAW: &str = r#"
pub allowed_raw_literal_ids: String,
"#;
const BYTES: &[u8] = b"
pub allowed_byte_literal_ids: String,
";

pub allowed_user_ids: String,

#[cfg(test)]
mod early_tests {
    pub allowed_early_test_ids: String,
    const BRACE_DECOY: &str = "}";
}

pub allowed_approver_ids: Option<String>,

#[cfg(test)]
mod tail_tests {
    pub allowed_test_tail_ids: String,
}
"####;
        let line_of = |needle: &str| {
            SAMPLE
                .lines()
                .position(|line| line.contains(needle))
                .map(|line| line + 1)
                .unwrap_or_else(|| panic!("sample has no line containing `{needle}`"))
        };

        assert_eq!(
            serialized_user_grants("fixture.rs", SAMPLE),
            BTreeMap::from([
                (
                    ("fixture.rs".into(), "allowed_user_ids".into()),
                    line_of("pub allowed_user_ids"),
                ),
                (
                    ("fixture.rs".into(), "allowed_approver_ids".into()),
                    line_of("pub allowed_approver_ids"),
                ),
            ])
        );
    }

    #[test]
    #[should_panic(expected = "registrations without a production field")]
    fn removing_a_production_field_is_not_hidden_by_a_literal_decoy() {
        let source = r####"
pub allowed_user_ids: String,
const DECOY: &str = r#"pub allowed_user_ids: String,"#;
"####;
        let without_production_field = source.replacen("pub allowed_user_ids: String,", "", 1);
        let discovered = serialized_user_grants("fixture.rs", &without_production_field);
        let registered = BTreeSet::from([("fixture.rs".into(), "allowed_user_ids".into())]);

        assert_registry_matches(&discovered, &registered);
    }

    #[test]
    #[should_panic(expected = "fixture.rs:2 `allowed_user_ids`")]
    fn an_unregistered_production_field_reports_its_original_line() {
        let discovered = serialized_user_grants(
            "fixture.rs",
            "// diagnostic line one\npub allowed_user_ids: String,\n",
        );

        assert_registry_matches(&discovered, &BTreeSet::new());
    }

    #[test]
    fn every_serialized_user_grant_is_registered() {
        let entity_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/entities");
        let mut discovered = BTreeMap::new();
        for entry in std::fs::read_dir(&entity_dir).expect("read entity source directory") {
            let path = entry.expect("read entity source entry").path();
            if path.extension().and_then(|value| value.to_str()) != Some("rs") {
                continue;
            }
            let file = path
                .file_name()
                .and_then(|value| value.to_str())
                .expect("entity file name")
                .to_string();
            let source = std::fs::read_to_string(&path).expect("read entity source");
            discovered.extend(serialized_user_grants(&file, &source));
        }

        let registered: BTreeSet<_> = SERIALIZED_USER_ID_GRANTS
            .iter()
            .map(|(file, field)| ((*file).to_string(), (*field).to_string()))
            .collect();
        assert_registry_matches(&discovered, &registered);
    }
}
