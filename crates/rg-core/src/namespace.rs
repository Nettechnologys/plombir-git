//! The first path segment, and who is allowed to hold it.
//!
//! An owner — a person or an organization — is addressed by the first segment
//! of the URL: `/{owner}`, `/{owner}/{repo}`. The application's own pages live
//! in that same segment (`/dashboard`, `/explore`, `/settings`, …) and so do
//! the server's own endpoints (`/api`, `/git`, `/v2`, `/health`, `/metrics`,
//! `/api-docs`). Nothing used to compare the two: `validate_username` checked
//! length, first character, alphabet and path traversal, and had no opinion on
//! whether the name it was passing was a segment already spoken for.
//!
//! So registering `explore` succeeded and the account's profile was unreachable
//! for good — SvelteKit matches its static route before `[owner]`, and the
//! server matches a mounted route before the SPA fallback. The operation
//! reported success and produced something else, and the only person who ever
//! found out was the account holder.
//!
//! ## Why the list is written here and proved elsewhere
//!
//! [`RESERVED_SEGMENTS`] is one list, and it is not allowed to be a second
//! hand-kept copy of the route tree: `rg-http`'s
//! `namespace_reservation_guard` walks the real `RouteFact` table the running
//! build produced *and* the top level of `web/src/routes/`, and requires the
//! two sides to agree with this list exactly. A page added tomorrow that this
//! list does not know about turns that guard red; a name reserved here that
//! nothing claims turns it red too, so the list cannot quietly grow into a ban
//! list either.
//!
//! It lives in this crate rather than in `rg-http` because the check belongs to
//! [`crate::validate_username`], which is the single door every account name
//! comes through — registration, organization creation, and the SSO and LDAP
//! paths that *generate* a name without a human ever seeing it.

/// Every first path segment this application already answers for.
///
/// Sorted, lowercase, and derived from the route tree rather than invented —
/// see the module header for what keeps it that way. Segments that no username
/// could collide with anyway (`v2` is under the three-character minimum) are
/// deliberately absent: this list is what `validate_username` refuses, and
/// refusing a name that is already impossible would only make the rule harder
/// to read.
pub const RESERVED_SEGMENTS: &[&str] = &[
    "admin",
    "api",
    "api-docs",
    "dashboard",
    "explore",
    "forgot-password",
    "git",
    "health",
    "help",
    "imports",
    "livez",
    "login",
    "metrics",
    "notifications",
    "orgs",
    "readyz",
    "register",
    "reset-password",
    "search",
    "settings",
    "verify-email",
];

/// Whether `name` is a segment the application answers for itself.
///
/// Case-insensitive, although neither router is: `/Explore` matches no static
/// route and really would reach an account named `Explore`. The reason to
/// refuse it anyway is the other half of the same fact — `users.username`
/// carries no `NOCASE` collation, so `explore` and `Explore` are two different
/// accounts, and an account whose URL differs from one of this application's
/// own pages only in case is a distinction no reader makes at a glance. One
/// name is a cheap price for not having that.
pub fn is_reserved_segment(name: &str) -> bool {
    RESERVED_SEGMENTS
        .iter()
        .any(|segment| name.eq_ignore_ascii_case(segment))
}

/// Whether an account or an organization already holds `name`.
///
/// Accounts and organizations live in two tables but answer to one URL segment
/// (`/{owner}`), and `resolve_owner` reads the account first. A door that asks
/// only its own table therefore hands out a name the other one already holds:
/// a stranger registering `acme` next to the organization `acme` took over
/// everything that resolves the owner by name — new repositories "in acme",
/// the `/acme/...` links — and an organization named after an account did the
/// same to the account (card_4b0594a02218). Every door that mints an owner
/// name asks this instead: registration, organization creation, bots, and the
/// SSO and LDAP paths that generate a name without a human seeing it.
///
/// Exact match, like the two lookups it combines. This is the friendly first
/// answer, not the guarantee: two concurrent creates of one name across the
/// two tables can both pass it. The guarantee is `owner_names`, the database's
/// own unique key over both tables, written by triggers in the same statement
/// as the row — the loser of that race fails its insert with an ordinary
/// unique violation, which every door already answers as "taken"
/// (`m20261009_000001_owner_names_unique`, card_8f3f821705f2).
pub async fn owner_name_is_taken(
    db: &rg_db::DatabaseConnection,
    name: &str,
) -> anyhow::Result<bool> {
    if rg_db::ops::user_ops::find_by_username(db, name)
        .await?
        .is_some()
    {
        return Ok(true);
    }
    Ok(rg_db::ops::org_ops::get_org_by_name(db, name)
        .await?
        .is_some())
}

/// Name every organization whose name an account holds too.
///
/// A sibling of [`report_owners_holding_reserved_names`]: the doors refuse new
/// collisions, this says which ones came through first. The organization is the
/// one that lost — `/{name}` resolves to the account — and which of the two to
/// rename is the operator's call, so a warning and a startup that continues.
pub async fn report_names_held_by_an_account_and_an_organization(db: &rg_db::DatabaseConnection) {
    let shared = match rg_db::ops::org_ops::list_names_held_by_an_account_too(db).await {
        Ok(shared) => shared,
        // Silence here reads exactly like "none", which is the answer the
        // operator would act on.
        Err(error) => {
            tracing::warn!(
                error = %format!("{error:#}"),
                "could not check whether any organization shares its name with an account"
            );
            return;
        }
    };
    if shared.is_empty() {
        return;
    }
    tracing::warn!(
        names = shared.join(", "),
        "these names are held by an account and by an organization at once; `/{{name}}` and \
         everything that resolves an owner by name reach the account, so the organization is \
         unreachable by its name; they predate the rule and renaming one of the two is a \
         decision for you, not for the server"
    );
}

/// Name every account and organization already holding a reserved segment.
///
/// The reservation closes the door; it says nothing about who walked through it
/// before there was one. Such an owner is not broken data — the account works,
/// it can push, it can be a collaborator — but its `/{owner}` page is answered
/// by the application's own route and always will be, so the only fixes are a
/// rename or nothing, and both are the operator's call to make, not a boot
/// pass's. Hence a warning that names them and a startup that continues.
///
/// One point lookup per reserved segment, on a unique index, once per boot: the
/// alternative is a query nobody remembers to run, which is how the criterion
/// "existing holders were found and a decision was taken" turns into "nobody
/// looked".
pub async fn report_owners_holding_reserved_names(db: &rg_db::DatabaseConnection) {
    let (accounts, organizations) = find_owners_holding_reserved_names(db).await;
    if accounts.is_empty() && organizations.is_empty() {
        return;
    }
    tracing::warn!(
        accounts = accounts.join(", "),
        organizations = organizations.join(", "),
        "these owners hold a name this application answers for itself, so their `/{{owner}}` page \
         is unreachable; they predate the reservation and renaming them is a decision for you, \
         not for the server"
    );
}

/// Name every repository the git transport already cannot address.
///
/// The sibling of [`report_owners_holding_reserved_names`], one path segment
/// down and with a worse failure behind it. An owner holding `explore` gets an
/// unreachable page; a repository called `foo.git` gets a *clone of somebody
/// else's code* when a `foo` exists next to it, because both transports strip
/// the suffix before they look the name up. [`crate::validate_repo_name`] closes
/// the door on new ones; this says who walked through it first.
///
/// A warning and a startup that continues, for the same reason as above: the
/// only fixes are a rename or nothing, and both are the operator's call.
pub async fn report_repositories_the_transport_cannot_address(db: &rg_db::DatabaseConnection) {
    let affected = match rg_db::ops::repo_ops::list_names_the_transport_cannot_address(db).await {
        Ok(affected) => affected,
        // Silence here reads exactly like "none", which is the answer the
        // operator would act on.
        Err(error) => {
            tracing::warn!(
                error = %format!("{error:#}"),
                "could not check whether any repository carries a name the git transport cannot \
                 address"
            );
            return;
        }
    };
    if affected.is_empty() {
        return;
    }
    tracing::warn!(
        repositories = affected
            .iter()
            .map(|(owner, name)| format!("{owner}/{name}"))
            .collect::<Vec<_>>()
            .join(", "),
        "these repositories carry a name the git transport strips or resolves away, so cloning \
         them reaches a different repository or nothing at all; they predate the rule and \
         renaming them is a decision for you, not for the server"
    );
}

/// Name every repository whose name is not ASCII.
///
/// The third pass of the same family, and the quietest failure of the three. An
/// owner holding `explore` gets an unreachable page; a repository called
/// `foo.git` gets a clone of somebody else's code; a repository called
/// `раyment` gets *read as* `payment` by everyone who sees a link to it, and
/// nothing anywhere reports a problem. [`crate::validate_repo_name`] refuses
/// new ones — this says which ones were already there when the rule arrived.
///
/// A warning and a startup that continues, as with its two siblings: renaming
/// somebody's repository is the operator's call, not the server's.
pub async fn report_repositories_with_names_that_are_not_ascii(db: &rg_db::DatabaseConnection) {
    let affected = match rg_db::ops::repo_ops::list_non_ascii_names(db).await {
        Ok(affected) => affected,
        // Silence here reads exactly like "none", which is the answer the
        // operator would act on.
        Err(error) => {
            tracing::warn!(
                error = %format!("{error:#}"),
                "could not check whether any repository carries a name that is not ASCII"
            );
            return;
        }
    };
    if affected.is_empty() {
        return;
    }
    tracing::warn!(
        repositories = affected
            .iter()
            .map(|(owner, name)| format!("{owner}/{name}"))
            .collect::<Vec<_>>()
            .join(", "),
        "these repositories carry a name outside ASCII, so another name that merely looks the \
         same is indistinguishable from theirs in any link; they predate the rule and renaming \
         them is a decision for you, not for the server"
    );
}

/// The two lists the warning above is made of, separated so the finding can be
/// asserted on rather than read out of a log line.
async fn find_owners_holding_reserved_names(
    db: &rg_db::DatabaseConnection,
) -> (Vec<String>, Vec<String>) {
    let mut accounts: Vec<String> = Vec::new();
    let mut organizations: Vec<String> = Vec::new();

    for segment in RESERVED_SEGMENTS {
        match rg_db::ops::user_ops::find_by_username(db, segment).await {
            Ok(Some(user)) => accounts.push(user.username),
            Ok(None) => {}
            // A failed lookup must not be silence: silence here reads exactly
            // like "nobody holds one", which is the answer the operator will
            // act on.
            Err(error) => tracing::warn!(
                segment,
                error = %error,
                "could not check whether an account holds this reserved name"
            ),
        }
        match rg_db::ops::org_ops::get_org_by_name(db, segment).await {
            Ok(Some(org)) => organizations.push(org.name),
            Ok(None) => {}
            Err(error) => tracing::warn!(
                segment,
                error = %error,
                "could not check whether an organization holds this reserved name"
            ),
        }
    }

    (accounts, organizations)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reserved_segment_is_recognized_whatever_its_case() {
        assert!(is_reserved_segment("explore"));
        assert!(is_reserved_segment("Explore"));
        assert!(is_reserved_segment("API-DOCS"));
    }

    /// The rule is the whole segment, not a prefix of it: `explorer` is a
    /// perfectly reachable account, and refusing it would take a valid name
    /// away to solve a collision that does not exist.
    #[test]
    fn a_name_that_merely_starts_with_one_is_free() {
        assert!(!is_reserved_segment("explorer"));
        assert!(!is_reserved_segment("admins"));
        assert!(!is_reserved_segment("git-mirror"));
    }

    /// An owner registered before the reservation existed is still there, and
    /// the boot pass has to name it — a silent start reads exactly like "nobody
    /// holds one", which is the answer the operator would act on.
    #[tokio::test]
    async fn an_owner_registered_before_the_reservation_is_named() {
        let db = crate::test_support::migrated_memory_database().await;
        // Straight through the ops, which is how such a row got there: the
        // validator that would refuse the name now did not exist then.
        rg_db::ops::user_ops::create_user(&db, "explore", "explore@example.invalid", "", "Explore")
            .await
            .expect("seed an account holding a reserved name");
        rg_db::ops::user_ops::create_user(
            &db,
            "explorer",
            "explorer@example.invalid",
            "",
            "Explorer",
        )
        .await
        .expect("seed an account holding a free name");

        let (accounts, organizations) = find_owners_holding_reserved_names(&db).await;
        assert_eq!(
            accounts,
            vec!["explore".to_string()],
            "the pass named the wrong accounts"
        );
        assert!(
            organizations.is_empty(),
            "no organization was seeded: {organizations:?}"
        );
    }

    /// card_4b0594a02218: the doors now ask both tables, and the boot pass
    /// names the collisions that came through before they did — only those.
    #[tokio::test]
    async fn a_name_held_by_an_account_and_an_organization_is_found_and_named() {
        let db = crate::test_support::migrated_memory_database().await;
        let founder = rg_db::ops::user_ops::create_user(
            &db,
            "founder",
            "founder@example.invalid",
            "",
            "Founder",
        )
        .await
        .expect("seed the organizations' owner");
        // Straight through the ops, the way such a pair got in before.
        rg_db::ops::user_ops::create_user(&db, "acme", "acme@example.invalid", "", "Acme")
            .await
            .expect("seed the account that shadows the organization");
        // `owner_names` refuses that pair now (card_8f3f821705f2), so the
        // database is put into the state an upgraded one with such a pair is
        // in: the account holds the name in `owner_names`, the organization
        // shares it in its own table and nowhere else.
        use sea_orm::ConnectionTrait;
        db.execute_unprepared("DELETE FROM owner_names WHERE name = 'acme'")
            .await
            .expect("release the name for the legacy seed");
        for org in ["acme", "lonely"] {
            rg_db::ops::org_ops::create_org(&db, org, None, None, founder.id, "public")
                .await
                .expect("seed an organization");
        }
        db.execute_unprepared(
            "UPDATE owner_names SET kind = 'user', \
             owner_id = (SELECT id FROM users WHERE username = 'acme') WHERE name = 'acme'",
        )
        .await
        .expect("give the name back to the account, as the backfill does");

        assert_eq!(
            rg_db::ops::org_ops::list_names_held_by_an_account_too(&db)
                .await
                .expect("list collisions"),
            vec!["acme".to_string()]
        );
        for (name, taken) in [
            ("acme", true),
            ("lonely", true),
            ("founder", true),
            ("free", false),
        ] {
            assert_eq!(
                owner_name_is_taken(&db, name)
                    .await
                    .expect("look the name up"),
                taken,
                "{name}"
            );
        }
    }

    /// The list is what the guard compares against, so a duplicate or an
    /// out-of-order entry is a sign it was edited by hand instead of from the
    /// tree.
    #[test]
    fn the_list_is_sorted_and_free_of_duplicates() {
        let mut sorted = RESERVED_SEGMENTS.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted, RESERVED_SEGMENTS.to_vec());
    }
}
