//! card_5fb3a682a256: a paginating listing must sort on a **total** order.
//!
//! `LIMIT/OFFSET` cuts a window out of a sequence. If the `ORDER BY` leaves
//! ties — and `created_at` / `updated_at` do, at second precision, whenever an
//! import or a bot files a batch — the engine is free to resolve them however
//! it happens to scan, and two neighbouring requests may resolve them
//! differently. Then one row lands on two pages and the row beside it lands on
//! none, while the response is `200` and `total` is correct. Nothing about the
//! answer says a row was withheld.
//!
//! Two kinds of check live here, because one alone would not hold the line:
//!
//! - The **walks** pin the behaviour on rows deliberately given one identical
//!   timestamp: every row exactly once across the pages, and in the order the
//!   tiebreaker fixes. SQLite resolves such a tie in `rowid` order, which is
//!   *ascending* `id` — so a descending listing that lost its tiebreaker fails
//!   the order assertion here rather than passing by luck.
//! - The **source guard** covers the functions no walk was written for, and
//!   every function added later: any op that reaches for `.offset(` or
//!   `.paginate(` must name the primary key in its `ORDER BY`.
//!
//! Out of the guard's scope by construction: `repo_watch_ops` pages by keyset
//! (`id > after_id`, already total), and `issue_label_ops` builds its page in
//! raw SQL ordered by `il.issue_id` (already unique). Neither uses the SeaORM
//! offset builders the guard looks for.

use std::collections::BTreeSet;

use rg_db::entities::{audit_log, issue, repository};
use rg_db::sea_orm::{DatabaseConnection, NotSet, Set};

/// Rows per page. Small on purpose: the defect only shows once a tie is cut in
/// half by a page boundary, so the boundary has to land inside the tie.
const PER_PAGE: u64 = 2;

/// How many rows share one timestamp. Enough to straddle several boundaries.
const TIED_ROWS: usize = 7;

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "forgekeep-pagination-{label}-{}.db",
            uuid::Uuid::new_v4().simple()
        ));
        Self { path }
    }

    fn url(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path.display())
    }
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

async fn migrated_db(name: &str) -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new(name);
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    (db, temp)
}

/// The assertion every walk makes: the pages partition the listing.
///
/// `expected` is the full set of ids the predicate matches; `walked` is what
/// the page walk actually handed out, in order. A duplicate, a hole and a
/// disagreement with `total` are named separately because they are separate
/// symptoms of the same missing tiebreaker.
fn assert_pages_partition(walked: &[i64], expected: &BTreeSet<i64>, total: i64, what: &str) {
    let seen: BTreeSet<i64> = walked.iter().copied().collect();

    assert_eq!(
        walked.len(),
        seen.len(),
        "{what}: the page walk served some row twice — {walked:?}"
    );
    assert_eq!(
        seen, *expected,
        "{what}: the walked rows are not the rows that match the predicate"
    );
    assert_eq!(
        total as usize,
        expected.len(),
        "{what}: `total` counts a different set than the pages are cut from"
    );
}

/// Strictly monotonic ids are what proves the tie was broken by the primary
/// key rather than by whatever order the storage engine felt like scanning.
fn assert_ids_strictly_ordered(walked: &[i64], descending: bool, what: &str) {
    for pair in walked.windows(2) {
        let ordered = if descending {
            pair[0] > pair[1]
        } else {
            pair[0] < pair[1]
        };
        let direction = if descending {
            "descending"
        } else {
            "ascending"
        };
        assert!(
            ordered,
            "{what}: rows sharing a timestamp came back out of {direction} id order \
             ({} then {}) — the sort key alone does not order them, so the page \
             boundaries are not stable: {walked:?}",
            pair[0], pair[1]
        );
    }
}

/// An account and a repository for the rows to hang off.
async fn owner_and_repo(
    db: &DatabaseConnection,
    at: chrono::DateTime<chrono::Utc>,
) -> (i64, repository::Model) {
    let user = rg_db::ops::user_ops::create_user(db, "dana", "dana@example.com", "", "Dana")
        .await
        .expect("create the account the rows hang off");
    let repo = repo_row(db, user.id, "forge", None, at).await;
    (user.id, repo)
}

async fn repo_row(
    db: &DatabaseConnection,
    owner_id: i64,
    name: &str,
    origin_repo_id: Option<i64>,
    at: chrono::DateTime<chrono::Utc>,
) -> repository::Model {
    rg_db::ops::repo_ops::create(
        db,
        repository::ActiveModel {
            id: NotSet,
            owner_id: Set(owner_id),
            name: Set(name.to_string()),
            description: Set(None),
            is_private: Set(false),
            default_branch: Set("main".to_string()),
            fork_id: Set(None),
            stars_count: Set(0),
            forks_count: Set(0),
            org_id: Set(None),
            created_at: Set(at),
            updated_at: Set(at),
            deleted_at: Set(None),
            origin_repo_id: Set(origin_repo_id),
        },
    )
    .await
    .expect("create repository")
}

/// `issue_ops::list_by_repo_paginated` — the representative of the
/// `.offset()/.limit()` descending listings (issues, PRs, notifications,
/// pipelines, releases, stars, time entries).
#[tokio::test]
async fn issue_pages_partition_a_batch_filed_in_one_instant() {
    let (db, _temp) = migrated_db("issues").await;
    let at = chrono::Utc::now();
    let (user_id, repo) = owner_and_repo(&db, at).await;

    // One timestamp for the whole batch: this is what an import writes when the
    // upstream API reports `created_at` at second precision.
    let mut expected = BTreeSet::new();
    for number in 1..=TIED_ROWS as i64 {
        let created = rg_db::ops::issue_ops::create(
            &db,
            issue::ActiveModel {
                id: NotSet,
                repo_id: Set(repo.id),
                number: Set(number),
                title: Set(format!("imported issue {number}")),
                body: Set(None),
                state: Set("open".to_string()),
                author_id: Set(user_id),
                assignee_id: Set(None),
                milestone_id: Set(None),
                created_at: Set(at),
                updated_at: Set(at),
                closed_at: Set(None),
                deleted_at: Set(None),
            },
        )
        .await
        .expect("create issue");
        assert!(expected.insert(created.id), "issue ids are unique");
    }

    let mut walked = Vec::new();
    let mut total = 0;
    for page in 0..TIED_ROWS as u64 {
        let (rows, count) = rg_db::ops::issue_ops::list_by_repo_paginated(
            &db,
            repo.id,
            None,
            page * PER_PAGE,
            PER_PAGE,
        )
        .await
        .expect("walk a page of issues");
        total = count;
        walked.extend(rows.into_iter().map(|row| row.id));
    }

    assert_pages_partition(&walked, &expected, total, "issues");
    assert_ids_strictly_ordered(&walked, true, "issues");
}

/// `repo_ops::list_forks` — the ascending half of the same class. A fork storm
/// (or a namespace migration) writes many rows in one instant.
#[tokio::test]
async fn fork_pages_partition_forks_created_in_one_instant() {
    let (db, _temp) = migrated_db("forks").await;
    let at = chrono::Utc::now();
    let (user_id, origin) = owner_and_repo(&db, at).await;

    let mut expected = BTreeSet::new();
    for n in 0..TIED_ROWS {
        let fork = repo_row(
            &db,
            user_id,
            &format!("forge-fork-{n}"),
            Some(origin.id),
            at,
        )
        .await;
        assert!(expected.insert(fork.id), "fork ids are unique");
    }

    let mut walked = Vec::new();
    let mut total = 0;
    for page in 0..TIED_ROWS as u64 {
        let (rows, count) =
            rg_db::ops::repo_ops::list_forks(&db, origin.id, page * PER_PAGE, PER_PAGE)
                .await
                .expect("walk a page of forks");
        total = count;
        walked.extend(rows.into_iter().map(|row| row.id));
    }

    assert_pages_partition(&walked, &expected, total, "forks");
    assert_ids_strictly_ordered(&walked, false, "forks");
}

/// `repo_ops::list_public_paginated` — the `/explore` feed, ordered by
/// `updated_at`, the loosest key of the set: any bulk touch moves a whole batch
/// of repositories to the same instant.
#[tokio::test]
async fn explore_pages_partition_repos_touched_in_one_instant() {
    let (db, _temp) = migrated_db("explore").await;
    let at = chrono::Utc::now();
    let (user_id, first) = owner_and_repo(&db, at).await;

    let mut expected = BTreeSet::new();
    assert!(expected.insert(first.id), "repo ids are unique");
    for n in 1..TIED_ROWS {
        let repo = repo_row(&db, user_id, &format!("forge-{n}"), None, at).await;
        assert!(expected.insert(repo.id), "repo ids are unique");
    }

    let mut walked = Vec::new();
    let mut total = 0;
    for page in 0..TIED_ROWS as u64 {
        let (rows, count) =
            rg_db::ops::repo_ops::list_public_paginated(&db, page * PER_PAGE, PER_PAGE)
                .await
                .expect("walk a page of public repos");
        total = count;
        walked.extend(rows.into_iter().map(|row| row.id));
    }

    assert_pages_partition(&walked, &expected, total, "explore");
    assert_ids_strictly_ordered(&walked, true, "explore");
}

/// `audit_log_ops::list_paginated` — the representative of the SeaORM
/// `Paginator` half (audit logs, login logs, orgs, users). The paginator
/// applies the same `LIMIT/OFFSET` under a friendlier name, so it inherits the
/// same requirement on the sort key.
///
/// Audit entries are the sharpest case: a single request that touches several
/// resources writes its rows within one clock tick by construction, so a
/// compliance export walking the pages is exactly where a dropped row matters.
#[tokio::test]
async fn audit_log_pages_partition_entries_written_in_one_instant() {
    let (db, _temp) = migrated_db("audit").await;
    let at = chrono::Utc::now();

    let mut expected = BTreeSet::new();
    for n in 0..TIED_ROWS {
        let row = rg_db::ops::audit_log_ops::insert(
            &db,
            audit_log::ActiveModel {
                id: NotSet,
                user_id: Set(None),
                username: Set(Some("dana".to_string())),
                action: Set(format!("repo.update.{n}")),
                resource_type: Set(Some("repo".to_string())),
                resource_id: Set(None),
                resource_name: Set(None),
                ip_address: Set(None),
                user_agent: Set(None),
                details: Set(None),
                created_at: Set(at),
            },
        )
        .await
        .expect("insert audit entry");
        assert!(expected.insert(row.id), "audit ids are unique");
    }

    let mut walked = Vec::new();
    let mut total = 0;
    for page in 0..TIED_ROWS as u64 {
        let (rows, count) = rg_db::ops::audit_log_ops::list_paginated(
            &db, page, PER_PAGE, None, None, None, None, None,
        )
        .await
        .expect("walk a page of audit entries");
        total = count as i64;
        walked.extend(rows.into_iter().map(|row| row.id));
    }

    assert_pages_partition(&walked, &expected, total, "audit log");
    assert_ids_strictly_ordered(&walked, true, "audit log");
}

/// Drop the doc comment and attributes that belong to the *next* item.
///
/// Splitting a file on `fn` leaves each chunk ending in the doc block of the
/// function that follows it, so both source scans below would otherwise read
/// the next function's prose as this one's code. That misattributes in both
/// directions: prose mentioning `.offset(` makes an innocent neighbour look
/// like a paginating op, and prose mentioning `order_by_…Column::Id` would let
/// a real offender pass. A function body ends at its `}`, so stripping the
/// trailing run of blank, comment and attribute lines is exactly the cut.
fn without_the_next_item_s_doc_block(body: &str) -> &str {
    let mut end = body.len();
    for line in body.lines().rev() {
        let trimmed = line.trim_start();
        if trimmed.is_empty()
            || trimmed.starts_with("//")
            || trimmed.starts_with("#[")
            || trimmed.starts_with("#!")
        {
            end -= line.len();
            // Every line but the last carries a `\n` that goes with it.
            end = end.saturating_sub(usize::from(end > 0));
        } else {
            break;
        }
    }
    &body[..end]
}

/// Every paginating op in `rg-db`, including the ones written after this test.
///
/// The walks above pin four functions; there are a dozen more, and the next one
/// will be written by somebody who never read this file. So the rule itself is
/// checked: a function that reaches for `.offset(` or `.paginate(` must name
/// the primary key in an `order_by_*`. That is a *sufficient* condition, not a
/// proof of totality — but it is the exact step whose absence produced the
/// defect, and it is checkable from the source.
#[test]
fn every_paginating_op_breaks_ties_on_the_primary_key() {
    let ops_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ops");
    let mut offenders = Vec::new();
    let mut checked = 0usize;

    let mut files: Vec<_> = std::fs::read_dir(&ops_dir)
        .expect("read src/ops")
        .map(|entry| entry.expect("read dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    files.sort();

    for path in &files {
        let source = std::fs::read_to_string(path).expect("read ops source");
        let file = path
            .file_name()
            .expect("ops file has a name")
            .to_string_lossy()
            .into_owned();

        // The ops files are one flat list of free functions, so splitting on
        // the `fn` keyword is enough to attribute a builder call to the
        // function that made it.
        for body in source
            .split("\nasync fn ")
            .flat_map(|s| s.split("\npub async fn "))
        {
            let name = body
                .split(['(', '<', '\n'])
                .next()
                .unwrap_or_default()
                .trim()
                .to_string();
            let body = without_the_next_item_s_doc_block(body);
            if !body.contains(".offset(") && !body.contains(".paginate(") {
                continue;
            }
            checked += 1;
            let breaks_ties = body
                .match_indices("order_by_")
                .any(|(at, _)| body[at..].lines().take(2).any(|l| l.contains("Column::Id")));
            if !breaks_ties {
                offenders.push(format!("{file}::{name}"));
            }
        }
    }

    assert!(
        checked >= 14,
        "the scan found only {checked} paginating ops — it stopped matching the \
         source layout and is no longer checking anything"
    );
    assert!(
        offenders.is_empty(),
        "these paginating ops sort on a key that admits ties, so their page \
         boundaries move between requests — add `order_by_*(…Column::Id)` after \
         the sort key: {offenders:#?}"
    );
}

/// card_1e3c1cff05b4: `Paginator::fetch_page` takes a **page index**, not a row
/// offset, and squares the unit if it is handed one.
///
/// The guard above cannot see this defect: the offending listings had a perfectly
/// total order, and the wrong number was the *argument*. `fetch_page(page)`
/// builds `OFFSET page_size * page` itself, so a row offset arriving there comes
/// out as `per_page² × (page − 1)` — at `per_page = 20`, page 2 asks for row 400.
/// The response is `200` with a correct `total` and an empty `data`, and nothing
/// in it says the rest of the rows are unreachable on every page.
///
/// The whole defect is legible at the call site: a parameter named `offset` fed
/// into `fetch_page`. Two ops did exactly that. So that is what is checked —
/// every `fetch_page` argument must be page-shaped by name, which is the one
/// step whose absence produced the defect and is checkable from the source.
///
/// A function that wants row offsets should not reach for `fetch_page` at all;
/// `.offset(offset).limit(limit)` is what every other op in this module uses and
/// carries no unit to confuse.
#[test]
fn no_paginating_op_hands_a_row_offset_to_fetch_page() {
    let ops_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ops");
    let mut offenders = Vec::new();
    let mut checked = 0usize;

    let mut files: Vec<_> = std::fs::read_dir(&ops_dir)
        .expect("read src/ops")
        .map(|entry| entry.expect("read dir entry").path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "rs"))
        .collect();
    files.sort();

    for path in &files {
        let source = std::fs::read_to_string(path).expect("read ops source");
        let file = path
            .file_name()
            .expect("ops file has a name")
            .to_string_lossy()
            .into_owned();

        // Comment lines are skipped, so the prose above these very functions —
        // which has to name `fetch_page` to explain the rule — cannot be read
        // as a call site.
        for line in source
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
        {
            let Some(after) = line.split_once(".fetch_page(") else {
                continue;
            };
            let argument = after.1.split(')').next().unwrap_or_default().trim();
            checked += 1;
            // `page` / `page_index` is the unit `fetch_page` means. Anything
            // else — and `offset` above all — is a different quantity wearing
            // the same parameter slot.
            if !argument.contains("page") {
                offenders.push(format!("{file}: fetch_page({argument})"));
            }
        }
    }

    assert!(
        checked >= 2,
        "the scan found only {checked} `fetch_page` call(s) — it stopped matching \
         the source layout and is no longer checking anything"
    );
    assert!(
        offenders.is_empty(),
        "`fetch_page` takes a 0-based PAGE INDEX and multiplies it by the page \
         size itself; these call sites hand it something else, which squares the \
         unit and makes most of the listing unreachable on every page. Slice with \
         `.offset(offset).limit(limit)` instead: {offenders:#?}"
    );
}

/// OCI uses keyset pagination rather than LIMIT/OFFSET: `last` must be compared
/// by the same database ordering that determines which tag follows it.
///
/// SQLite often reads the unique `(repository, tag)` index in tag order even
/// without an explicit `ORDER BY`, so a behavioural test can stay green after
/// that clause is deleted. This guard holds the query shape independently; the
/// routed test in `rg-http` proves the resulting page walk and Link contract.
#[test]
fn oci_tag_marker_and_order_use_the_same_column() {
    let source = include_str!("../../src/ops/oci_ops.rs");
    let (_, after_name) = source
        .split_once("\npub async fn list_tags(")
        .expect("oci_ops::list_tags must remain discoverable");
    let body = after_name
        .split("\nasync fn ")
        .next()
        .expect("list_tags body")
        .split("\npub async fn ")
        .next()
        .expect("list_tags body");
    let body = without_the_next_item_s_doc_block(body);

    assert!(
        body.contains("oci_tag::Column::Tag.gt(last)"),
        "oci_ops::list_tags must start strictly after the requested tag"
    );
    assert!(
        body.contains(".order_by_asc(oci_tag::Column::Tag)"),
        "oci_ops::list_tags must define the tag order that `last` advances through"
    );
    assert!(
        body.contains("query.limit(limit)"),
        "card_f5bc6920f459: the page size must reach SQL as a `LIMIT`. Truncating \
         in Rust over the full selection reads the whole repository to serve one \
         page, and a walk by `Link` reads it once per page"
    );
}

/// card_f5bc6920f459: the page the caller asked for is the page the database
/// builds — reading the rest and dropping it in Rust is not applying a limit.
///
/// The source guard above holds the `.limit()` in the query shape; this holds
/// what the caller can observe. Move the truncation back into the caller and
/// the function starts answering with every tag in the repository, which is
/// exactly the cost the limit exists to bound.
#[tokio::test]
async fn oci_tag_page_size_is_applied_by_the_database() {
    let (db, _temp) = migrated_db("oci-tag-limit").await;
    let at = chrono::Utc::now();
    let (user_id, repo) = owner_and_repo(&db, at).await;
    let oci_repo = rg_db::ops::oci_ops::find_or_create_repo(&db, repo.id, "registry", user_id)
        .await
        .expect("create the OCI repository the tags belong to");

    // Pushed out of lexical order so a page that is right by luck of insertion
    // order is still wrong here.
    for tag in ["zeta", "alpha", "middle", "beta", "gamma"] {
        rg_db::ops::oci_ops::upsert_tag_manifest(
            &db,
            oci_repo.id,
            tag,
            &format!("sha256:{tag}"),
            "application/vnd.oci.image.manifest.v1+json",
            2,
            "{}",
            2,
            Some(user_id),
            &[],
        )
        .await
        .unwrap_or_else(|error| panic!("record tag {tag}: {error}"));
    }

    assert_eq!(
        rg_db::ops::oci_ops::list_tags(&db, oci_repo.id, None, Some(2))
            .await
            .expect("read a bounded first page"),
        vec!["alpha".to_string(), "beta".to_string()],
        "a page of two must come back as two rows, not as five for the caller to cut",
    );
    assert_eq!(
        rg_db::ops::oci_ops::list_tags(&db, oci_repo.id, Some("beta"), Some(2))
            .await
            .expect("read a bounded page after a marker"),
        vec!["gamma".to_string(), "middle".to_string()],
        "the bound must compose with the keyset marker rather than replace it",
    );
    assert_eq!(
        rg_db::ops::oci_ops::list_tags(&db, oci_repo.id, None, Some(99))
            .await
            .expect("read a page larger than the repository")
            .len(),
        5,
        "a limit above the row count must not invent rows or drop any",
    );
    assert_eq!(
        rg_db::ops::oci_ops::list_tags(&db, oci_repo.id, None, None)
            .await
            .expect("read the unpaged listing")
            .len(),
        5,
        "no limit still means the complete listing — the unpaged contract is unchanged",
    );
}
