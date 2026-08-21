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
    "login",
    "metrics",
    "notifications",
    "orgs",
    "register",
    "reset-password",
    "search",
    "settings",
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
