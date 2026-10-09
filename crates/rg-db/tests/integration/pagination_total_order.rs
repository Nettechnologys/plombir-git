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

#[allow(dead_code)]
mod rust_source {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/support/rust_source.rs"
    ));
}

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
            "plombir-git-pagination-{label}-{}.db",
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
        let created = rg_db::sea_orm::ActiveModelTrait::insert(
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
            &db,
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
        let (rows, count) = rg_db::ops::repo_ops::list_forks_visible_to(
            &db,
            origin.id,
            None,
            page * PER_PAGE,
            PER_PAGE,
        )
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

#[derive(Debug, Eq, PartialEq)]
struct FunctionRange {
    name: std::ops::Range<usize>,
    body: std::ops::Range<usize>,
}

#[derive(Debug, Eq, PartialEq)]
struct SourceScan {
    checked: usize,
    offenders: Vec<String>,
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn skip_whitespace(code: &str, mut at: usize) -> usize {
    while code
        .as_bytes()
        .get(at)
        .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        at += 1;
    }
    at
}

fn generic_group_end(code: &str, open: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    if bytes.get(open) != Some(&b'<') {
        return None;
    }

    let mut depth = 0usize;
    for (relative, byte) in bytes[open..].iter().enumerate() {
        match byte {
            b'<' => depth += 1,
            b'>' if bytes.get((open + relative).wrapping_sub(1)) != Some(&b'-') => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(open + relative + 1);
                }
            }
            _ => {}
        }
    }
    None
}

fn bracket_group_end(code: &str, open: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    if !matches!(bytes.get(open), Some(b'(' | b'[' | b'{')) {
        return None;
    }

    let mut depth = 0usize;
    for (relative, byte) in bytes[open..].iter().enumerate() {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(open + relative + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Function ranges found only from the byte-aligned production-code view.
///
/// The returned offsets address the original source too. A declaration-shaped
/// literal therefore cannot create a range, a brace in prose cannot close one,
/// and a complete `#[cfg(test)]` item contributes no production function.
fn production_function_ranges(code: &str) -> Vec<FunctionRange> {
    let bytes = code.as_bytes();
    let mut ranges = Vec::new();
    let mut cursor = 0usize;

    while let Some(relative) = code[cursor..].find("fn") {
        let fn_at = cursor + relative;
        let after_fn = fn_at + 2;
        if bytes
            .get(fn_at.wrapping_sub(1))
            .is_some_and(|byte| is_ident_byte(*byte))
            || bytes.get(after_fn).is_some_and(|byte| is_ident_byte(*byte))
        {
            cursor = after_fn;
            continue;
        }

        let name_start = skip_whitespace(code, after_fn);
        let mut name_end = name_start;
        while bytes.get(name_end).is_some_and(|byte| is_ident_byte(*byte)) {
            name_end += 1;
        }
        if name_end == name_start {
            cursor = after_fn;
            continue;
        }

        let mut params_open = skip_whitespace(code, name_end);
        if bytes.get(params_open) == Some(&b'<') {
            let Some(after_generics) = generic_group_end(code, params_open) else {
                cursor = name_end;
                continue;
            };
            params_open = skip_whitespace(code, after_generics);
        }
        if bytes.get(params_open) != Some(&b'(') {
            cursor = name_end;
            continue;
        }
        let Some(after_params) = bracket_group_end(code, params_open) else {
            cursor = name_end;
            continue;
        };

        let mut body_open = skip_whitespace(code, after_params);
        while let Some(byte) = bytes.get(body_open) {
            match byte {
                b'{' => break,
                b';' => {
                    body_open = bytes.len();
                    break;
                }
                b'(' | b'[' => {
                    let Some(after_group) = bracket_group_end(code, body_open) else {
                        body_open = bytes.len();
                        break;
                    };
                    body_open = after_group;
                }
                _ => body_open += 1,
            }
        }
        if bytes.get(body_open) != Some(&b'{') {
            cursor = name_end;
            continue;
        }
        let Some(body_end) = bracket_group_end(code, body_open) else {
            cursor = name_end;
            continue;
        };

        ranges.push(FunctionRange {
            name: name_start..name_end,
            body: fn_at..body_end,
        });
        cursor = body_end;
    }

    ranges
}

fn method_call_argument_ranges(code: &str, name_prefix: &str) -> Vec<std::ops::Range<usize>> {
    let bytes = code.as_bytes();
    let needle = format!(".{name_prefix}");
    let mut arguments = Vec::new();

    for (at, _) in code.match_indices(&needle) {
        let name_start = at + 1;
        let prefix_end = name_start + name_prefix.len();
        let name_end = if name_prefix.ends_with('_') {
            let mut name_end = prefix_end;
            while bytes.get(name_end).is_some_and(|byte| is_ident_byte(*byte)) {
                name_end += 1;
            }
            if name_end == prefix_end {
                continue;
            }
            name_end
        } else {
            if bytes
                .get(prefix_end)
                .is_some_and(|byte| is_ident_byte(*byte))
            {
                continue;
            }
            prefix_end
        };
        let open = skip_whitespace(code, name_end);
        if bytes.get(open) != Some(&b'(') {
            continue;
        }
        if let Some(end) = bracket_group_end(code, open) {
            arguments.push(open + 1..end - 1);
        }
    }

    arguments
}

/// The body of the production `fn name`, read from the byte-aligned code-only
/// view.
///
/// A declaration-shaped literal therefore cannot invent the function, prose
/// cannot end it early, and a `#[cfg(test)]` copy of the name is not the
/// production one.
fn production_function_body<'a>(code: &'a str, name: &str) -> Option<&'a str> {
    let mut bodies = production_function_ranges(code)
        .into_iter()
        .filter(|function| code[function.name.clone()] == *name)
        .map(|function| &code[function.body]);

    let only = bodies.next()?;
    assert!(
        bodies.next().is_none(),
        "expected exactly one production `fn {name}` — with a second one, which of \
         the two this guard reads is an accident of source order"
    );
    Some(only)
}

fn has_method_call(code: &str, name: &str) -> bool {
    !method_call_argument_ranges(code, name).is_empty()
}

fn contains_column_id(code: &str) -> bool {
    let bytes = code.as_bytes();
    code.match_indices("Column").any(|(at, _)| {
        let before_ok = bytes
            .get(at.wrapping_sub(1))
            .is_none_or(|byte| !is_ident_byte(*byte));
        let mut cursor = skip_whitespace(code, at + "Column".len());
        let has_separator = bytes.get(cursor..cursor + 2) == Some(b"::");
        cursor = skip_whitespace(code, cursor + usize::from(has_separator) * 2);
        let has_id = code.get(cursor..cursor + 2) == Some("Id")
            && bytes
                .get(cursor + 2)
                .is_none_or(|byte| !is_ident_byte(*byte));
        before_ok && has_separator && has_id
    })
}

fn contains_page_shaped_identifier(code: &str) -> bool {
    code.split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
        .any(|ident| {
            ident == "page"
                || ident.starts_with("page_")
                || ident.ends_with("_page")
                || ident.contains("_page_")
        })
}

fn scan_total_order(source: &str) -> SourceScan {
    let code = rust_source::production_rust_code_only(source);
    let mut offenders = Vec::new();
    let mut checked = 0usize;

    let functions = production_function_ranges(&code);
    // A listing may build its ordered selection in a helper of the same file
    // (so a query-plan test can explain the very statement the server sends)
    // and only cut the page itself. The helper's `ORDER BY` is then the one
    // the page is cut from, so it counts — but only when the helper is called
    // as a free function: `.name(` is some other type's method and `path::name(`
    // another module's function, and neither is the helper this file defines.
    let ordering_helpers: Vec<&str> = functions
        .iter()
        .filter(|function| orders_by_primary_key(&code[function.body.clone()]))
        .map(|function| &code[function.name.clone()])
        .collect();

    for function in &functions {
        let body = &code[function.body.clone()];
        if !has_method_call(body, "offset") && !has_method_call(body, "paginate") {
            continue;
        }
        checked += 1;
        let breaks_ties = orders_by_primary_key(body)
            || ordering_helpers
                .iter()
                .any(|helper| calls_free_function(body, helper));
        if !breaks_ties {
            offenders.push(source[function.name.clone()].to_owned());
        }
    }

    SourceScan { checked, offenders }
}

/// Whether some `order_by_*` in `body` names the primary key.
fn orders_by_primary_key(body: &str) -> bool {
    method_call_argument_ranges(body, "order_by_")
        .into_iter()
        .any(|argument| contains_column_id(&body[argument]))
}

/// Whether `body` calls `name(` as a free function — not as `.name(` (a
/// method) and not as `path::name(` (another module's function).
fn calls_free_function(body: &str, name: &str) -> bool {
    let bytes = body.as_bytes();
    body.match_indices(name).any(|(at, _)| {
        let before_ok = bytes
            .get(at.wrapping_sub(1))
            .is_none_or(|byte| !is_ident_byte(*byte) && *byte != b'.' && *byte != b':');
        let after = skip_whitespace(body, at + name.len());
        before_ok && bytes.get(after) == Some(&b'(')
    })
}

#[test]
fn pagination_source_scan_follows_an_ordering_helper_of_the_same_file_only() {
    let source = r###"
pub async fn delegates() {
    page_query(repo_id).offset(offset);
}

fn page_query(repo_id: i64) -> Select<Entity> {
    Entity::find()
        .order_by_desc(entity::Column::CreatedAt)
        .order_by_desc(entity::Column::Id)
}

pub async fn delegates_to_a_builder_that_leaves_ties() {
    loose_query().offset(offset);
}

fn loose_query() -> Select<Entity> {
    Entity::find().order_by_desc(entity::Column::CreatedAt)
}

pub async fn calls_a_method_of_the_same_name() {
    other.page_query(repo_id).offset(offset);
}

pub async fn calls_another_modules_function_of_the_same_name() {
    elsewhere::page_query(repo_id).offset(offset);
}
"###;

    assert_eq!(
        scan_total_order(source),
        SourceScan {
            checked: 4,
            offenders: vec![
                "delegates_to_a_builder_that_leaves_ties".to_owned(),
                "calls_a_method_of_the_same_name".to_owned(),
                "calls_another_modules_function_of_the_same_name".to_owned(),
            ],
        }
    );
}

fn scan_fetch_page_arguments(source: &str) -> SourceScan {
    let code = rust_source::production_rust_code_only(source);
    let mut offenders = Vec::new();
    let mut checked = 0usize;

    for function in production_function_ranges(&code) {
        let body = &code[function.body];
        let name = &source[function.name];
        for argument in method_call_argument_ranges(body, "fetch_page") {
            checked += 1;
            let argument = body[argument].trim();
            if !contains_page_shaped_identifier(argument) {
                offenders.push(format!("{name}: fetch_page({argument})"));
            }
        }
    }

    SourceScan { checked, offenders }
}

#[test]
fn pagination_source_scans_ignore_non_code_decoys_and_keep_live_calls() {
    let source = r###"
pub async fn prose_only() {
    let normal = ".offset(offset).fetch_page(offset)";
    let bytes = b".paginate(db, 20).fetch_page(page)";
    let raw = r#"pub async fn invented() {
        query.offset(offset).order_by_desc(entity::Column::Id);
    }"#;
    let raw_bytes = br#".fetch_page(offset)"#;
    // query.offset(offset).order_by_desc(entity::Column::Id);
    /* query.paginate(db, 20).fetch_page(offset); */
}

#[cfg(test)]
mod test_only {
    async fn pagination_decoy() {
        query.offset(offset).order_by_desc(entity::Column::Id);
        query.fetch_page(page).await;
    }
}

pub async fn real_listing() {
    query
        .order_by_desc(entity::Column::CreatedAt)
        .order_by_desc(entity::Column::Id)
        .offset(offset)
        .all(db)
        .await;
}

pub async fn real_page() {
    query
        .order_by_asc(entity::Column::Id)
        .paginate(db, per_page)
        .fetch_page(clamp_page_index(page - 1, per_page))
        .await;
}
"###;

    assert_eq!(
        scan_total_order(source),
        SourceScan {
            checked: 2,
            offenders: Vec::new(),
        }
    );
    assert_eq!(
        scan_fetch_page_arguments(source),
        SourceScan {
            checked: 1,
            offenders: Vec::new(),
        }
    );
}

#[test]
fn pagination_source_scans_report_live_offenders_after_decoys() {
    let source = r###"
pub async fn before() {
    let raw = r#"} pub async fn invented() { .offset(offset) }"#;
}

pub async fn missing_tiebreaker() {
    query.order_by_desc(entity::Column::CreatedAt).offset(offset);
}

pub async fn row_offset_in_a_page_slot() {
    query.paginate(db, per_page).fetch_page(offset).await;
}
"###;

    assert_eq!(
        scan_total_order(source),
        SourceScan {
            checked: 2,
            offenders: vec![
                "missing_tiebreaker".to_owned(),
                "row_offset_in_a_page_slot".to_owned(),
            ],
        }
    );
    assert_eq!(
        scan_fetch_page_arguments(source),
        SourceScan {
            checked: 1,
            offenders: vec!["row_offset_in_a_page_slot: fetch_page(offset)".to_owned()],
        }
    );
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

        let scan = scan_total_order(&source);
        checked += scan.checked;
        offenders.extend(
            scan.offenders
                .into_iter()
                .map(|function| format!("{file}::{function}")),
        );
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

        let scan = scan_fetch_page_arguments(&source);
        checked += scan.checked;
        offenders.extend(
            scan.offenders
                .into_iter()
                .map(|offender| format!("{file}::{offender}")),
        );
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
/// The three facts `list_tags` has to state for its keyset walk to be sound.
#[derive(Debug, PartialEq, Eq)]
struct KeysetShape {
    marker: bool,
    order: bool,
    limit: bool,
}

/// Read the query shape of the production `list_tags` out of `source`.
///
/// Every one of the three facts is a `contains` over a *code-only* body, which
/// is what makes the answer worth anything: the same three spellings occur in
/// this module's own prose, and a comment left behind by the commit that
/// deleted the call it describes would otherwise keep the guard green.
fn scan_oci_tag_keyset(source: &str) -> Option<KeysetShape> {
    let code = rust_source::production_rust_code_only(source);
    let body = production_function_body(&code, "list_tags")?;

    Some(KeysetShape {
        marker: body.contains("oci_tag::Column::Tag.gt(last)"),
        order: body.contains(".order_by_asc(oci_tag::Column::Tag)"),
        limit: body.contains("query.limit(limit)"),
    })
}

#[test]
fn oci_keyset_scan_ignores_non_code_decoys_and_keeps_the_live_query_shape() {
    // All three spellings are present, none of them in executable code.
    let decoys_only = r###"
/// Starts after `oci_tag::Column::Tag.gt(last)`.
pub async fn list_tags(db: &Db) -> Vec<String> {
    let normal = "oci_tag::Column::Tag.gt(last)";
    let bytes = b".order_by_asc(oci_tag::Column::Tag)";
    let raw = r#"query.limit(limit)"#;
    // .order_by_asc(oci_tag::Column::Tag)
    /* query.limit(limit) */
    query.all(db).await
}
"###;

    assert_eq!(
        scan_oci_tag_keyset(decoys_only),
        Some(KeysetShape {
            marker: false,
            order: false,
            limit: false,
        }),
        "prose and literals are being read as the query the function builds"
    );

    // A declaration-shaped literal, a brace in prose and a test-only namesake
    // ahead of the real function; the live calls follow all three.
    let live_after_decoys = r###"
pub async fn before() {
    let raw = r#"} pub async fn list_tags(db: &Db) { query.limit(limit) }"#;
}

#[cfg(test)]
mod test_only {
    async fn list_tags(db: &Db) -> Vec<String> {
        query.limit(limit).all(db).await
    }
}

pub async fn list_tags(db: &Db, last: Option<&str>, limit: Option<u64>) -> Vec<String> {
    let note = "list_tags once ordered by oci_tag::Column::Id";
    if let Some(last) = last {
        query = query.filter(oci_tag::Column::Tag.gt(last));
    }
    query = query.order_by_asc(oci_tag::Column::Tag);
    if let Some(limit) = limit {
        query = query.limit(limit);
    }
    query.all(db).await
}
"###;

    assert_eq!(
        scan_oci_tag_keyset(live_after_decoys),
        Some(KeysetShape {
            marker: true,
            order: true,
            limit: true,
        }),
        "the live query shape stops being visible once a decoy precedes it"
    );

    assert_eq!(
        scan_oci_tag_keyset("pub async fn other() {}\n"),
        None,
        "a file without a production `list_tags` must be reported as such, not \
         silently pass every assertion"
    );
}

#[test]
fn oci_tag_marker_and_order_use_the_same_column() {
    let source = include_str!("../../src/ops/oci_ops.rs");
    let shape = scan_oci_tag_keyset(source).expect("oci_ops::list_tags must remain discoverable");

    assert!(
        shape.marker,
        "oci_ops::list_tags must start strictly after the requested tag"
    );
    assert!(
        shape.order,
        "oci_ops::list_tags must define the tag order that `last` advances through"
    );
    assert!(
        shape.limit,
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
