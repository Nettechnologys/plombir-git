//! CODEOWNERS parsing, matching, and automatic reviewer requests.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{NotSet, Set, TransactionTrait};

use crate::committed_blob;
use crate::db::DbPools;
use rg_db::entities::pr_reviewer_request;
use rg_db::entities::repository::Model as Repository;
use rg_db::ops::{pr_reviewer_request_ops, user_ops};

/// Where a CODEOWNERS file may live, in priority order: the first path that
/// exists on the base branch is the policy and the rest are not looked at.
///
/// A list, not literals inside the loop, so `docs/codeowners.md` has
/// something to be checked against — an author has no other way to learn the
/// paths, and nothing in the product names them.
///
/// `.gitea/CODEOWNERS` is where a repository that moved here from Gitea or
/// Forgejo keeps it; without it that file was silently not a policy
/// (card_a96a2878c38c). Last, as Gitea itself tries it, so a repository that
/// already carried a file at one of the other paths keeps the one it had.
///
/// `.gitlab/CODEOWNERS` is GitLab's third path. It is read only because the
/// parser now understands GitLab's sections (card_8e1c5506d559): before that,
/// opening it would have opened the way to a file executed by half.
const CODEOWNERS_PATHS: &[&str] = &[
    ".github/CODEOWNERS",
    "CODEOWNERS",
    "docs/CODEOWNERS",
    ".gitea/CODEOWNERS",
    ".gitlab/CODEOWNERS",
];

/// The largest CODEOWNERS file this server will read in order to parse it.
///
/// The file is matched line by line, so there is no reading it in pieces: the
/// whole of it has to be in memory at once. That makes its size a memory budget
/// chosen by whoever can push to the base branch, over a path taken on every
/// pull request that is opened — and a megabyte is already four orders of
/// magnitude above the longest rule set anybody writes by hand.
///
/// Documented in `docs/codeowners.md`, and checked against it below: a file
/// that goes over this is not read, and the author of a repository has no other
/// way to learn why their reviewers stopped being requested.
const MAX_CODEOWNERS_SIZE: u64 = 1024 * 1024;

/// Bound the pattern factor of matching even when the whole file is below 1 MiB.
const MAX_CODEOWNERS_PATTERN_BYTES: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeownerRule {
    pub line: usize,
    pub pattern: String,
    pub owners: Vec<String>,
    /// The GitLab section the rule sits in: `0` for every rule above the first
    /// `[Section]` heading (a file without headings is all section `0`), then
    /// one index per distinct section name, in the order the names first
    /// appear. The last matching rule wins *within* a section; the sections'
    /// winners are then combined.
    pub section: usize,
    /// A `!pattern` line: a path it matches takes no owner from its section,
    /// whatever else in that section matches it too.
    pub exclusion: bool,
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
///
/// GitLab's dialect is part of the format, not a stranger to it: a
/// `[Section] @owners` heading starts a section whose owners are the default
/// for the lines under it that name none, `^[Section]` is the same heading
/// marked optional, and `!pattern` excludes a path from its section. Read as a
/// plain rule, a heading used to become a pattern nobody wrote and every line
/// under it an ownerless rule that was dropped (card_8e1c5506d559).
pub fn parse_codeowners(contents: &str) -> ParsedCodeowners {
    let mut parsed = ParsedCodeowners::default();
    // Section names compared without case, as GitLab merges `[Docs]` and
    // `[DOCS]` into one section; index 0 is the unnamed section above them.
    let mut section_names: Vec<String> = Vec::new();
    let mut section = 0;
    let mut heading: Option<String> = None;
    let mut default_owners: Vec<String> = Vec::new();
    for (index, source) in contents.lines().enumerate() {
        let line_number = index + 1;
        let line = strip_comment(source).trim();
        if line.is_empty() {
            continue;
        }

        if let Some(found) = section_heading(line) {
            let key = found.name.to_lowercase();
            section = match section_names.iter().position(|name| *name == key) {
                Some(at) => at + 1,
                None => {
                    section_names.push(key);
                    section_names.len()
                }
            };
            // GitLab counts code-owner approvals per section. Nothing here
            // requires a code owner's approval at all, so a count has nothing
            // to apply to — said by name rather than read as if it held.
            if let Some(approvals) = found.approvals {
                parsed.diagnostics.push(CodeownersDiagnostic::new(
                    line_number,
                    format!("[{}][{approvals}]", found.name),
                    format!(
                        "section `{}` sets an approval count, and no code-owner approval is \
                         required here: CODEOWNERS only requests reviewers, so its owners are \
                         requested and the count is not enforced",
                        found.name
                    ),
                ));
            }
            default_owners = parse_owners(
                line_number,
                found.owners.split_whitespace(),
                &mut parsed.diagnostics,
            );
            heading = Some(found.name.to_string());
            continue;
        }

        let mut fields = line.split_whitespace();
        let Some(pattern) = fields.next() else {
            continue;
        };
        if pattern.len() > MAX_CODEOWNERS_PATTERN_BYTES {
            parsed.diagnostics.push(CodeownersDiagnostic::new(
                line_number,
                pattern,
                format!("pattern exceeds the {MAX_CODEOWNERS_PATTERN_BYTES}-byte CODEOWNERS limit"),
            ));
            continue;
        }
        if has_dangling_escape(pattern) {
            parsed.diagnostics.push(CodeownersDiagnostic::new(
                line_number,
                pattern,
                "pattern ends with a backslash that escapes no character",
            ));
            continue;
        }

        if let Some(excluded) = pattern.strip_prefix('!') {
            if excluded.is_empty() {
                parsed.diagnostics.push(CodeownersDiagnostic::new(
                    line_number,
                    pattern,
                    "exclusion names no pattern",
                ));
                continue;
            }
            for declaration in fields {
                parsed.diagnostics.push(CodeownersDiagnostic::new(
                    line_number,
                    declaration,
                    format!("`{pattern}` is an exclusion: it removes owners and assigns none"),
                ));
            }
            parsed.rules.push(CodeownerRule {
                line: line_number,
                pattern: excluded.to_string(),
                owners: Vec::new(),
                section,
                exclusion: true,
            });
            continue;
        }

        let mut declarations = fields.peekable();
        let owners = if declarations.peek().is_none() {
            if default_owners.is_empty() {
                parsed.diagnostics.push(CodeownersDiagnostic::new(
                    line_number,
                    pattern,
                    match &heading {
                        Some(name) => {
                            format!(
                                "rule names no owner, and section `{name}` has no default owner"
                            )
                        }
                        None => "rule names no owner".to_string(),
                    },
                ));
            }
            default_owners.clone()
        } else {
            parse_owners(line_number, declarations, &mut parsed.diagnostics)
        };
        if !owners.is_empty() {
            parsed.rules.push(CodeownerRule {
                line: line_number,
                pattern: pattern.to_string(),
                owners,
                section,
                exclusion: false,
            });
        }
    }
    parsed
}

/// The `@owner` declarations of one line; every other declaration is
/// diagnosed and left out.
fn parse_owners<'a>(
    line: usize,
    declarations: impl IntoIterator<Item = &'a str>,
    diagnostics: &mut Vec<CodeownersDiagnostic>,
) -> Vec<String> {
    let mut owners = Vec::new();
    for declaration in declarations {
        match declaration.strip_prefix('@') {
            Some(owner) if !owner.is_empty() => owners.push(owner.to_string()),
            _ => diagnostics.push(CodeownersDiagnostic::new(
                line,
                declaration,
                format!("`{declaration}` is not an `@username` or `@org/team` owner"),
            )),
        }
    }
    owners
}

/// A GitLab section heading: `[Name]`, optionally `^`-prefixed (optional
/// section) and followed by `[n]` (approval count), then the default owners.
struct SectionHeading<'a> {
    name: &'a str,
    approvals: Option<&'a str>,
    owners: &'a str,
}

/// Read `line` as a section heading, or `None` when it is a rule.
///
/// The closing bracket has to end the token: `[id].tsx @web` is a rule for a
/// file named with brackets, not a section called `id`. A rule that really
/// starts with `[` — or with `!` — is written with a backslash in front.
fn section_heading(line: &str) -> Option<SectionHeading<'_>> {
    let rest = line.strip_prefix('^').unwrap_or(line).strip_prefix('[')?;
    let close = rest.find(']')?;
    let name = rest[..close].trim();
    if name.is_empty() {
        return None;
    }
    let mut rest = &rest[close + 1..];
    let mut approvals = None;
    if let Some(count) = rest.strip_prefix('[') {
        let close = count.find(']')?;
        approvals = Some(&count[..close]);
        rest = &count[close + 1..];
    }
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some(SectionHeading {
        name,
        approvals,
        owners: rest,
    })
}

/// Owners of the changed paths, each with the line of the rule that named it.
///
/// Per path and per section the last matching rule wins, unless an exclusion
/// in that section matches the path — then the section gives it nobody, as in
/// GitLab, where an excluded path cannot be included again further down. The
/// sections' winners are combined in the order the sections first appear.
fn owners_for_paths_with_lines(rules: &[CodeownerRule], paths: &[String]) -> Vec<(usize, String)> {
    let sections = rules.iter().map(|rule| rule.section + 1).max().unwrap_or(0);
    let mut excluded = vec![false; sections];
    let mut winners: Vec<Option<&CodeownerRule>> = vec![None; sections];
    let mut seen = HashSet::new();
    let mut owners = Vec::new();
    for path in paths {
        excluded.fill(false);
        winners.fill(None);
        for rule in rules {
            if excluded[rule.section] || !pattern_matches(&rule.pattern, path) {
                continue;
            }
            if rule.exclusion {
                excluded[rule.section] = true;
            } else {
                winners[rule.section] = Some(rule);
            }
        }
        for (rule, excluded) in winners.iter().zip(&excluded) {
            let Some(rule) = rule.filter(|_| !excluded) else {
                continue;
            };
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
    // Pinned before the first candidate is looked at, not resolved afresh per
    // read: the candidates are tried in order and the winner is then read, so a
    // branch that moves in between would answer the size from one tree and the
    // bytes from another — which is the whole of what the ceiling below is
    // supposed to bound.
    let Some(commit) = committed_blob::verified_branch_commit(git, repo_path, base_branch)
        .with_context(|| format!("look up CODEOWNERS candidate on `{base_branch}`"))?
    else {
        return Ok(None);
    };
    for candidate in CODEOWNERS_PATHS.iter().copied() {
        let Some(size) = committed_blob::blob_size(git, repo_path, &commit, candidate)
            .with_context(|| format!("look up CODEOWNERS candidate `{candidate}`"))?
        else {
            continue;
        };
        // Spent before the memory it bounds, and that ordering is the fix:
        // `cat-file blob` has no cap of its own, and neither has the gateway
        // that collects its output, so a file measured only once it is in hand
        // has already cost every byte the ceiling was declared to save.
        if size > MAX_CODEOWNERS_SIZE {
            anyhow::bail!(
                "CODEOWNERS candidate `{candidate}` is larger than {MAX_CODEOWNERS_SIZE} bytes"
            );
        }

        let object = format!("{commit}:{candidate}");
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
    db: &impl crate::db::DbPools,
    repo_path: &Path,
    base_branch: &str,
    changed_paths: &[String],
    repository: &Repository,
    pr_id: i64,
    author_id: i64,
    requested_by_id: i64,
) -> Result<CodeownerRequestOutcome> {
    let pools = db.owned();
    let db = pools.read();
    let repo_path = repo_path.to_path_buf();
    let base_branch = base_branch.to_string();
    let changed_paths = changed_paths.to_vec();
    let Some((parsed, owners)) = tokio::task::spawn_blocking(move || {
        let Some(parsed) = load_codeowners(&repo_path, &base_branch)? else {
            return Ok::<_, anyhow::Error>(None);
        };
        let owners = owners_for_paths_with_lines(&parsed.rules, &changed_paths);
        Ok(Some((parsed, owners)))
    })
    .await??
    else {
        return Ok(CodeownerRequestOutcome::default());
    };

    let mut requested = Vec::new();
    let mut diagnostics = parsed.diagnostics;
    let mut seen_users = HashSet::new();
    for (line, owner) in owners {
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

            // One CODEOWNER is one atomic unit. A broken descriptive event must
            // not leave that owner requested without its audit trail, and it
            // must not prevent independent owners later in the rule from being
            // considered.
            let transaction = db
                .begin()
                .await
                .context("db: begin CODEOWNER reviewer request")?;
            let request = pr_reviewer_request_ops::create(
                &transaction,
                pr_reviewer_request::ActiveModel {
                    id: NotSet,
                    pr_id: Set(pr_id),
                    reviewer_id: Set(user.id),
                    requested_by_id: Set(requested_by_id),
                    created_at: Set(Utc::now()),
                },
            )
            .await?;
            if let Err(error) = rg_db::ops::pr_event_ops::record(
                &transaction,
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
            .await
            {
                let error_chain = format!("{error:#}");
                if let Err(rollback_error) = transaction.rollback().await {
                    return Err(error).context(format!(
                        "CODEOWNER @{owner} timeline write failed and its reviewer request could \
                         not be rolled back: {rollback_error}"
                    ));
                }
                tracing::error!(
                    repo_id = repository.id,
                    pr_id,
                    reviewer_id = user.id,
                    reviewer = %user.username,
                    line,
                    error = %error_chain,
                    "CODEOWNER reviewer request was rolled back because its timeline event could not be recorded"
                );
                continue;
            }
            transaction
                .commit()
                .await
                .context("db: commit CODEOWNER reviewer request")?;
            crate::review::service::notify_review_requested(
                &pools,
                pr_id,
                user.id,
                requested_by_id,
            );
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
    #[derive(Clone, Copy)]
    enum Token {
        Byte(u8),
        Any,
        Star,
        DoubleStar,
    }

    struct States {
        positions: Vec<usize>,
        present: Vec<bool>,
    }

    impl States {
        fn new(accept: usize) -> Self {
            Self {
                positions: Vec::new(),
                present: vec![false; accept + 1],
            }
        }

        fn insert(&mut self, tokens: &[Token], mut position: usize) {
            while !self.present[position] {
                self.present[position] = true;
                self.positions.push(position);
                match tokens.get(position) {
                    Some(Token::Star | Token::DoubleStar) => position += 1,
                    _ => return,
                }
            }
        }

        fn clear(&mut self) {
            for position in self.positions.drain(..) {
                self.present[position] = false;
            }
        }
    }

    // An escape consumes the following byte literally. Runs of at least two
    // stars are one DoubleStar, as in the former matcher.
    let mut tokens = Vec::with_capacity(pattern.len());
    let mut at = 0;
    while at < pattern.len() {
        match pattern[at] {
            b'\\' if at + 1 < pattern.len() => {
                tokens.push(Token::Byte(pattern[at + 1]));
                at += 2;
            }
            b'*' if pattern.get(at + 1) == Some(&b'*') => {
                tokens.push(Token::DoubleStar);
                at += 2;
                while pattern.get(at) == Some(&b'*') {
                    at += 1;
                }
            }
            b'*' => {
                tokens.push(Token::Star);
                at += 1;
            }
            b'?' => {
                tokens.push(Token::Any);
                at += 1;
            }
            byte => {
                tokens.push(Token::Byte(byte));
                at += 1;
            }
        }
    }

    // Each active position is inserted at most once per input byte. A star
    // also admits its successor without consuming input (epsilon closure).
    let mut live = States::new(tokens.len());
    let mut next = States::new(tokens.len());
    live.insert(&tokens, 0);
    for &byte in value {
        next.clear();
        for &position in &live.positions {
            match tokens.get(position) {
                Some(Token::Byte(expected)) if byte == *expected => {
                    next.insert(&tokens, position + 1)
                }
                Some(Token::Any) if byte != b'/' => next.insert(&tokens, position + 1),
                Some(Token::Star) if byte != b'/' => next.insert(&tokens, position),
                Some(Token::DoubleStar) => next.insert(&tokens, position),
                _ => {}
            }
        }
        if next.positions.is_empty() {
            return false;
        }
        std::mem::swap(&mut live, &mut next);
    }
    live.present[tokens.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::ConnectionTrait as _;
    use std::collections::BTreeSet;
    use std::time::Duration;

    // The former memoized backtracker is kept only as a small-input oracle.
    // It must never receive an unbounded repository path.
    fn old_glob_matches(pattern: &[u8], value: &[u8]) -> bool {
        fn from(
            pattern: &[u8],
            value: &[u8],
            p: usize,
            v: usize,
            failed: &mut HashSet<(usize, usize)>,
        ) -> bool {
            if !failed.insert((p, v)) {
                return false;
            }
            if p == pattern.len() {
                return v == value.len();
            }
            match pattern[p] {
                b'\\' if p + 1 < pattern.len() => {
                    value.get(v) == Some(&pattern[p + 1])
                        && from(pattern, value, p + 2, v + 1, failed)
                }
                b'*' if pattern.get(p + 1) == Some(&b'*') => {
                    let mut next = p + 2;
                    while pattern.get(next) == Some(&b'*') {
                        next += 1;
                    }
                    (v..=value.len()).any(|at| from(pattern, value, next, at, failed))
                }
                b'*' => {
                    let end = value[v..]
                        .iter()
                        .position(|byte| *byte == b'/')
                        .map_or(value.len(), |offset| v + offset);
                    (v..=end).any(|at| from(pattern, value, p + 1, at, failed))
                }
                b'?' if v < value.len() && value[v] != b'/' => {
                    from(pattern, value, p + 1, v + 1, failed)
                }
                byte if value.get(v) == Some(&byte) => from(pattern, value, p + 1, v + 1, failed),
                _ => false,
            }
        }
        from(pattern, value, 0, 0, &mut HashSet::new())
    }

    #[test]
    fn iterative_glob_agrees_with_old_matcher_on_short_inputs() {
        fn words(alphabet: &[u8], max_len: usize) -> Vec<Vec<u8>> {
            let mut all = vec![Vec::new()];
            let mut level = vec![Vec::new()];
            for _ in 0..max_len {
                level = level
                    .into_iter()
                    .flat_map(|prefix| {
                        alphabet.iter().map(move |&byte| {
                            let mut word = prefix.clone();
                            word.push(byte);
                            word
                        })
                    })
                    .collect();
                all.extend(level.iter().cloned());
            }
            all
        }
        let patterns = words(b"a/*?\\b", 4);
        let values = words(b"a/*?", 3);
        for pattern in &patterns {
            for value in &values {
                assert_eq!(
                    glob_matches(pattern, value),
                    old_glob_matches(pattern, value),
                    "pattern={pattern:?}, value={value:?}"
                );
            }
        }
    }

    #[test]
    fn pathological_codeowners_pattern_finishes_within_deadline() {
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let pattern = format!("{}b", "**a".repeat(40));
            let path = "a".repeat(2048);
            sender
                .send(glob_matches(pattern.as_bytes(), path.as_bytes()))
                .unwrap();
        });
        assert_eq!(receiver.recv_timeout(Duration::from_secs(2)), Ok(false));
    }

    #[test]
    fn long_literal_does_not_recurse_on_a_small_stack() {
        std::thread::Builder::new()
            .stack_size(64 * 1024)
            .spawn(|| {
                let literal = vec![b'a'; 32 * 1024];
                assert!(glob_matches(&literal, &literal));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn oversized_codeowners_pattern_is_diagnosed_and_dropped() {
        let at_limit = "a".repeat(MAX_CODEOWNERS_PATTERN_BYTES);
        let too_long = "a".repeat(MAX_CODEOWNERS_PATTERN_BYTES + 1);
        let parsed = parse_codeowners(&format!("{at_limit} @alice\n{too_long} @bob\n"));
        assert_eq!(parsed.rules.len(), 1);
        assert_eq!(parsed.rules[0].pattern, at_limit);
        assert_eq!(parsed.diagnostics.len(), 1);
        assert_eq!(parsed.diagnostics[0].line, 2);
        assert!(parsed.diagnostics[0]
            .reason
            .contains("4096-byte CODEOWNERS limit"));
    }

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

    /// The paths are the whole of "where does this file go?", and none of
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
        assert_worked_example(
            "<!-- example: codeowners-file -->",
            "<!-- examples: codeowners-resolution -->",
            4,
        );
    }

    /// The same, for the GitLab sections example: which section contributes
    /// what is the part of that dialect no experiment on one rule shows.
    #[test]
    fn the_sections_example_resolves_to_the_owners_the_documentation_promises() {
        assert_worked_example(
            "<!-- example: codeowners-sections-file -->",
            "<!-- examples: codeowners-sections-resolution -->",
            5,
        );
    }

    fn assert_worked_example(file_marker: &str, resolution_marker: &str, min_rules: usize) {
        let (name, content) = CODEOWNERS_DOCUMENTATION;
        let rules = parse_codeowners(&fenced_block_after(name, content, file_marker));
        assert!(
            rules.rules.len() >= min_rules,
            "{name}: only {} rules parsed out of the example after `{file_marker}` — a rule the \
             page shows is being dropped by the parser it is supposed to illustrate",
            rules.rules.len()
        );
        assert!(rules.diagnostics.is_empty(), "{:?}", rules.diagnostics);

        let rows = table_rows_after(name, content, resolution_marker);
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
            "only {} resolution examples read after `{resolution_marker}` in {name} — the table \
             scanner has stopped matching",
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
        let files: Vec<(&str, &str)> = codeowners.into_iter().collect();
        bare_repository_with(&files)
    }

    fn bare_repository_with(files: &[(&str, &str)]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        let bare = dir.path().join("repo.git");
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();

        git.run_or_bail(&["init", "-q", "-b", "main", work.to_str().unwrap()], None)
            .unwrap();
        if !files.is_empty() {
            for (path, contents) in files {
                let file = work.join(path);
                std::fs::create_dir_all(file.parent().unwrap()).unwrap();
                std::fs::write(file, contents).unwrap();
            }
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

    /// card_8e1c5506d559: a GitLab heading used to become the rule `[Docs]`
    /// and every ownerless line under it a diagnostic, so `docs/` reached
    /// nobody. The heading's owners are the default for the lines below it.
    #[test]
    fn a_gitlab_section_heading_is_a_default_owner_not_a_rule() {
        let parsed = parse_codeowners("[Docs] @docs-team\ndocs/\n");
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert!(
            parsed.rules.iter().all(|rule| rule.pattern != "[Docs]"),
            "the heading must not be a pattern: {:?}",
            parsed.rules
        );
        assert_eq!(
            owners_for_paths(&parsed.rules, &["docs/readme.md".into()]),
            ["docs-team"]
        );

        // Owners written on the line win over the section's default.
        let parsed = parse_codeowners("^[Docs] @docs-team\ndocs/ @alice\n*.md\n");
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert_eq!(
            owners_for_paths(&parsed.rules, &["docs/guide.txt".into()]),
            ["alice"]
        );
        assert_eq!(
            owners_for_paths(&parsed.rules, &["README.md".into()]),
            ["docs-team"]
        );
    }

    /// GitLab's own worked example: the last match wins inside a section, and
    /// the winners of every section — the unnamed one above the first heading
    /// included — are combined.
    #[test]
    fn each_section_contributes_its_last_matching_rule() {
        let parsed = parse_codeowners(
            "* @admin\n\n[README Owners]\nREADME.md @user1 @user2\ninternal/README.md @user4\n\n\
             [README other owners]\nREADME.md @user3\n",
        );
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert_eq!(
            owners_for_paths(&parsed.rules, &["internal/README.md".into()]),
            ["admin", "user4", "user3"]
        );
        assert_eq!(
            owners_for_paths(&parsed.rules, &["README.md".into()]),
            ["admin", "user1", "user2", "user3"]
        );
        assert_eq!(
            owners_for_paths(&parsed.rules, &["src/lib.rs".into()]),
            ["admin"]
        );
    }

    /// `[Docs]` and `[DOCS]` are one section, so the later line wins over the
    /// earlier one instead of both being requested.
    #[test]
    fn section_names_are_one_section_whatever_their_case() {
        let parsed = parse_codeowners("[Docs]\n*.md @writers\n[DOCS]\nguide.md @editor\n");
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert_eq!(
            owners_for_paths(&parsed.rules, &["guide.md".into()]),
            ["editor"]
        );
        assert_eq!(
            owners_for_paths(&parsed.rules, &["notes.md".into()]),
            ["writers"]
        );
    }

    /// `!pattern` used to be a rule for a path that starts with `!`. In GitLab
    /// it takes a path out of its section, and nothing later in that section
    /// takes it back; other sections are unaffected.
    #[test]
    fn an_exclusion_takes_a_path_out_of_its_section_only() {
        let parsed = parse_codeowners(
            "* @admin\n!pom.xml\n[Ruby] @ruby-team\n*.rb\n!/config/**/*.rb\n/config/ @ops\n",
        );
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert!(owners_for_paths(&parsed.rules, &["pom.xml".into()]).is_empty());
        assert_eq!(
            owners_for_paths(&parsed.rules, &["lib/app.rb".into()]),
            ["admin", "ruby-team"]
        );
        assert_eq!(
            owners_for_paths(&parsed.rules, &["config/env/prod.rb".into()]),
            ["admin"],
            "excluded in `Ruby`, and the later `/config/` cannot include it again"
        );

        let literal = parse_codeowners("\\!important @alice\n");
        assert!(literal.diagnostics.is_empty(), "{:?}", literal.diagnostics);
        assert_eq!(
            owners_for_paths(&literal.rules, &["!important".into()]),
            ["alice"],
            "an escaped `!` is a byte of the file name"
        );
    }

    /// What a heading or exclusion cannot carry is said by name, and a token
    /// that only looks like a heading stays a rule.
    #[test]
    fn what_a_section_cannot_execute_is_diagnosed_by_name() {
        let parsed = parse_codeowners(
            "[Docs][2] @writers\ndocs/\n[Empty]\nsrc/\n!vendor/ @bob\n!\n[id].tsx @web\n\\[Docs] @carol\n",
        );
        let reasons: Vec<(usize, &str, &str)> = parsed
            .diagnostics
            .iter()
            .map(|d| (d.line, d.declaration.as_str(), d.reason.as_str()))
            .collect();
        assert_eq!(reasons.len(), 4, "{reasons:?}");
        assert_eq!((reasons[0].0, reasons[0].1), (1, "[Docs][2]"));
        assert!(reasons[0].2.contains("approval count"), "{reasons:?}");
        assert_eq!((reasons[1].0, reasons[1].1), (4, "src/"));
        assert!(reasons[1].2.contains("section `Empty`"), "{reasons:?}");
        assert_eq!((reasons[2].0, reasons[2].1), (5, "@bob"));
        assert!(reasons[2].2.contains("exclusion"), "{reasons:?}");
        assert_eq!(reasons[3], (6, "!", "exclusion names no pattern"));

        // The count is not enforced, but the section's owners still are requested.
        assert_eq!(
            owners_for_paths(&parsed.rules, &["docs/a.md".into()]),
            ["writers"]
        );
        assert_eq!(
            owners_for_paths(&parsed.rules, &["[id].tsx".into()]),
            ["web"]
        );
        assert_eq!(
            owners_for_paths(&parsed.rules, &["[Docs]".into()]),
            ["carol"]
        );
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
                    section: 0,
                    exclusion: false,
                }],
                diagnostics: Vec::new(),
            })
        );
    }

    /// card_a96a2878c38c: a repository that moved here from Gitea keeps its
    /// file in `.gitea/`, and that file alone is the policy.
    #[test]
    fn a_gitea_codeowners_file_is_the_policy_when_it_is_the_only_one() {
        let (_dir, bare) = bare_repository(Some((".gitea/CODEOWNERS", "*.rs @rust\n")));

        assert_eq!(
            load_codeowners(&bare, "main").unwrap(),
            Some(ParsedCodeowners {
                rules: vec![CodeownerRule {
                    line: 1,
                    pattern: "*.rs".into(),
                    owners: vec!["rust".into()],
                    section: 0,
                    exclusion: false,
                }],
                diagnostics: Vec::new(),
            })
        );
    }

    /// card_8e1c5506d559: GitLab's third path, read together with its sections.
    #[test]
    fn a_gitlab_codeowners_file_is_the_policy_when_it_is_the_only_one() {
        let (_dir, bare) =
            bare_repository(Some((".gitlab/CODEOWNERS", "[Docs] @docs-team\ndocs/\n")));

        let parsed = load_codeowners(&bare, "main").unwrap().expect("a policy");
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert_eq!(
            owners_for_paths(&parsed.rules, &["docs/readme.md".into()]),
            ["docs-team"]
        );
    }

    /// Adding the Gitea path must not change which file wins in a repository
    /// that already had one at a path read before it.
    #[test]
    fn a_gitea_codeowners_file_does_not_outrank_the_paths_read_before_it() {
        let (_dir, bare) = bare_repository_with(&[
            (".gitea/CODEOWNERS", "*.rs @gitea-owner\n"),
            ("docs/CODEOWNERS", "*.rs @rust\n"),
        ]);

        let owners: Vec<String> = load_codeowners(&bare, "main")
            .unwrap()
            .expect("a policy")
            .rules
            .into_iter()
            .flat_map(|rule| rule.owners)
            .collect();
        assert_eq!(owners, ["rust"]);
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
                base_sha: Set(None),
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
                original_author: NotSet,
                original_author_id: NotSet,
                original_service: NotSet,
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

    /// One owner's event failure is local to that owner: its request rolls
    /// back, while later independent owners are still processed. Returning at
    /// the failed event used to leave the first request committed and skip the
    /// second owner entirely.
    #[tokio::test]
    async fn a_broken_owner_event_rolls_back_that_request_and_keeps_the_fan_out_running() {
        let db = crate::test_support::migrated_memory_database().await;
        let owner = user_ops::create_user(
            &db,
            "fanout-owner",
            "fanout-owner@example.invalid",
            "",
            "Fanout Owner",
        )
        .await
        .unwrap();
        let first = user_ops::create_user(
            &db,
            "first-reviewer",
            "first-reviewer@example.invalid",
            "",
            "First Reviewer",
        )
        .await
        .unwrap();
        let second = user_ops::create_user(
            &db,
            "second-reviewer",
            "second-reviewer@example.invalid",
            "",
            "Second Reviewer",
        )
        .await
        .unwrap();
        let now = Utc::now();
        let repository = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(owner.id),
                name: Set("fanout-code".into()),
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
                title: Set("Keep CODEOWNERS fan-out running".into()),
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
                base_sha: Set(None),
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
                original_author: NotSet,
                original_author_id: NotSet,
                original_service: NotSet,
            },
        )
        .await
        .unwrap();
        let (_repository_dir, bare) = bare_repository(Some((
            ".github/CODEOWNERS",
            "src/** @first-reviewer @second-reviewer\n",
        )));
        db.execute_unprepared(
            "CREATE TRIGGER first_codeowner_event_outage BEFORE INSERT ON pr_events \
             WHEN new.event_type = 'reviewer_requested' \
              AND instr(new.metadata, '\"reviewer\":\"first-reviewer\"') > 0 \
             BEGIN SELECT RAISE(ABORT, 'timeline storage is unavailable'); END;",
        )
        .await
        .expect("reject only the first CODEOWNER event");

        let outcome = request_codeowners(
            &db,
            &bare,
            "main",
            &["src/lib.rs".into()],
            &repository,
            pull_request.id,
            owner.id,
            owner.id,
        )
        .await
        .expect("one broken owner event is an advisory per-owner failure");

        assert_eq!(outcome.requested, ["second-reviewer"]);
        assert!(outcome.diagnostics.is_empty());
        assert!(
            pr_reviewer_request_ops::find(&db, pull_request.id, first.id)
                .await
                .unwrap()
                .is_none(),
            "the owner whose event failed must not be left partially requested"
        );
        assert!(
            pr_reviewer_request_ops::find(&db, pull_request.id, second.id)
                .await
                .unwrap()
                .is_some(),
            "the event failure must not skip later CODEOWNERS"
        );
        let events = rg_db::ops::pr_event_ops::list_by_pr(&db, pull_request.id)
            .await
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "reviewer_requested");
        assert!(events[0].metadata.contains("second-reviewer"));
    }

    /// The ceiling is a rule an author of a repository has to be able to read,
    /// because failing it is invisible from their side: the reviewers simply
    /// stop being requested. The number is taken off the constant the loader
    /// enforces, so raising or lowering it without touching the page fails
    /// here rather than leaving the page describing a limit that is not the
    /// one applied.
    #[test]
    fn the_size_limit_a_codeowners_file_must_stay_under_is_documented() {
        let (name, content) = CODEOWNERS_DOCUMENTATION;
        let documented =
            fenced_block_after(name, content, "<!-- inventory: codeowners-size-ceiling -->");
        let documented: Vec<&str> = documented
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();

        assert_eq!(
            documented.len(),
            1,
            "{name}: the size-ceiling inventory holds {documented:?}, and it says how many bytes \
             a CODEOWNERS file may have — one number, or an author cannot tell which one binds"
        );
        let documented: u64 = documented[0].parse().unwrap_or_else(|error| {
            panic!(
                "{name}: `{}` is not a number of bytes: {error}",
                documented[0]
            )
        });

        assert_eq!(
            documented, MAX_CODEOWNERS_SIZE,
            "{name} promises that a CODEOWNERS file may be {documented} bytes, and the loader \
             refuses one over {MAX_CODEOWNERS_SIZE} — an author reading this page commits a file \
             that assigns nobody and is told nothing"
        );
    }

    /// The size of a committed file is chosen by whoever can push to the base
    /// branch, and this path runs on every pull request that is opened. The
    /// refusal is what makes it a budget of this server's rather than theirs.
    #[test]
    fn an_oversized_codeowners_file_is_refused_by_its_ceiling() {
        const RULE: &str = "* @owner\n";
        let oversized = RULE.repeat(MAX_CODEOWNERS_SIZE as usize / RULE.len() + 1);
        assert!(oversized.len() as u64 > MAX_CODEOWNERS_SIZE);

        let (_dir, bare) = bare_repository(Some((".github/CODEOWNERS", &oversized)));
        let error = load_codeowners(&bare, "main")
            .expect_err("a CODEOWNERS file over the ceiling is refused, not parsed");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(&format!("larger than {MAX_CODEOWNERS_SIZE} bytes")),
            "{rendered}"
        );
        assert!(rendered.contains(".github/CODEOWNERS"), "{rendered}");
    }

    /// The ordering is the whole of the fix above, and the test before it
    /// cannot see it: a ceiling spent after `git cat-file blob` has collected
    /// the blob refuses the same file with the same message, having first paid
    /// the memory it was declared to save. `cat-file` has no cap of its own and
    /// neither has the gateway that collects its output, so the order is
    /// asserted where it lives.
    #[test]
    fn the_codeowners_ceiling_is_spent_before_the_blob_is_read() {
        let code = rust_source::production_rust_code_only(include_str!("codeowners.rs"));
        let start = code
            .find("fn load_codeowners(")
            .expect("`load_codeowners` must still be the one reader of a CODEOWNERS blob");
        let body = &code[start..];
        let end = body
            .find("\n}")
            .map(|offset| offset + 2)
            .unwrap_or(body.len());
        let body = &body[..end];

        let ceiling = body.find("MAX_CODEOWNERS_SIZE").expect(
            "`load_codeowners` no longer names `MAX_CODEOWNERS_SIZE`: nothing bounds the blob it \
             is about to read into memory",
        );
        // The anchor is the call, not the `cat-file` literal: literals are
        // blanked in this view, and so is the comment above the check.
        let read = body.find("git.run(").expect(
            "`load_codeowners` no longer runs git directly — the anchor this ordering is \
             asserted against has moved, so the assertion below proves nothing",
        );
        assert!(
            ceiling < read,
            "`load_codeowners` compares against `MAX_CODEOWNERS_SIZE` only after `git.run` has \
             collected the blob: a 5 GiB file committed at a CODEOWNERS path is then materialised \
             in full and refused afterwards"
        );
    }

    /// A tree committed at one of the candidate paths is not a CODEOWNERS file.
    /// Reading the size first is what makes it possible to say so: the reader
    /// used to hand the path straight to `cat-file blob`, which failed there
    /// and took the whole lookup down with it, so a directory named
    /// `.github/CODEOWNERS` hid a real file at one of the later paths.
    #[test]
    fn a_directory_at_a_candidate_path_does_not_hide_the_real_file() {
        let (_dir, bare) = bare_repository_with(&[
            (".github/CODEOWNERS/notes.md", "not a policy\n"),
            ("CODEOWNERS", "*.rs @rust\n"),
        ]);

        assert_eq!(
            load_codeowners(&bare, "main").unwrap(),
            Some(ParsedCodeowners {
                rules: vec![CodeownerRule {
                    line: 1,
                    pattern: "*.rs".into(),
                    owners: vec!["rust".into()],
                    section: 0,
                    exclusion: false,
                }],
                diagnostics: Vec::new(),
            })
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
