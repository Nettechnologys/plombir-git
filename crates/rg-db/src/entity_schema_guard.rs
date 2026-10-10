//! Guard against entity/schema drift (card_d33afb82797f).
//!
//! A SeaORM entity names its table in `#[sea_orm(table_name = "...")]` and its
//! columns after its `Model` fields; the migration that creates that table
//! names both through an `Iden` enum. Nothing ties the two sides together. A
//! bare `#[derive(Iden)] enum Mirror` produces the table `mirror`, an entity
//! saying `mirrors` compiles just as happily, and the pair only fails at
//! runtime — `no such table: mirrors`, on every query through that entity. The
//! whole mirror feature shipped that way and answered 500 for months; packages,
//! orgs, teams, import tasks and oauth accounts each did before it.
//!
//! So the check is empirical rather than a code convention: migrate an empty
//! database, then run a real `SELECT` through every entity. An unresolvable
//! table or column fails there exactly as it would in production, which makes
//! this the same test for the whole class — including column drift, which no
//! table-name comparison would see.
//!
//! The probe runs on SQLite only, which is enough for this class: an `Iden`
//! enum renders the same identifier on every backend, so a name that drifts
//! drifts identically on Postgres and MySQL.
//!
//! Adding an entity means adding it to `probed_entities!` below. That is not
//! left to memory: `the_probe_list_covers_every_entity` compares the list
//! against the entity source files and `entities/mod.rs`, and fails if any of
//! the three disagree.

#![cfg(test)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use sea_orm::{DatabaseConnection, EntityTrait, QuerySelect};

#[allow(dead_code)]
mod rust_source {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/support/rust_source.rs"
    ));
}

/// Expands to the probe list twice: once as names (checked against the source
/// tree) and once as a real query per entity.
macro_rules! probed_entities {
    ($($module:ident),+ $(,)?) => {
        fn probed_entity_names() -> BTreeSet<String> {
            [$(stringify!($module).to_string()),+].into_iter().collect()
        }

        /// `(entity module, database error)` for every entity whose columns the
        /// migrated schema cannot satisfy.
        async fn unreadable_entities(db: &DatabaseConnection) -> Vec<(&'static str, String)> {
            let mut failures = Vec::new();
            $(
                if let Err(e) = crate::entities::$module::Entity::find()
                    .limit(1)
                    .all(db)
                    .await
                {
                    failures.push((stringify!($module), e.to_string()));
                }
            )+
            failures
        }
    };
}

probed_entities!(
    access_token,
    access_token_repository,
    artifact,
    attachment,
    audit_log,
    board,
    board_card,
    board_column,
    ci_cache_entry,
    ci_environment,
    ci_environment_approval,
    ci_environment_approver_grant,
    ci_retention_policy,
    ci_secret,
    commit_signing_key,
    commit_status,
    deploy_key,
    email_confirmation,
    encryption_key_check,
    import_task,
    instance_settings,
    instance_signing_key,
    issue,
    issue_comment,
    issue_label,
    label,
    lfs_lock,
    lfs_object,
    login_log,
    merge_queue_entry,
    mfa_backup_code,
    milestone,
    mirror,
    mirror_sync_lease,
    notification,
    notification_setting,
    npm_dist_tag,
    npm_dist_tag_set,
    oauth_account,
    oci_blob,
    oci_manifest,
    oci_publication_lease,
    oci_repository,
    oci_tag,
    oci_upload,
    organization,
    organization_member,
    package,
    package_file,
    package_registry,
    package_version,
    passkey_credential,
    password_reset_token,
    pipeline,
    pipeline_concurrency_lock,
    pipeline_job,
    pipeline_stage,
    pr_event,
    pr_review,
    pr_reviewer_request,
    protected_branch,
    protected_branch_push_grant,
    protected_tag,
    protected_tag_push_grant,
    pull_request,
    release,
    release_asset,
    repo_collaborator,
    repo_number_floor,
    repo_star,
    repo_watch,
    repository,
    repository_redirect,
    repository_transfer_lease,
    review_comment,
    runner,
    ssh_key,
    sso_provider,
    team,
    team_member,
    thread_subscription,
    time_entry,
    user,
    user_avatar,
    webauthn_ceremony_spend,
    webhook,
    webhook_delivery,
    wiki_page,
    wiki_revision,
);

fn entities_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src/entities")
}

/// Entity module names taken from the source files on disk.
fn entity_files() -> BTreeSet<String> {
    std::fs::read_dir(entities_dir())
        .expect("read entities dir")
        .filter_map(|entry| {
            let path = entry.expect("entities dir entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                return None;
            }
            match path.file_stem().and_then(|s| s.to_str()) {
                Some("mod") | None => None,
                Some(stem) => Some(stem.to_string()),
            }
        })
        .collect()
}

fn declared_entity_name(line: &str) -> Option<String> {
    let mut tokens = line.split_whitespace();
    if tokens.next()? != "pub" || tokens.next()? != "mod" {
        return None;
    }
    let name_token = tokens.next()?;
    let (name, has_semicolon) = match name_token.strip_suffix(';') {
        Some(name) => (name, true),
        None => (name_token, tokens.next() == Some(";")),
    };
    if !has_semicolon
        || tokens.next().is_some()
        || name.is_empty()
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        || name.chars().next().is_some_and(|ch| ch.is_ascii_digit())
    {
        return None;
    }
    Some(name.to_string())
}

fn declared_entity_names(source: &str) -> BTreeSet<String> {
    rust_source::production_rust_code_only(source)
        .lines()
        .filter_map(declared_entity_name)
        .collect()
}

fn entity_inventory_contract(
    source: &str,
    on_disk: &BTreeSet<String>,
    probed: &BTreeSet<String>,
) -> Result<(), String> {
    let declared = declared_entity_names(source);
    if &declared != on_disk {
        return Err(format!(
            "entities/mod.rs and the entity source files disagree: declared={declared:?}, \
             on_disk={on_disk:?}"
        ));
    }
    if probed != on_disk {
        return Err(format!(
            "probed_entities! is out of step with the entity source files: probed={probed:?}, \
             on_disk={on_disk:?}"
        ));
    }
    Ok(())
}

/// The class check: every entity must be readable against the migrated schema.
#[tokio::test]
async fn every_entity_is_readable_after_migrations() {
    let db = sea_orm::Database::connect("sqlite::memory:")
        .await
        .expect("open in-memory sqlite");
    crate::run_migrations(&db).await.expect("run migrations");

    let failures = unreadable_entities(&db).await;

    assert!(
        failures.is_empty(),
        "these entities cannot be read against the schema the migrations build — every query \
         through them fails the same way in production:\n  {}",
        failures
            .iter()
            .map(|(module, err)| format!("entities::{module}: {err}"))
            .collect::<Vec<_>>()
            .join("\n  ")
    );
}

/// The probe list is only worth anything while it covers every entity, so it is
/// cross-checked against both the source files and the module declarations.
/// A new entity that skips this file fails here instead of silently opting out.
#[test]
fn the_probe_list_covers_every_entity() {
    let source =
        std::fs::read_to_string(entities_dir().join("mod.rs")).expect("read entities/mod.rs");
    let on_disk = entity_files();
    let probed = probed_entity_names();

    entity_inventory_contract(&source, &on_disk, &probed).unwrap_or_else(|error| panic!("{error}"));

    let removed = on_disk
        .iter()
        .next()
        .expect("the entity inventory cannot be empty");
    let declaration = format!("pub mod {removed};");
    let mutated = source.replacen(&declaration, &" ".repeat(declaration.len()), 1);
    assert_ne!(
        mutated, source,
        "mutation target `{declaration}` must exist"
    );
    assert!(
        entity_inventory_contract(&mutated, &on_disk, &probed).is_err(),
        "removing the production declaration for `{removed}` must fail the guard"
    );
}

#[test]
fn entity_module_census_ignores_non_code_and_test_only_decoys() {
    const SOURCE: &str = r####"
// pub mod line_comment;
/*
pub mod block_comment;
*/
const NORMAL: &str = "
pub mod normal_string;
";
const RAW: &str = r#"
pub mod raw_string;
"#;
const BYTES: &[u8] = b"
pub mod byte_string;
";
const RAW_BYTES: &[u8] = br##"
pub mod raw_byte_string;
"##;

pub mod live_entity /* comments between tokens stay whitespace */ ;

#[cfg(test)]
mod tests {
    pub mod test_only;
}
"####;

    assert_eq!(
        declared_entity_names(SOURCE),
        BTreeSet::from(["live_entity".to_string()])
    );
}
