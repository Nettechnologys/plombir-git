//! CODEOWNERS parsing, matching, and automatic reviewer requests.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{DatabaseConnection, NotSet, Set};

use rg_db::entities::pr_reviewer_request;
use rg_db::entities::repository::Model as Repository;
use rg_db::ops::{pr_reviewer_request_ops, user_ops};

/// Where a CODEOWNERS file may live, in priority order: the first path that
/// exists on the base branch is the policy and the rest are not looked at.
///
/// A list, not three literals inside the loop, so `docs/codeowners.md` has
/// something to be checked against — an author has no other way to learn the
/// paths, and nothing in the product names them.
const CODEOWNERS_PATHS: &[&str] = &[".github/CODEOWNERS", "CODEOWNERS", "docs/CODEOWNERS"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeownerRule {
    pub line: usize,
    pub pattern: String,
    pub owners: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeownersDiagnostic {
    pub line: usize,
    pub declaration: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedCodeowners {
    pub rules: Vec<CodeownerRule>,
    pub diagnostics: Vec<CodeownersDiagnostic>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeownerRequestOutcome {
    pub requested: Vec<String>,
    pub diagnostics: Vec<CodeownersDiagnostic>,
}

impl CodeownersDiagnostic {
    fn new(line: usize, declaration: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            line,
            declaration: declaration.into(),
            reason: reason.into(),
        }
    }
}

/// Parse a CODEOWNERS file, retaining every executable rule and explaining
/// every non-comment declaration the engine cannot execute.
pub fn parse_codeowners(contents: &str) -> ParsedCodeowners {
    let mut parsed = ParsedCodeowners::default();
    for (index, source) in contents.lines().enumerate() {
        let line_number = index + 1;
        let line = strip_comment(source).trim();
        if line.is_empty() {
            continue;
        }

        let mut fields = line.split_whitespace();
        let Some(pattern) = fields.next() else {
            continue;
        };
        if has_dangling_escape(pattern) {
            parsed.diagnostics.push(CodeownersDiagnostic::new(
                line_number,
                pattern,
                "pattern ends with a backslash that escapes no character",
            ));
            continue;
        }

        let mut owners = Vec::new();
        let mut saw_owner = false;
        for declaration in fields {
            saw_owner = true;
            match declaration.strip_prefix('@') {
                Some(owner) if !owner.is_empty() => owners.push(owner.to_string()),
                _ => parsed.diagnostics.push(CodeownersDiagnostic::new(
                    line_number,
                    declaration,
                    format!("`{declaration}` is not an `@username` or `@org/team` owner"),
                )),
            }
        }
        if !saw_owner {
            parsed.diagnostics.push(CodeownersDiagnostic::new(
                line_number,
                pattern,
                "rule names no owner",
            ));
        }
        if !owners.is_empty() {
            parsed.rules.push(CodeownerRule {
                line: line_number,
                pattern: pattern.to_string(),
                owners,
            });
        }
    }
    parsed
}

fn owners_for_paths_with_lines(rules: &[CodeownerRule], paths: &[String]) -> Vec<(usize, String)> {
    let mut seen = HashSet::new();
    let mut owners = Vec::new();
    for path in paths {
        if let Some(rule) = rules
            .iter()
            .rev()
            .find(|rule| pattern_matches(&rule.pattern, path))
        {
            for owner in &rule.owners {
                if seen.insert(owner.to_ascii_lowercase()) {
                    owners.push((rule.line, owner.clone()));
                }
            }
        }
    }
    owners
}

/// Load CODEOWNERS from the base branch using the standard location priority.
pub fn load_codeowners(repo_path: &Path, base_branch: &str) -> Result<Option<ParsedCodeowners>> {
    let git = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let branch_ref = format!("refs/heads/{base_branch}");
    for candidate in CODEOWNERS_PATHS.iter().copied() {
        let listing = git.run(
            &["ls-tree", "-z", "--name-only", &branch_ref, "--", candidate],
            Some(repo_path),
        )?;
        listing
            .ensure_success()
            .with_context(|| format!("look up CODEOWNERS candidate `{candidate}`"))?;
        if !listing
            .stdout
            .split(|byte| *byte == 0)
            .any(|name| name == candidate.as_bytes())
        {
            continue;
        }

        let object = format!("{branch_ref}:{candidate}");
        let output = git.run(&["cat-file", "blob", &object], Some(repo_path))?;
        output
            .ensure_success()
            .with_context(|| format!("read CODEOWNERS candidate `{candidate}`"))?;
        return Ok(Some(parse_codeowners(&output.stdout_str())));
    }
    Ok(None)
}

/// Request readable, active user accounts selected by CODEOWNERS and return a
/// diagnostic for every matched declaration the engine cannot resolve. A team
/// owner (`@org/team`) is accepted only for this repository's organization and
/// only when the team has write/admin permission.
#[allow(clippy::too_many_arguments)]
pub async fn request_codeowners(
    db: &DatabaseConnection,
    repo_path: &Path,
    base_branch: &str,
    changed_paths: &[String],
    repository: &Repository,
    pr_id: i64,
    author_id: i64,
    requested_by_id: i64,
) -> Result<CodeownerRequestOutcome> {
    let repo_path = repo_path.to_path_buf();
    let base_branch = base_branch.to_string();
    let Some(parsed) =
        tokio::task::spawn_blocking(move || load_codeowners(&repo_path, &base_branch)).await??
    else {
        return Ok(CodeownerRequestOutcome::default());
    };

    let mut requested = Vec::new();
    let mut diagnostics = parsed.diagnostics;
    let mut seen_users = HashSet::new();
    for (line, owner) in owners_for_paths_with_lines(&parsed.rules, changed_paths) {
        let mut candidates = Vec::new();
        if let Some((org_name, team_name)) = owner.split_once('/') {
            if team_name.contains('/') {
                diagnostics.push(CodeownersDiagnostic::new(
                    line,
                    format!("@{owner}"),
                    "team owner must have the form `@org/team`",
                ));
                continue;
            }
            let Some(org_id) = repository.org_id else {
                diagnostics.push(CodeownersDiagnostic::new(
                    line,
                    format!("@{owner}"),
                    "repository is not owned by an organization",
                ));
                continue;
            };
            let Some(org) = rg_db::ops::org_ops::get_org(db, org_id).await? else {
                diagnostics.push(CodeownersDiagnostic::new(
                    line,
                    format!("@{owner}"),
                    format!("repository organization id {org_id} does not exist"),
                ));
                continue;
            };
            if !org.name.eq_ignore_ascii_case(org_name) {
                diagnostics.push(CodeownersDiagnostic::new(
                    line,
                    format!("@{owner}"),
                    format!(
                        "organization `{org_name}` does not own this repository (expected `{}`)",
                        org.name
                    ),
                ));
                continue;
            }
            let Some(team) = rg_db::ops::org_ops::find_team_by_name(db, org_id, team_name).await?
            else {
                diagnostics.push(CodeownersDiagnostic::new(
                    line,
                    format!("@{owner}"),
                    format!(
                        "team `{team_name}` does not exist in organization `{}`",
                        org.name
                    ),
                ));
                continue;
            };
            if !matches!(team.permission.as_str(), "write" | "admin") {
                diagnostics.push(CodeownersDiagnostic::new(
                    line,
                    format!("@{owner}"),
                    format!(
                        "team permission `{}` cannot own code; expected `write` or `admin`",
                        team.permission
                    ),
                ));
                continue;
            }
            candidates.extend(
                rg_db::ops::org_ops::list_team_members(db, team.id)
                    .await?
                    .into_iter()
                    .map(|member| member.user_id),
            );
        } else if let Some(user) = user_ops::find_by_username(db, &owner)
            .await
            .with_context(|| format!("resolve CODEOWNER @{owner}"))?
        {
            candidates.push(user.id);
        } else {
            diagnostics.push(CodeownersDiagnostic::new(
                line,
                format!("@{owner}"),
                format!("account `{owner}` does not exist"),
            ));
        }

        for candidate_id in candidates {
            if !seen_users.insert(candidate_id) {
                continue;
            }
            let Some(user) = user_ops::find_by_id(db, candidate_id).await? else {
                continue;
            };
            if user.id == author_id || !user.is_active || user.deleted_at.is_some() {
                continue;
            }
            // `?`, not `unwrap_or(false)`: a read check that could not run is
            // not a codeowner without access. Swallowing it dropped the
            // reviewer from the PR and still reported success — every other
            // lookup in this loop propagates, and the caller logs the failure.
            if !crate::repo::service::can_read_repo(db, repository, Some(user.id)).await? {
                continue;
            }
            if pr_reviewer_request_ops::find(db, pr_id, user.id)
                .await?
                .is_some()
            {
                continue;
            }

            let request = pr_reviewer_request_ops::create(
                db,
                pr_reviewer_request::ActiveModel {
                    id: NotSet,
                    pr_id: Set(pr_id),
                    reviewer_id: Set(user.id),
                    requested_by_id: Set(requested_by_id),
                    created_at: Set(Utc::now()),
                },
            )
            .await?;
            rg_db::ops::pr_event_ops::record(
                db,
                repository.id,
                pr_id,
                Some(requested_by_id),
                "reviewer_requested",
                None,
                serde_json::json!({
                    "request_id": request.id,
                    "reviewer_id": user.id,
                    "reviewer": user.username.clone(),
                    "source": "codeowners"
                }),
            )
            .await?;
            requested.push(user.username);
        }
    }
    Ok(CodeownerRequestOutcome {
        requested,
        diagnostics,
    })
}

/// The rule text of a line: everything before the `#` that opens a comment.
///
/// A `#` is escaped by an *odd* number of preceding backslashes, because an
/// even run is itself escaped: `\#` is a literal `#`, and `\\#` is a literal
/// backslash followed by a comment. Counting the run rather than looking at
/// one byte is what keeps the two readings apart — and the escape is only
/// half the job, [`glob_matches`] resolving it is the other half.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b'#' {
            continue;
        }
        let escapes = bytes[..index]
            .iter()
            .rev()
            .take_while(|byte| **byte == b'\\')
            .count();
        if escapes % 2 == 0 {
            return &line[..index];
        }
    }
    line
}

/// A pattern whose final backslash has nothing to escape.
///
/// It is what an author writes when trying to escape a space — `docs\ dir` —
/// which this format cannot carry at all: the field ends at the whitespace
/// before anything reads the backslash, leaving a pattern that means nothing
/// anybody wrote. Dropping the rule is the honest answer; keeping it would
/// hand back a rule for a path that ends in a backslash.
fn has_dangling_escape(pattern: &str) -> bool {
    pattern
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'\\')
        .count()
        % 2
        == 1
}

fn pattern_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim();
    let anchored = pattern.starts_with('/');
    let mut pattern = pattern.trim_start_matches('/').to_string();
    if pattern.ends_with('/') {
        pattern.push_str("**");
    }
    let path = path.trim_start_matches('/');

    if anchored || pattern.contains('/') {
        glob_matches(pattern.as_bytes(), path.as_bytes())
    } else {
        path.split('/')
            .any(|component| glob_matches(pattern.as_bytes(), component.as_bytes()))
    }
}

fn glob_matches(pattern: &[u8], value: &[u8]) -> bool {
    fn matches_from(
        pattern: &[u8],
        value: &[u8],
        pattern_index: usize,
        value_index: usize,
        failed: &mut HashSet<(usize, usize)>,
    ) -> bool {
        if !failed.insert((pattern_index, value_index)) {
            return false;
        }
        if pattern_index == pattern.len() {
            return value_index == value.len();
        }
        match pattern[pattern_index] {
            // The second half of the escape `strip_comment` grants: a
            // backslash makes the next byte a literal, so `\#` reaches the
            // file named `#` and `\*` reaches the one named `*`. Without this
            // arm the backslash stayed in the pattern as an ordinary byte and
            // the rule could only match a path that physically contained one —
            // parsed, listed, and unable to win (card_3bb161c1337a).
            b'\\' if pattern_index + 1 < pattern.len() => {
                value.get(value_index) == Some(&pattern[pattern_index + 1])
                    && matches_from(pattern, value, pattern_index + 2, value_index + 1, failed)
            }
            b'*' if pattern.get(pattern_index + 1) == Some(&b'*') => {
                let mut next = pattern_index + 2;
                while pattern.get(next) == Some(&b'*') {
                    next += 1;
                }
                (value_index..=value.len())
                    .any(|index| matches_from(pattern, value, next, index, failed))
            }
            b'*' => {
                let end = value[value_index..]
                    .iter()
                    .position(|byte| *byte == b'/')
                    .map_or(value.len(), |offset| value_index + offset);
                (value_index..=end)
                    .any(|index| matches_from(pattern, value, pattern_index + 1, index, failed))
            }
            b'?' if value_index < value.len() && value[value_index] != b'/' => {
                matches_from(pattern, value, pattern_index + 1, value_index + 1, failed)
            }
            byte if value.get(value_index) == Some(&byte) => {
                matches_from(pattern, value, pattern_index + 1, value_index + 1, failed)
            }
            _ => false,
        }
    }

    matches_from(pattern, value, 0, 0, &mut HashSet::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    /// Resolve owners for changed paths. As in GitHub CODEOWNERS, the last
    /// matching rule wins for each path.
    fn owners_for_paths(rules: &[CodeownerRule], paths: &[String]) -> Vec<String> {
        owners_for_paths_with_lines(rules, paths)
            .into_iter()
            .map(|(_, owner)| owner)
            .collect()
    }

    /// The document an author of a *repository* — not an operator of this
    /// server — reads to learn this file. Resolved at compile time, so a moved
    /// or renamed document breaks the build instead of silently skipping the
    /// checks below, and editing it re-runs them.
    const CODEOWNERS_DOCUMENTATION: (&str, &str) = (
        "docs/codeowners.md",
        include_str!("../../../../docs/codeowners.md"),
    );

    /// The production view of this file, with complete test items blanked, so a
    /// literal that exists only in a fixture cannot pass for one the engine
    /// enforces.
    fn production_source() -> String {
        rust_source::production_rust_source(include_str!("codeowners.rs"))
    }

    fn team_owner_permission_guard(source: &str) -> &str {
        const PREFIX: &str = "matches!(team.permission.as_str(), ";

        let code = rust_source::rust_code_only(source);
        let start = code.find(PREFIX).map(|at| at + PREFIX.len()).expect(
            "the team-owner permission check must stay a `matches!` over \
                 `team.permission.as_str()` for the document to be checked against it",
        );
        let end = code[start..]
            .find(')')
            .map(|relative| start + relative)
            .expect("the team-owner permission `matches!` guard must close");
        &source[start..end]
    }

    /// The line a marker comment sits on.
    ///
    /// The marker, rather than a block's position on the page, is what ties an
    /// example in the document to this file: inserting a paragraph must not
    /// silently re-point a check at somebody else's example.
    fn marker_line(name: &str, content: &str, marker: &str) -> usize {
        content
            .lines()
            .position(|line| line.trim() == marker)
            .unwrap_or_else(|| {
                panic!(
                    "{name}: the marker `{marker}` is gone, so the example it introduced can no \
                     longer be checked against this file — restore it above the block"
                )
            })
    }

    /// The body of the first fenced block after a marker comment.
    fn fenced_block_after(name: &str, content: &str, marker: &str) -> String {
        let start = marker_line(name, content, marker);
        let mut body: Vec<&str> = Vec::new();
        let mut inside = false;

        for line in content.lines().skip(start + 1) {
            if line.trim_start().starts_with("```") {
                if inside {
                    return body.join("\n");
                }
                inside = true;
                continue;
            }
            if inside {
                body.push(line);
            }
        }
        panic!(
            "{name}: no closed fenced block follows the marker `{marker}`, so the example it \
             introduces cannot be read"
        )
    }

    /// The body rows of the first Markdown table after a marker comment, as
    /// `(document line number, cells)`. The header and the `---` separator are
    /// dropped; cells keep their text verbatim.
    fn table_rows_after(name: &str, content: &str, marker: &str) -> Vec<(usize, Vec<String>)> {
        let start = marker_line(name, content, marker);
        let mut rows = Vec::new();
        let mut header = false;

        for (index, line) in content.lines().enumerate().skip(start + 1) {
            let line = line.trim();
            if !line.starts_with('|') {
                if header {
                    break;
                }
                continue;
            }
            let cells: Vec<String> = line
                .trim_matches('|')
                .split('|')
                .map(|cell| cell.trim().to_owned())
                .collect();
            if !header {
                header = true;
                continue;
            }
            if cells
                .iter()
                .all(|cell| !cell.is_empty() && cell.chars().all(|c| c == '-' || c == ':'))
            {
                continue;
            }
            rows.push((index + 1, cells));
        }

        assert!(
            header,
            "{name}: no Markdown table follows the marker `{marker}`"
        );
        rows
    }

    /// One table cell that holds a single `code`-fenced value.
    fn code_cell(cell: &str) -> String {
        cell.trim().trim_matches('`').trim().to_owned()
    }

    /// The three paths are the whole of "where does this file go?", and none of
    /// them is discoverable: nothing in the UI names the file, and the order
    /// decides which copy wins when an author has committed two. Compared in
    /// both directions, and in order — the order *is* the priority rule.
    #[test]
    fn every_path_a_codeowners_file_may_live_at_is_listed_in_the_documentation() {
        let (name, content) = CODEOWNERS_DOCUMENTATION;
        let documented: Vec<String> =
            fenced_block_after(name, content, "<!-- inventory: codeowners-paths -->")
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect();
        let opened: Vec<String> = CODEOWNERS_PATHS
            .iter()
            .map(|path| (*path).to_owned())
            .collect();

        let documented_set: BTreeSet<&str> = documented.iter().map(String::as_str).collect();
        let opened_set: BTreeSet<&str> = opened.iter().map(String::as_str).collect();

        let undocumented: Vec<&&str> = opened_set.difference(&documented_set).collect();
        assert!(
            undocumented.is_empty(),
            "{name}: each of {undocumented:?} is a path a CODEOWNERS file is read from, and the \
             inventory does not list it — an author has no way to learn the path exists"
        );

        let abandoned: Vec<&&str> = documented_set.difference(&opened_set).collect();
        assert!(
            abandoned.is_empty(),
            "{name}: the inventory offers each of {abandoned:?} as a CODEOWNERS location, but \
             nothing opens it any more — a file committed there is read by nobody"
        );

        assert_eq!(
            documented, opened,
            "{name}: the inventory is in a different order than the paths are tried, and the \
             document presents that order as the rule that decides which copy wins"
        );
    }

    /// The matcher is this file's own, not a gitignore library, so "it works
    /// like GitHub" is not something the document may fall back on: every rule
    /// it teaches is run here against the function that decides.
    #[test]
    fn every_matching_example_in_the_documentation_is_what_the_matcher_does() {
        let (name, content) = CODEOWNERS_DOCUMENTATION;
        let rows = table_rows_after(name, content, "<!-- examples: pattern-matching -->");

        for (line, cells) in &rows {
            assert!(
                cells.len() == 3,
                "{name}:{line}: a matching example is `| pattern | path | yes/no |`, and this row \
                 has {} cells",
                cells.len()
            );
            let pattern = code_cell(&cells[0]);
            let path = code_cell(&cells[1]);
            let expected = match cells[2].as_str() {
                "yes" => true,
                "no" => false,
                other => panic!(
                    "{name}:{line}: the Matches column reads `{other}`, and only `yes` or `no` \
                     say what the matcher is supposed to answer"
                ),
            };

            assert_eq!(
                pattern_matches(&pattern, &path),
                expected,
                "{name}:{line}: the document promises that `{pattern}` {} `{path}`, and the \
                 matcher disagrees — an author reading this page writes a rule that never fires",
                if expected {
                    "matches"
                } else {
                    "does not match"
                }
            );
        }

        // A floor, not a count: it fails loudly if the table scanner ever stops
        // matching and this test quietly checks nothing.
        assert!(
            rows.len() >= 10,
            "only {} matching examples read out of {name} — the table scanner has stopped matching",
            rows.len()
        );
    }

    /// The dialect rules are one half; which rule *wins* is the other, and it
    /// is the half an author gets wrong. The worked example and the resolution
    /// table below it are parsed and resolved by the same two functions that
    /// serve a real pull request.
    #[test]
    fn the_worked_example_resolves_to_the_owners_the_documentation_promises() {
        let (name, content) = CODEOWNERS_DOCUMENTATION;
        let rules = parse_codeowners(&fenced_block_after(
            name,
            content,
            "<!-- example: codeowners-file -->",
        ));
        assert!(
            rules.rules.len() >= 4,
            "{name}: only {} rules parsed out of the worked example — a rule the page shows is \
             being dropped by the parser it is supposed to illustrate",
            rules.rules.len()
        );
        assert!(rules.diagnostics.is_empty(), "{:?}", rules.diagnostics);

        let rows = table_rows_after(name, content, "<!-- examples: codeowners-resolution -->");
        for (line, cells) in &rows {
            assert!(
                cells.len() == 2,
                "{name}:{line}: a resolution example is `| path | owners |`, and this row has {} \
                 cells",
                cells.len()
            );
            let path = code_cell(&cells[0]);
            let expected: Vec<String> = cells[1]
                .split(',')
                .map(|owner| code_cell(owner).trim_start_matches('@').to_owned())
                .collect();

            assert_eq!(
                owners_for_paths(&rules.rules, std::slice::from_ref(&path)),
                expected,
                "{name}:{line}: the document promises that `{path}` goes to {expected:?}, and the \
                 resolver disagrees"
            );
        }

        assert!(
            rows.len() >= 4,
            "only {} resolution examples read out of {name} — the table scanner has stopped \
             matching",
            rows.len()
        );
    }

    /// The one rule an author cannot find by experiment, because failing it is
    /// indistinguishable from success: a team owner is honoured only at these
    /// permission levels. Read off the guard itself, so widening the guard
    /// cannot leave the document behind.
    #[test]
    fn the_permissions_a_team_owner_needs_are_named_in_the_documentation() {
        let (name, content) = CODEOWNERS_DOCUMENTATION;
        let source = production_source();
        let guard = team_owner_permission_guard(&source);
        let levels: Vec<&str> = guard
            .split('|')
            .map(|level| level.trim().trim_matches('"'))
            .filter(|level| !level.is_empty())
            .collect();

        assert!(
            levels.len() >= 2,
            "only {levels:?} read off the team-owner permission guard — the scanner has stopped \
             matching it"
        );

        // A set, not "is the word on the page somewhere": the page has to name
        // these levels *as the ones that grant ownership*. A mention inside a
        // sentence saying the opposite would satisfy a `contains` check and
        // leave the document contradicting the guard.
        let enforced: BTreeSet<&str> = levels.into_iter().collect();
        let documented_levels =
            fenced_block_after(name, content, "<!-- inventory: team-owner-permissions -->");
        let documented: BTreeSet<&str> = documented_levels
            .lines()
            .map(str::trim)
            .filter(|level| !level.is_empty())
            .collect();

        let undocumented: Vec<&&str> = enforced.difference(&documented).collect();
        assert!(
            undocumented.is_empty(),
            "{name} does not list {undocumented:?} among the permissions that make a team an \
             owner, so a team that has one looks to its author exactly like a team that does \
             not — the whole failure is a reviewer who never appears"
        );

        let overstated: Vec<&&str> = documented.difference(&enforced).collect();
        assert!(
            overstated.is_empty(),
            "{name} promises that {overstated:?} makes a team an owner, and the guard refuses \
             it — an author reading this page writes a rule that assigns nobody"
        );
    }

    fn bare_repository(
        codeowners: Option<(&str, &str)>,
    ) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let bare = dir.path().join("repo.git");
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();

        git.run_or_bail(&["init", "-q", "-b", "main", work.to_str().unwrap()], None)
            .unwrap();
        if let Some((path, contents)) = codeowners {
            let file = work.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, contents).unwrap();
            git.run_or_bail(&["add", "."], Some(&work)).unwrap();
        }
        git.run_or_bail(
            &[
                "-c",
                "user.name=CODEOWNERS test",
                "-c",
                "user.email=codeowners@example.com",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-qm",
                "fixture",
            ],
            Some(&work),
        )
        .unwrap();
        git.run_or_bail(
            &[
                "clone",
                "--bare",
                "-q",
                work.to_str().unwrap(),
                bare.to_str().unwrap(),
            ],
            None,
        )
        .unwrap();

        (dir, bare)
    }

    #[test]
    fn parser_ignores_comments_and_preserves_owner_order() {
        let parsed = parse_codeowners(
            "# defaults\n* @alice\n/docs/ @writers @org/docs # prose\n*.rs @rustacean\n",
        );
        assert!(parsed.diagnostics.is_empty());
        let rules = parsed.rules;
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[1].pattern, "/docs/");
        assert_eq!(rules[1].owners, ["writers", "org/docs"]);
    }

    /// The escape used to be granted by the comment stripper and honoured by
    /// nobody: the rule parsed, kept its owner, joined the list, and could only
    /// win against a path with a backslash in it. The failure an author saw was
    /// a reviewer who never appeared.
    #[test]
    fn an_escaped_hash_is_a_rule_for_the_file_named_with_one() {
        let parsed = parse_codeowners("\\#notes @alice\n");
        assert!(parsed.diagnostics.is_empty());
        let rules = parsed.rules;
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].owners, ["alice"]);

        assert!(pattern_matches(&rules[0].pattern, "#notes"));
        assert!(!pattern_matches(&rules[0].pattern, "notes"));
        assert!(
            !pattern_matches(&rules[0].pattern, "\\#notes"),
            "the backslash is the escape, not a byte of the path"
        );
        assert_eq!(owners_for_paths(&rules, &["#notes".into()]), ["alice"]);
    }

    #[test]
    fn a_backslash_escapes_a_wildcard_as_well_as_a_hash() {
        assert!(pattern_matches("a\\*b", "a*b"));
        assert!(!pattern_matches("a\\*b", "axb"));
        assert!(pattern_matches("a\\?b", "a?b"));
        assert!(!pattern_matches("a\\?b", "axb"));
    }

    /// An even run of backslashes is escaped backslashes, so the `#` after it
    /// is a comment again — the one reading under which `\\#` and `\#` are not
    /// the same line.
    #[test]
    fn only_an_odd_run_of_backslashes_escapes_the_hash() {
        assert_eq!(strip_comment("docs\\#note"), "docs\\#note");
        assert_eq!(strip_comment("docs\\\\#note"), "docs\\\\");
        assert_eq!(strip_comment("docs #note"), "docs ");
    }

    /// A space is the one character the escape cannot reach: the field ends at
    /// the whitespace before the backslash is ever read.
    #[test]
    fn a_pattern_left_holding_a_dangling_escape_is_diagnosed() {
        let parsed = parse_codeowners("docs\\ dir/*.rs @alice\n");
        assert!(parsed.rules.is_empty());
        assert_eq!(
            parsed.diagnostics,
            [CodeownersDiagnostic::new(
                1,
                "docs\\",
                "pattern ends with a backslash that escapes no character"
            )]
        );

        let escaped = parse_codeowners("docs\\\\ @alice\n");
        assert_eq!(escaped.rules.len(), 1);
        assert!(escaped.diagnostics.is_empty());
    }

    #[test]
    fn last_matching_rule_wins_per_path() {
        let parsed = parse_codeowners("* @default\n*.rs @rust\n/src/api/** @api\n");
        assert!(parsed.diagnostics.is_empty());
        let owners = owners_for_paths(
            &parsed.rules,
            &["src/api/pulls.rs".into(), "README.md".into()],
        );
        assert_eq!(owners, ["api", "default"]);
    }

    #[test]
    fn glob_supports_anchored_directories_and_double_star() {
        assert!(pattern_matches("/docs/", "docs/guides/setup.md"));
        assert!(!pattern_matches("/docs/", "nested/docs/setup.md"));
        assert!(pattern_matches("src/**/test?.rs", "src/api/v1/test1.rs"));
        assert!(!pattern_matches("src/*/test?.rs", "src/api/v1/test1.rs"));
        assert!(pattern_matches("/README.md", "README.md"));
        assert!(!pattern_matches("/README.md", "docs/README.md"));
    }

    #[test]
    fn standard_location_is_loaded_from_a_bare_repository() {
        let (_dir, bare) = bare_repository(Some((".github/CODEOWNERS", "*.rs @rust\n")));

        assert_eq!(
            load_codeowners(&bare, "main").unwrap(),
            Some(ParsedCodeowners {
                rules: vec![CodeownerRule {
                    line: 1,
                    pattern: "*.rs".into(),
                    owners: vec!["rust".into()],
                }],
                diagnostics: Vec::new(),
            })
        );
    }

    #[tokio::test]
    async fn unexecutable_owners_are_diagnosed_but_intentional_filters_stay_quiet() {
        let db = crate::test_support::migrated_memory_database().await;
        let owner = user_ops::create_user(
            &db,
            "codeowners-owner",
            "codeowners-owner@example.invalid",
            "",
            "Codeowners Owner",
        )
        .await
        .unwrap();
        let reviewer = user_ops::create_user(
            &db,
            "valid-reviewer",
            "valid-reviewer@example.invalid",
            "",
            "Valid Reviewer",
        )
        .await
        .unwrap();
        let no_read =
            user_ops::create_user(&db, "no-read", "no-read@example.invalid", "", "No Read")
                .await
                .unwrap();
        let organization = rg_db::ops::org_ops::create_org(
            &db,
            "home-org",
            Some("Home Org"),
            None,
            owner.id,
            "private",
        )
        .await
        .unwrap();
        let now = Utc::now();
        let repository = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(owner.id),
                name: Set("diagnostic-code".into()),
                description: Set(None),
                is_private: Set(true),
                default_branch: Set("main".into()),
                fork_id: Set(None),
                stars_count: Set(0),
                forks_count: Set(0),
                org_id: Set(Some(organization.id)),
                created_at: Set(now),
                updated_at: Set(now),
                deleted_at: Set(None),
                origin_repo_id: Set(None),
            },
        )
        .await
        .unwrap();
        rg_db::ops::repo_collaborator_ops::create(
            &db,
            rg_db::entities::repo_collaborator::ActiveModel {
                id: NotSet,
                repo_id: Set(repository.id),
                user_id: Set(reviewer.id),
                permission: Set("read".into()),
                created_at: Set(now),
            },
        )
        .await
        .unwrap();
        let pull_request = rg_db::ops::pull_request_ops::create(
            &db,
            rg_db::entities::pull_request::ActiveModel {
                id: NotSet,
                repo_id: Set(repository.id),
                number: Set(1),
                title: Set("Diagnose CODEOWNERS".into()),
                body: Set(None),
                state: Set("open".into()),
                is_draft: Set(false),
                auto_merge_enabled: Set(false),
                auto_merge_strategy: Set(None),
                auto_merge_enabled_by_id: Set(None),
                auto_merge_enabled_at: Set(None),
                author_id: Set(owner.id),
                reviewer_id: Set(None),
                head_branch: Set("feature".into()),
                base_branch: Set("main".into()),
                head_sha: Set(None),
                merge_strategy: Set(None),
                merge_commit_sha: Set(None),
                head_repo_id: Set(None),
                ci_approved_sha: Set(None),
                ci_approved_by: Set(None),
                ci_approved_at: Set(None),
                milestone_id: Set(None),
                labels: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                closed_at: Set(None),
                merged_at: Set(None),
            },
        )
        .await
        .unwrap();

        let (_repository_dir, bare) = bare_repository(Some((
            ".github/CODEOWNERS",
            "*.md docs@example.com\nsrc/** @missing-reviewer @foreign-org/reviewers \
             @valid-reviewer @codeowners-owner @no-read\n",
        )));
        let outcome = request_codeowners(
            &db,
            &bare,
            "main",
            &["README.md".into(), "src/lib.rs".into()],
            &repository,
            pull_request.id,
            owner.id,
            owner.id,
        )
        .await
        .unwrap();

        assert_eq!(outcome.requested, ["valid-reviewer"]);
        assert_eq!(
            outcome.diagnostics,
            [
                CodeownersDiagnostic::new(
                    1,
                    "docs@example.com",
                    "`docs@example.com` is not an `@username` or `@org/team` owner",
                ),
                CodeownersDiagnostic::new(
                    2,
                    "@missing-reviewer",
                    "account `missing-reviewer` does not exist",
                ),
                CodeownersDiagnostic::new(
                    2,
                    "@foreign-org/reviewers",
                    "organization `foreign-org` does not own this repository (expected `home-org`)",
                ),
            ]
        );

        // The second pass reaches the existing-request filter. Together with
        // the author and no-read owners above, it must add no diagnostic noise.
        let repeated = request_codeowners(
            &db,
            &bare,
            "main",
            &["README.md".into(), "src/lib.rs".into()],
            &repository,
            pull_request.id,
            owner.id,
            owner.id,
        )
        .await
        .unwrap();
        assert!(repeated.requested.is_empty());
        assert_eq!(repeated.diagnostics, outcome.diagnostics);

        assert!(
            rg_db::ops::pr_reviewer_request_ops::find(&db, pull_request.id, no_read.id)
                .await
                .unwrap()
                .is_none(),
            "the no-read owner is intentionally filtered, not requested"
        );
    }

    #[test]
    fn missing_policy_is_none_but_missing_repository_is_an_error() {
        let (dir, bare) = bare_repository(None);
        assert_eq!(load_codeowners(&bare, "main").unwrap(), None);

        let missing = dir.path().join("disappeared.git");
        let error = load_codeowners(&missing, "main")
            .expect_err("an unavailable repository is not an absent CODEOWNERS file");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("look up CODEOWNERS candidate"),
            "{rendered}"
        );
        assert!(rendered.contains("disappeared.git"), "{rendered}");
    }
}
