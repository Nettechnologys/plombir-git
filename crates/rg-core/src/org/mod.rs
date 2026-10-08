//! Organization service — business logic for org/team management.

use anyhow::{Context, Result};
use sea_orm::DatabaseConnection;

use rg_db::ops::org_ops;

/// Create a new organization.
pub async fn create_org(
    db: &DatabaseConnection,
    name: &str,
    display_name: Option<&str>,
    description: Option<&str>,
    owner_id: i64,
    visibility: &str,
) -> Result<rg_db::entities::organization::Model> {
    // Validate org name (same rules as username)
    crate::validate_username(name)?;

    // Check visibility
    if visibility != "public" && visibility != "private" {
        return Err(crate::error::invalid_request(
            "visibility must be 'public' or 'private'",
        ));
    }

    // The one answer both the pre-read and a losing insert give, so a caller
    // cannot tell which of the two noticed — and so the response code does not
    // become a side channel for "you lost the race".
    //
    // `Conflict`, not `InvalidRequest`: `validate_username` above owns
    // everything about the name the caller can fix, and it answers 400. What is
    // left is an organization that already holds the name — the same reading as
    // the taken-username branch of registration, which answers 409.
    let already_taken =
        || crate::error::conflict(format!("organization name '{name}' is already taken"));

    // Taken by an organization or by an account: both answer to `/{name}`,
    // and the account would win that lookup (card_4b0594a02218).
    if crate::namespace::owner_name_is_taken(db, name).await? {
        return Err(already_taken());
    }

    org_ops::create_org(db, name, display_name, description, owner_id, visibility)
        .await
        .map_err(|error| {
            if rg_db::is_unique_violation_anyhow(&error) {
                already_taken()
            } else {
                error
            }
        })
}

/// Get an organization by name.
pub async fn get_org_by_name(
    db: &DatabaseConnection,
    name: &str,
) -> Result<Option<rg_db::entities::organization::Model>> {
    org_ops::get_org_by_name(db, name).await
}

/// List organizations for a user.
pub async fn list_user_orgs(
    db: &DatabaseConnection,
    user_id: i64,
) -> Result<Vec<rg_db::entities::organization::Model>> {
    org_ops::list_user_orgs(db, user_id).await
}

/// Update an organization.
pub async fn update_org(
    db: &DatabaseConnection,
    id: i64,
    display_name: Option<&str>,
    description: Option<&str>,
    visibility: Option<&str>,
) -> Result<rg_db::entities::organization::Model> {
    org_ops::update_org(db, id, display_name, description, visibility)
        .await?
        .ok_or_else(|| crate::error::not_found("organization"))
}

/// Who is asking for an organization to be deleted.
///
/// Deletion used to take the actor as a bare `i64`, which is the same type as
/// the organization id sitting right next to it at every call site — and the
/// admin route did pass the org id into that position. The two answers to "may
/// this go away" are genuinely different rules, so they are spelled as
/// different variants instead of being distinguished by which number was typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgDeleteActor {
    /// A regular user. Allowed only when they own the organization.
    Owner(i64),
    /// An instance administrator. The route-level `InstanceAdmin` gate *is* the
    /// authorization here; owning the organization is deliberately not required,
    /// otherwise an admin could only ever delete their own organizations.
    InstanceAdmin,
}

/// Delete an organization and every repository in its namespace.
///
/// Repository metadata is not linked to `organizations` by a foreign key, and
/// its Git/blob storage cannot participate in the database delete. Retire each
/// repository through the same staged, compensated lifecycle as routed
/// repository deletion before removing the organization row. Each repository
/// is individually atomic; if a later one fails, the organization and every
/// repository not yet retired remain retryable and the request is an error.
pub async fn delete_org(
    db: &DatabaseConnection,
    repo_root: &std::path::Path,
    blob_storage: &dyn crate::blob_storage::BlobStorage,
    oci_storage: &crate::package_registry::oci::storage::OciStorage,
    id: i64,
    actor: OrgDeleteActor,
) -> Result<()> {
    let org = org_ops::get_org(db, id)
        .await?
        .ok_or_else(|| crate::error::not_found("organization"))?;

    match actor {
        OrgDeleteActor::Owner(user_id) if org.owner_id != user_id => {
            return Err(crate::error::forbidden(
                "only the organization owner can delete it",
            ));
        }
        OrgDeleteActor::Owner(_) | OrgDeleteActor::InstanceAdmin => {}
    }

    // Close the namespace before inventorying it. The ownership check above
    // read the org in a statement of its own, so two concurrent deletes both
    // pass it; this claim is the one statement only one of them can win, and
    // the loser gets the same 404 as a request for an organization that was
    // never there rather than a second retirement of the same storage.
    //
    // It is also what makes the inventory below mean something. Without it, a
    // `POST /repos` that resolved this organization a moment earlier could
    // commit its repository row after the inventory had already been taken, and
    // the organization row would then disappear from underneath a live
    // repository whose bytes nothing would ever collect (card_b6dd1fb60659).
    if !org_ops::begin_org_retirement(db, id).await? {
        return Err(crate::error::not_found("organization"));
    }

    if let Err(error) =
        retire_org_repositories(db, repo_root, blob_storage, oci_storage, id, &org.name).await
    {
        // The failure is retryable — the organization and every repository not
        // yet retired are exactly where they were — so the namespace has to
        // reopen with them.
        release_retirement_claim(db, id).await;
        return Err(error);
    }

    // The claim outlives every failure until the row it marks is gone, this one
    // included: a deletion that retired the storage and then could not remove
    // the row would otherwise leave a marked organization with no repositories
    // left, which no retry could ever claim again and no request could reopen.
    match org_ops::delete_org(db, id).await {
        // Nothing left to release — the row the marker lived on is gone.
        Ok(true) => Ok(()),
        Ok(false) => Err(crate::error::not_found("organization")),
        Err(error) => {
            release_retirement_claim(db, id).await;
            Err(error)
        }
    }
}

/// How many times the retirement loop re-reads the organization's repositories
/// before giving up.
///
/// The claim stops new requests from resolving this organization at all, so the
/// only repositories that can still appear are the ones already in flight when
/// it landed. That set is finite and small; a pass that keeps finding more of
/// them means something is creating repositories through a path that ignores
/// the claim, and looping forever would hide that rather than report it.
const MAX_ORG_RETIREMENT_PASSES: usize = 8;

/// Retire every repository of a claimed organization, until a pass finds none.
///
/// One pass is not enough. A repository creation that resolved this
/// organization before the claim landed can still commit its row afterwards, so
/// the inventory is re-read until it comes back empty — at which point no row
/// can appear any more, because every request that could have produced one has
/// either committed (and been retired here) or will find the claim and refuse.
async fn retire_org_repositories(
    db: &DatabaseConnection,
    repo_root: &std::path::Path,
    blob_storage: &dyn crate::blob_storage::BlobStorage,
    oci_storage: &crate::package_registry::oci::storage::OciStorage,
    id: i64,
    org_name: &str,
) -> Result<()> {
    for pass in 0..MAX_ORG_RETIREMENT_PASSES {
        // A database that cannot answer which repositories belong to this
        // organization must not let the organization row disappear while their
        // bytes remain live.
        let repositories = rg_db::ops::repo_ops::list_by_org(db, id).await.context(
            "failed to inventory the organization's repositories — its storage cannot be retired \
             without them",
        )?;
        if repositories.is_empty() {
            return Ok(());
        }
        if pass > 0 {
            tracing::info!(
                org_id = id,
                pass,
                repositories = repositories.len(),
                "organization deletion found repositories created while it was retiring — \
                 retiring them too"
            );
        }
        for repo in &repositories {
            if let Err(error) =
                crate::repo::service::delete_repo(db, repo_root, blob_storage, oci_storage, repo)
                    .await
            {
                tracing::error!(
                    org_id = id,
                    repo_id = repo.id,
                    repo = %repo.name,
                    error = %format!("{error:#}"),
                    "organization deletion stopped: this repository's storage could not be \
                     retired, so the organization row was left in place and the request can be \
                     retried"
                );
                return Err(error.context(format!(
                    "failed to retire repository '{}' (id {}) while deleting organization '{org_name}'",
                    repo.name, repo.id
                )));
            }
        }
    }
    Err(crate::error::conflict(format!(
        "organization '{org_name}' is still gaining repositories after \
         {MAX_ORG_RETIREMENT_PASSES} retirement passes; it was left in place"
    )))
}

/// Reopen a namespace whose retirement could not finish.
///
/// The caller is already returning the original failure, so a failed release
/// can only be reported: the organization stays visible and readable, but no
/// repository can be created in it until the marker is cleared by hand or by a
/// retried deletion that succeeds.
async fn release_retirement_claim(db: &DatabaseConnection, id: i64) {
    if let Err(error) = org_ops::abort_org_retirement(db, id).await {
        tracing::error!(
            org_id = id,
            error = %format!("{error:#}"),
            "failed to reopen an organization whose deletion was aborted — no repository can be \
             created in it until its `deleted_at` marker is cleared"
        );
    }
}

/// Add a member to an organization.
pub async fn add_org_member(
    db: &DatabaseConnection,
    org_id: i64,
    user_id: i64,
    role: &str,
) -> Result<rg_db::entities::organization_member::Model> {
    if role != "owner" && role != "admin" && role != "member" {
        return Err(crate::error::invalid_request(
            "role must be 'owner', 'admin', or 'member'",
        ));
    }
    let member = org_ops::add_org_member(db, org_id, user_id, role).await?;
    // Org membership grants access across all org repos — flush perm cache.
    crate::repo::service::invalidate_perm_cache_all(db);
    Ok(member)
}

/// Remove a member from an organization.
pub async fn remove_org_member(db: &DatabaseConnection, org_id: i64, user_id: i64) -> Result<()> {
    if !org_ops::remove_org_member(db, org_id, user_id).await? {
        return Err(crate::error::not_found("organization member"));
    }
    crate::repo::service::invalidate_perm_cache_all(db);
    Ok(())
}

/// List organization members.
pub async fn list_org_members(
    db: &DatabaseConnection,
    org_id: i64,
) -> Result<Vec<rg_db::entities::organization_member::Model>> {
    org_ops::list_org_members(db, org_id).await
}

/// Check if user is a member of the org.
pub async fn is_org_member(db: &DatabaseConnection, org_id: i64, user_id: i64) -> Result<bool> {
    org_ops::is_org_member(db, org_id, user_id).await
}

/// Find a specific org member.
pub async fn find_org_member(
    db: &DatabaseConnection,
    org_id: i64,
    user_id: i64,
) -> Result<Option<rg_db::entities::organization_member::Model>> {
    org_ops::find_org_member(db, org_id, user_id).await
}

// ── Team service ─────────────────────────────────────────────

/// Create a team.
pub async fn create_team(
    db: &DatabaseConnection,
    org_id: i64,
    name: &str,
    description: Option<&str>,
    permission: &str,
) -> Result<rg_db::entities::team::Model> {
    if permission != "read" && permission != "write" && permission != "admin" {
        return Err(crate::error::invalid_request(
            "permission must be 'read', 'write', or 'admin'",
        ));
    }
    org_ops::create_team(db, org_id, name, description, permission).await
}

/// List teams for an organization.
pub async fn list_org_teams(
    db: &DatabaseConnection,
    org_id: i64,
) -> Result<Vec<rg_db::entities::team::Model>> {
    org_ops::list_org_teams(db, org_id).await
}

/// Get a team by ID.
pub async fn get_team(
    db: &DatabaseConnection,
    id: i64,
) -> Result<Option<rg_db::entities::team::Model>> {
    org_ops::get_team(db, id).await
}

/// Delete a team.
///
/// Mirrors [`delete_org`]: the "no such team" outcome carries
/// [`crate::error::NotFound`] so the HTTP layer can answer `404` to *that* and
/// nothing else — a failed delete stays a 5xx the client retries instead of
/// reading as "the team was already gone".
pub async fn delete_team(db: &DatabaseConnection, id: i64) -> Result<()> {
    if !org_ops::delete_team(db, id).await? {
        return Err(crate::error::not_found("team"));
    }
    crate::repo::service::invalidate_perm_cache_all(db);
    Ok(())
}

/// Add a member to a team.
pub async fn add_team_member(
    db: &DatabaseConnection,
    team_id: i64,
    user_id: i64,
    role: &str,
) -> Result<rg_db::entities::team_member::Model> {
    if role != "member" && role != "maintainer" {
        return Err(crate::error::invalid_request(
            "role must be 'member' or 'maintainer'",
        ));
    }
    let member = org_ops::add_team_member(db, team_id, user_id, role).await?;
    // Team membership can grant repo access — flush perm cache.
    crate::repo::service::invalidate_perm_cache_all(db);
    Ok(member)
}

/// Remove a member from a team.
pub async fn remove_team_member(db: &DatabaseConnection, team_id: i64, user_id: i64) -> Result<()> {
    if !org_ops::remove_team_member(db, team_id, user_id).await? {
        return Err(crate::error::not_found("team member"));
    }
    crate::repo::service::invalidate_perm_cache_all(db);
    Ok(())
}

/// List team members.
pub async fn list_team_members(
    db: &DatabaseConnection,
    team_id: i64,
) -> Result<Vec<rg_db::entities::team_member::Model>> {
    org_ops::list_team_members(db, team_id).await
}

/// card_b6dd1fb60659: an organization's deletion and a repository creation into
/// its namespace are two multi-statement lifecycles over storage no transaction
/// can hold. Between them they must never leave a live repository whose
/// organization is gone, and never leave its bytes where no collector walks.
#[cfg(test)]
mod org_retirement_race_tests {
    use super::*;
    use crate::blob_storage::LocalBlobStorage;
    use crate::package_registry::oci::storage::OciStorage;
    use rg_db::ops::{repo_ops, user_ops};
    use sea_orm::{ConnectOptions, Database};
    use std::sync::Arc;

    async fn setup_db() -> DatabaseConnection {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = Database::connect(options)
            .await
            .expect("connect in-memory database");
        rg_db::run_migrations(&db).await.expect("run migrations");
        db
    }

    /// A throwaway SQLite file, removed with its WAL siblings on drop.
    struct TempDb {
        path: std::path::PathBuf,
    }

    impl Drop for TempDb {
        #[allow(
            clippy::let_underscore_must_use,
            reason = "cleanup must not mask the assertion that failed the test"
        )]
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
            }
        }
    }

    /// A migrated database with more than one pooled connection, so concurrent
    /// tasks really do run their statements against separate connections.
    async fn setup_pooled_db(label: &str) -> (DatabaseConnection, TempDb) {
        let temp = TempDb {
            path: std::env::temp_dir().join(format!(
                "plombir-git-org-race-{label}-{}.db",
                uuid::Uuid::new_v4().simple()
            )),
        };
        let url = format!("sqlite://{}?mode=rwc", temp.path.display());
        let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
            .await
            .expect("connect to throwaway database");
        rg_db::run_migrations(&db).await.expect("run migrations");
        (db, temp)
    }

    fn oci_storage_for(repo_root: &std::path::Path) -> OciStorage {
        OciStorage::from_backend(
            Arc::new(LocalBlobStorage::new(repo_root)),
            repo_root.join("_oci_uploads"),
            Some(repo_root.to_path_buf()),
        )
    }

    /// A user, an organization they own, and the root their storage lives under.
    async fn seed_org(
        db: &DatabaseConnection,
        login: &str,
        org_name: &str,
    ) -> (i64, i64, tempfile::TempDir, std::path::PathBuf) {
        let owner = user_ops::create_user(
            db,
            login,
            &format!("{login}@example.invalid"),
            "unused",
            "Race Owner",
        )
        .await
        .expect("create owner");
        let org = create_org(db, org_name, None, None, owner.id, "public")
            .await
            .expect("create organization");
        let sandbox = tempfile::tempdir().expect("create repository root");
        let repo_root = sandbox.path().join("repos");
        (owner.id, org.id, sandbox, repo_root)
    }

    /// Every repository row of `org_id` whose organization no longer exists.
    async fn orphaned_repositories(db: &DatabaseConnection, org_id: i64) -> Vec<String> {
        if org_ops::get_org(db, org_id)
            .await
            .expect("read organization")
            .is_some()
        {
            return Vec::new();
        }
        repo_ops::list_by_org(db, org_id)
            .await
            .expect("inventory organization repositories")
            .into_iter()
            .map(|repo| repo.name)
            .collect()
    }

    /// The request resolved the organization before the deletion claimed it, so
    /// nothing on the create path could have refused it up front. It commits
    /// its row, finds the claim, and takes both the row and the Git tree back
    /// out — the alternative is a live repository owned by nobody.
    #[tokio::test]
    async fn a_repository_that_commits_after_the_retirement_claim_undoes_itself() {
        let db = setup_db().await;
        let (owner_id, org_id, _sandbox, repo_root) =
            seed_org(&db, "late-create-owner", "late-create-org").await;

        // Exactly what `delete_org` does first, and the only state a create that
        // resolved a moment earlier can still discover.
        assert!(
            org_ops::begin_org_retirement(&db, org_id)
                .await
                .expect("claim the organization"),
            "the first claim on an untouched organization must win"
        );

        let error = crate::repo::service::create_repo(
            &db,
            owner_id,
            "late",
            None,
            false,
            &repo_root,
            Some(org_id),
        )
        .await
        .expect_err("a repository must not be created into a retiring organization");
        assert!(
            format!("{error:#}").contains("being deleted"),
            "the refusal does not say the organization is going away: {error:#}"
        );

        assert!(
            repo_ops::list_by_org(&db, org_id)
                .await
                .expect("inventory organization repositories")
                .is_empty(),
            "the losing create left a live repository row in a retiring organization"
        );
        assert!(
            !repo_root.join("late-create-org/late.git").exists(),
            "the losing create left its Git tree behind"
        );
    }

    /// A second claim is not a second deletion. Two concurrent deletes must not
    /// both walk the same repositories' storage.
    #[tokio::test]
    async fn only_one_deletion_can_claim_an_organization() {
        let db = setup_db().await;
        let (_owner_id, org_id, _sandbox, _repo_root) =
            seed_org(&db, "claim-owner", "claim-org").await;

        assert!(org_ops::begin_org_retirement(&db, org_id).await.unwrap());
        assert!(
            !org_ops::begin_org_retirement(&db, org_id).await.unwrap(),
            "a second deletion claimed an organization already being retired"
        );

        // And the namespace reopens exactly once the claim is released.
        org_ops::abort_org_retirement(&db, org_id).await.unwrap();
        assert!(
            org_ops::begin_org_retirement(&db, org_id).await.unwrap(),
            "a released claim did not reopen the organization"
        );
    }

    /// The claim is what `delete_org` itself takes, not just an op sitting next
    /// to it: an organization another deletion is already retiring must not have
    /// its storage walked a second time, and the loser answers like a name that
    /// is not there.
    #[tokio::test]
    async fn a_deletion_refuses_an_organization_another_deletion_already_claimed() {
        let db = setup_db().await;
        let (owner_id, org_id, _sandbox, repo_root) =
            seed_org(&db, "second-delete-owner", "second-delete-org").await;
        crate::repo::service::create_repo(
            &db,
            owner_id,
            "held",
            None,
            false,
            &repo_root,
            Some(org_id),
        )
        .await
        .expect("create organization repository");

        assert!(org_ops::begin_org_retirement(&db, org_id).await.unwrap());

        let blob_storage = LocalBlobStorage::new(&repo_root);
        let refused = delete_org(
            &db,
            &repo_root,
            &blob_storage,
            &oci_storage_for(&repo_root),
            org_id,
            OrgDeleteActor::Owner(owner_id),
        )
        .await
        .expect_err("a second deletion retired an organization already being retired");
        assert!(
            format!("{refused:#}").contains("organization"),
            "the loser's refusal does not name the organization: {refused:#}"
        );
        assert!(
            repo_root.join("second-delete-org/held.git").exists(),
            "the losing deletion walked the storage the first one owns"
        );
        assert!(
            org_ops::get_org(&db, org_id).await.unwrap().is_some(),
            "the losing deletion removed the organization row the first one claimed"
        );
    }

    /// A deletion that cannot retire a repository leaves everything retryable —
    /// including the namespace. A claim that outlived its failed deletion would
    /// be a namespace no request can create in and no request can reopen.
    #[tokio::test]
    async fn a_failed_deletion_reopens_the_namespace_it_claimed() {
        let db = setup_db().await;
        let (owner_id, org_id, _sandbox, repo_root) =
            seed_org(&db, "reopen-owner", "reopen-org").await;
        crate::repo::service::create_repo(
            &db,
            owner_id,
            "kept",
            None,
            false,
            &repo_root,
            Some(org_id),
        )
        .await
        .expect("create organization repository");

        // A repository whose storage cannot be staged fails the deletion: the
        // blob root is a file, so the OCI upload tree under it cannot be made.
        let broken_root = repo_root.join("_oci_uploads");
        std::fs::create_dir_all(&repo_root).unwrap();
        std::fs::write(&broken_root, b"not a directory").unwrap();
        let blob_storage = LocalBlobStorage::new(&repo_root);
        let failed = delete_org(
            &db,
            &repo_root,
            &blob_storage,
            &OciStorage::from_backend(
                Arc::new(LocalBlobStorage::new(&repo_root)),
                broken_root.join("nested"),
                Some(repo_root.clone()),
            ),
            org_id,
            OrgDeleteActor::Owner(owner_id),
        )
        .await;
        assert!(
            failed.is_err(),
            "a repository whose storage could not be retired was reported as a deleted organization"
        );

        assert!(
            org_ops::org_is_active(&db, org_id)
                .await
                .expect("read organization retirement state"),
            "a failed deletion left the organization claimed and its namespace closed"
        );
        // Which is only meaningful if a create can actually use it again.
        crate::repo::service::create_repo(
            &db,
            owner_id,
            "after-retry",
            None,
            false,
            &repo_root,
            Some(org_id),
        )
        .await
        .expect("the reopened namespace still refuses new repositories");
    }

    /// card_9323f8041cef: the same invariant with the other entrance into the
    /// namespace. A transfer that lands after the deletion's last empty pass is
    /// not undone as cheaply as a creation — its Git tree, blob prefixes and
    /// registry data have already moved — so what is under test is that it
    /// moves all of that back rather than leaving a repository in a namespace
    /// that no longer exists.
    ///
    /// What this guards is the ordering, not the post-commit read: the window
    /// a transfer can land in is the one between the deletion's last empty pass
    /// and its final statement, which is too narrow to hit by racing. The read
    /// itself is pinned deterministically by
    /// `repo::service`'s `a_transfer_that_commits_after_the_destination_is_claimed_moves_back`,
    /// which lands the claim from a trigger on the transfer's own statement.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_concurrent_transfer_and_delete_never_orphan_a_repository() {
        for attempt in 0..6 {
            let (db, _temp) = setup_pooled_db(&format!("org-transfer-{attempt}")).await;
            let login = format!("transfer-race-owner-{attempt}");
            let org_name = format!("transfer-race-org-{attempt}");
            let (owner_id, org_id, _sandbox, repo_root) = seed_org(&db, &login, &org_name).await;
            std::fs::create_dir_all(&repo_root).unwrap();
            crate::repo::service::create_repo(
                &db, owner_id, "moving", None, false, &repo_root, None,
            )
            .await
            .expect("create the repository at its source");

            let transfer_db = db.clone();
            let transfer_root = repo_root.clone();
            let transfer_target = org_name.clone();
            let transfer_login = login.clone();
            let transfer = async move {
                let blob_storage = LocalBlobStorage::new(&transfer_root);
                crate::repo::service::transfer_repo(
                    &transfer_db,
                    owner_id,
                    &transfer_login,
                    "moving",
                    &transfer_target,
                    &transfer_root,
                    &blob_storage,
                    &oci_storage_for(&transfer_root),
                )
                .await
            };
            let delete_db = db.clone();
            let delete_root = repo_root.clone();
            let delete = async move {
                let blob_storage = LocalBlobStorage::new(&delete_root);
                delete_org(
                    &delete_db,
                    &delete_root,
                    &blob_storage,
                    &oci_storage_for(&delete_root),
                    org_id,
                    OrgDeleteActor::Owner(owner_id),
                )
                .await
            };
            let (transferred, deleted) = tokio::join!(transfer, delete);

            let orphans = orphaned_repositories(&db, org_id).await;
            assert!(
                orphans.is_empty(),
                "attempt {attempt}: the organization is gone but these repositories are still \
                 live: {orphans:?} (transfer: {:?}, delete: {:?})",
                transferred
                    .as_ref()
                    .map(|repo| repo.id)
                    .map_err(|e| format!("{e:#}")),
                deleted.as_ref().map_err(|e| format!("{e:#}")),
            );
            // A deletion that reported success owns the whole namespace: a
            // transfer that lost may leave nothing of itself in it.
            if deleted.is_ok() {
                assert!(
                    !repo_root.join(format!("{org_name}/moving.git")).exists(),
                    "attempt {attempt}: the deleted organization left the transferred \
                     repository's Git tree on disk"
                );
            }
            // And a transfer that lost must have put the repository back where
            // it came from, bytes included.
            if transferred.is_err() {
                assert!(
                    repo_root.join(format!("{login}/moving.git")).exists(),
                    "attempt {attempt}: the refused transfer left the source namespace without \
                     its Git tree (delete: {:?})",
                    deleted.as_ref().map_err(|e| format!("{e:#}")),
                );
            }
        }
    }

    /// card_507ff03ec043: the same race with the organization on the *source*
    /// side, which is the harder half. A transfer moves its bytes out of the
    /// namespace before the transaction that rewrites the row, so a deletion
    /// walking that row finds `<org>/<repo>.git` already gone — and a missing
    /// directory is documented to read as the end state a deletion asked for.
    ///
    /// The invariant under real concurrency is the pair of them: never bytes with
    /// no live row naming them, and never a live row whose bytes are gone.
    /// Whichever of the two wins, one of them must have refused.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_concurrent_transfer_out_of_an_organization_and_its_deletion_keep_one_owner() {
        for attempt in 0..6 {
            let (db, _temp) = setup_pooled_db(&format!("org-source-transfer-{attempt}")).await;
            let login = format!("source-race-owner-{attempt}");
            let org_name = format!("source-race-org-{attempt}");
            let (owner_id, org_id, _sandbox, repo_root) = seed_org(&db, &login, &org_name).await;
            std::fs::create_dir_all(&repo_root).unwrap();
            // Lives in the organization and moves out into the owner's personal
            // namespace — the reverse direction of the destination-side test
            // above, so the organization is the namespace being retired *and* the
            // one the bytes are leaving.
            crate::repo::service::create_repo(
                &db,
                owner_id,
                "moving",
                None,
                false,
                &repo_root,
                Some(org_id),
            )
            .await
            .expect("create the repository in the organization");

            let transfer_db = db.clone();
            let transfer_root = repo_root.clone();
            let transfer_source = org_name.clone();
            let transfer_target = login.clone();
            let transfer = async move {
                let blob_storage = LocalBlobStorage::new(&transfer_root);
                crate::repo::service::transfer_repo(
                    &transfer_db,
                    owner_id,
                    &transfer_source,
                    "moving",
                    &transfer_target,
                    &transfer_root,
                    &blob_storage,
                    &oci_storage_for(&transfer_root),
                )
                .await
            };
            let delete_db = db.clone();
            let delete_root = repo_root.clone();
            let delete = async move {
                let blob_storage = LocalBlobStorage::new(&delete_root);
                delete_org(
                    &delete_db,
                    &delete_root,
                    &blob_storage,
                    &oci_storage_for(&delete_root),
                    org_id,
                    OrgDeleteActor::Owner(owner_id),
                )
                .await
            };
            let (transferred, deleted) = tokio::join!(transfer, delete);

            let outcomes = format!(
                "transfer: {:?}, delete: {:?}",
                transferred
                    .as_ref()
                    .map(|repo| repo.id)
                    .map_err(|e| format!("{e:#}")),
                deleted.as_ref().map_err(|e| format!("{e:#}")),
            );
            // Both reporting success is a legitimate outcome and not the bug: it
            // means the transfer won, the repository really did leave the
            // organization, and deleting the now-empty organization was correct.
            // What must never happen is either side consuming what the other
            // owns, so the invariant is on the state, not on who failed.
            let orphans = orphaned_repositories(&db, org_id).await;
            assert!(
                orphans.is_empty(),
                "attempt {attempt}: the organization is gone but these repositories still name \
                 it: {orphans:?} ({outcomes})"
            );

            // Every live row must have its Git tree, and every Git tree a live
            // row: the two halves of "nobody lost these bytes and nobody kept
            // bytes nothing names". The second half is the defect this card was
            // filed for — a deletion that read the transfer's emptied source path
            // as "already gone", soft-deleted the row, and left the bytes live
            // under their new owner with nothing naming them.
            let source_tree = repo_root.join(format!("{org_name}/moving.git"));
            let destination_tree = repo_root.join(format!("{login}/moving.git"));
            let live = repo_ops::find_personal_by_owner_and_name(&db, owner_id, "moving")
                .await
                .expect("read the personal row")
                .or(repo_ops::find_by_org_and_name(&db, org_id, "moving")
                    .await
                    .expect("read the organization row"));
            match live {
                Some(row) if row.org_id.is_some() => assert!(
                    source_tree.exists(),
                    "attempt {attempt}: the repository is still the organization's but its Git \
                     tree is gone ({outcomes})"
                ),
                Some(_) => assert!(
                    destination_tree.exists(),
                    "attempt {attempt}: the repository moved to its new owner without its Git \
                     tree ({outcomes})"
                ),
                None => assert!(
                    !source_tree.exists() && !destination_tree.exists(),
                    "attempt {attempt}: no live row names this repository, but its Git tree is \
                     still on disk ({outcomes})"
                ),
            }
        }
    }

    /// The invariant under real concurrency, both orderings included: whichever
    /// of the two wins, no live repository row may name an organization that is
    /// gone. The create may lose and undo itself, or commit early enough for the
    /// deletion's re-inventory to retire it — never neither.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_concurrent_create_and_delete_never_orphan_a_repository() {
        for attempt in 0..12 {
            // Pooled and file-backed on purpose. `sqlite::memory:` with one
            // connection makes the two tasks take turns on that connection, so
            // the interleaving this test exists for never happens and it would
            // pass against code with no protocol at all.
            let (db, _temp) = setup_pooled_db(&format!("org-race-{attempt}")).await;
            let login = format!("race-owner-{attempt}");
            let org_name = format!("race-org-{attempt}");
            let (owner_id, org_id, _sandbox, repo_root) = seed_org(&db, &login, &org_name).await;
            std::fs::create_dir_all(&repo_root).unwrap();

            let create_db = db.clone();
            let create_root = repo_root.clone();
            let create = async move {
                crate::repo::service::create_repo(
                    &create_db,
                    owner_id,
                    "contested",
                    None,
                    false,
                    &create_root,
                    Some(org_id),
                )
                .await
            };
            let delete_db = db.clone();
            let delete_root = repo_root.clone();
            let delete = async move {
                let blob_storage = LocalBlobStorage::new(&delete_root);
                delete_org(
                    &delete_db,
                    &delete_root,
                    &blob_storage,
                    &oci_storage_for(&delete_root),
                    org_id,
                    OrgDeleteActor::Owner(owner_id),
                )
                .await
            };
            let (created, deleted) = tokio::join!(create, delete);

            let orphans = orphaned_repositories(&db, org_id).await;
            assert!(
                orphans.is_empty(),
                "attempt {attempt}: the organization is gone but these repositories are still \
                 live: {orphans:?} (create: {:?}, delete: {:?})",
                created
                    .as_ref()
                    .map(|repo| repo.id)
                    .map_err(|e| format!("{e:#}")),
                deleted.as_ref().map_err(|e| format!("{e:#}")),
            );
            // A deletion that reported success owns the whole namespace: no Git
            // tree of the losing create may outlive it.
            if deleted.is_ok() {
                assert!(
                    !repo_root.join(format!("{org_name}/contested.git")).exists(),
                    "attempt {attempt}: the deleted organization left the contested repository's \
                     Git tree on disk"
                );
            }
        }
    }
}
