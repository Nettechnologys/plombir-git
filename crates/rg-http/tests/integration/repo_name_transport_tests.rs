//! A repository name the git transport cannot address is refused at the door
//! (card_a9a991c507e4).
//!
//! One path segment below the owner reservations of `namespace_reservation_guard`,
//! and with a worse failure behind it. An owner holding `explore` gets a page
//! nothing can reach; a repository called `foo.git` gets **a clone of somebody
//! else's code**. Both transports strip one `.git` off the last segment —
//! `git_http::strip_git_suffix` and `rg_ssh::parse_repo_owner_name` — so
//! `owner/foo.git` is looked up as `owner/foo`: with no neighbour of that name
//! the clone fails on a repository whose page opens perfectly well, and with one
//! it succeeds against the wrong repository and says nothing.
//!
//! The unit tests beside `rg_core::validate_repo_name` hold the rule. This file
//! holds the two things they cannot see: that the rule is reached through the
//! door a person actually uses, and that the neighbour whose code would have
//! been served is really there while the refusal happens.
//!
//! Repositories created before the rule keep working, with the ambiguity they
//! already had, so the last test is about the other half of the promise — the
//! boot pass that names them instead of leaving them to be found by a bad clone.

use sea_orm::{ActiveModelTrait, NotSet, Set};

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

async fn create_named(base: &str, token: &str, name: &str) -> (u16, String) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await
        .expect("create a repository");
    let status = response.status().as_u16();
    (status, response.text().await.expect("response body"))
}

#[tokio::test]
async fn a_repository_named_after_the_git_suffix_is_refused_beside_the_neighbour_it_would_shadow() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "suffix-owner", "suffix-owner@example.com").await;

    // The neighbour first: this is the repository a clone of `foo.git` would
    // have been answered with, and its presence is what makes the refusal a
    // fix rather than a formality.
    create_repo(&base, &token, "foo").await;

    let (status, body) = create_named(&base, &token, "foo.git").await;
    assert_eq!(
        status, 400,
        "a name the transport strips must be refused, not stored: {body}"
    );
    for expected in [".git", "foo.git", "foo"] {
        assert!(
            body.contains(expected),
            "the refusal must name the rule, what was typed and what it would have reached \
             (missing {expected:?}): {body}"
        );
    }

    // The rule is the suffix, not the letters: taking `git` away from everybody
    // would be a worse trade than the ambiguity it prevents.
    let (status, body) = create_named(&base, &token, "git").await;
    assert_eq!(status, 201, "`git` is an ordinary repository name: {body}");
    let (status, body) = create_named(&base, &token, "foo.git.example").await;
    assert_eq!(
        status, 201,
        "`.git` in the middle addresses nothing ambiguous: {body}"
    );
}

#[tokio::test]
async fn a_repository_named_as_a_bare_path_segment_is_refused() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "dot-owner", "dot-owner@example.com").await;

    for name in [".", ".."] {
        let (status, body) = create_named(&base, &token, name).await;
        assert_eq!(
            status, 400,
            "`{name}` addresses a directory, not a repository, and must be refused: {body}"
        );
    }
    let (status, body) = create_named(&base, &token, "my.project").await;
    assert_eq!(status, 201, "a dot inside a name is ordinary: {body}");
}

/// The other half of the promise: the rule closes the door, and the boot pass
/// says who walked through it before there was one.
///
/// The row is written straight to the database because that is the only way it
/// can exist now — which is the point. `fork_repo` and `transfer_repo` carry an
/// existing name forward, and an instance that ran before this rule can hold
/// such a name; neither is a reason to make the operator discover it through a
/// clone that returns the wrong code.
#[tokio::test]
async fn repositories_that_predate_the_rule_are_named_at_boot_rather_than_left_to_be_found() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, owner_id) = register_full(&base, "legacy-owner", "legacy-owner@example.com").await;
    create_repo(&base, &token, "plain").await;

    let now = chrono::Utc::now();
    for name in ["legacy.git", "SHOUTED.GIT", ".."] {
        rg_db::entities::repository::ActiveModel {
            id: NotSet,
            owner_id: Set(owner_id),
            name: Set(name.to_string()),
            description: Set(None),
            is_private: Set(false),
            default_branch: Set("main".into()),
            fork_id: Set(None),
            stars_count: Set(0),
            forks_count: Set(0),
            org_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            origin_repo_id: Set(None),
        }
        .insert(&db)
        .await
        .unwrap_or_else(|error| panic!("seed the pre-rule repository `{name}`: {error}"));
    }

    let found = rg_db::ops::repo_ops::list_names_the_transport_cannot_address(&db)
        .await
        .expect("the boot pass reads the table");
    let mut names: Vec<String> = found
        .into_iter()
        .map(|(owner, name)| format!("{owner}/{name}"))
        .collect();
    names.sort();

    assert_eq!(
        names,
        vec![
            "legacy-owner/..".to_string(),
            "legacy-owner/SHOUTED.GIT".to_string(),
            "legacy-owner/legacy.git".to_string(),
        ],
        "the boot pass has to find every shape the rule refuses — including the upper-cased one, \
         which `LIKE` answers differently on SQLite and Postgres — and nothing else"
    );
}

/// The import derives its target name from the source URL, and a mirror URL is
/// exactly the shape that produces one: `git clone --bare` of
/// `https://host/o/foo.git` gives a directory called `foo.git`.
///
/// The derived name is safe — `StartImportRequest::resolved_target_name` trims
/// the suffix off the URL — but a name the client *types* is not, and it used to
/// be refused only inside the worker, long after the `POST` had answered `201`.
/// A refusal nobody is watching is not a refusal.
#[tokio::test]
async fn an_import_named_after_a_mirror_directory_is_refused_by_the_request_that_starts_it() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "import-owner", "import-owner@example.com").await;
    let client = reqwest::Client::new();

    let start = |target_name: &'static str| {
        let client = client.clone();
        let base = base.clone();
        let token = token.clone();
        async move {
            let response = client
                .post(format!("{base}/api/v1/imports"))
                .bearer_auth(&token)
                .json(&serde_json::json!({
                    "platform": "git",
                    // Never reached: these tests are about the answer to the POST.
                    "source_url": "https://example.invalid/octo/widgets.git",
                    "target_owner": "import-owner",
                    "target_name": target_name,
                }))
                .send()
                .await
                .expect("start an import");
            (
                response.status().as_u16(),
                response.text().await.expect("response body"),
            )
        }
    };

    let (status, body) = start("widgets.git").await;
    assert_eq!(
        status, 400,
        "a target name the transport strips must be refused where the client can see it: {body}"
    );
    assert!(
        body.contains("widgets.git"),
        "the refusal must name what was asked for: {body}"
    );

    // The name the *derivation* produces has the suffix trimmed off already, so
    // the ordinary mirror URL keeps working — the rule must not cost that.
    let (status, body) = start("widgets").await;
    assert_eq!(
        status, 201,
        "an ordinary import target must still be accepted: {body}"
    );
}

/// The homograph the owner segment was tightened against, one segment down
/// (card_9c82c2072a6f).
///
/// `validate_username_shape` is ASCII-only because a Cyrillic lookalike of an
/// account name is "a homograph waiting to happen in a namespace shared with
/// usernames" — its own words. The segment below it kept the unicode predicate,
/// so `alice/раyment` could sit next to `alice/payment` and be linked to as if
/// it were the same repository. The neighbour is created first here for the
/// same reason as in the `.git` test: the refusal has to happen while the
/// repository it would be mistaken for really exists.
#[tokio::test]
async fn a_repository_name_that_only_looks_like_its_neighbour_is_refused() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "homograph-owner", "homograph-owner@example.com").await;

    create_repo(&base, &token, "payment").await;

    // Cyrillic `р` + `а`, then ASCII `yment`.
    let lookalike = "\u{0440}\u{0430}yment";
    let (status, body) = create_named(&base, &token, lookalike).await;
    assert_eq!(
        status, 400,
        "a name that renders as an existing repository's must be refused, not stored: {body}"
    );
    assert!(
        body.contains("invalid character"),
        "the refusal must say which character broke the rule: {body}"
    );

    // And the rule must not have cost the ASCII name it protects.
    let (status, body) = create_named(&base, &token, "payment-v2").await;
    assert_eq!(
        status, 201,
        "an ordinary name must still be accepted: {body}"
    );
}

/// The other half of the promise, as with the `.git` rule: a repository created
/// before the rule keeps working, so the boot pass is what names it.
#[tokio::test]
async fn repositories_named_outside_ascii_before_the_rule_are_named_at_boot() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, owner_id) =
        register_full(&base, "legacy-unicode", "legacy-unicode@example.com").await;
    create_repo(&base, &token, "payment").await;

    let now = chrono::Utc::now();
    for name in ["\u{0440}\u{0430}yment", "caf\u{e9}"] {
        rg_db::entities::repository::ActiveModel {
            id: NotSet,
            owner_id: Set(owner_id),
            name: Set(name.to_string()),
            description: Set(None),
            is_private: Set(false),
            default_branch: Set("main".into()),
            fork_id: Set(None),
            stars_count: Set(0),
            forks_count: Set(0),
            org_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            origin_repo_id: Set(None),
        }
        .insert(&db)
        .await
        .unwrap_or_else(|error| panic!("seed the pre-rule repository `{name}`: {error}"));
    }

    let found = rg_db::ops::repo_ops::list_non_ascii_names(&db)
        .await
        .expect("the boot pass reads the table");
    let mut names: Vec<String> = found
        .into_iter()
        .map(|(owner, name)| format!("{owner}/{name}"))
        .collect();
    names.sort();

    assert_eq!(
        names,
        vec![
            "legacy-unicode/caf\u{e9}".to_string(),
            "legacy-unicode/\u{0440}\u{0430}yment".to_string(),
        ],
        "the boot pass has to find every repository the rule would now refuse — and none of the \
         ASCII ones beside them"
    );
}
