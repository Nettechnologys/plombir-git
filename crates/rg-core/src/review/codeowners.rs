//! CODEOWNERS parsing, matching, and automatic reviewer requests.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{DatabaseConnection, NotSet, Set};

use rg_db::entities::pr_reviewer_request;
use rg_db::entities::repository::Model as Repository;
use rg_db::ops::{pr_reviewer_request_ops, user_ops};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeownerRule {
    pub pattern: String,
    pub owners: Vec<String>,
}

/// Parse a CODEOWNERS file. Unsupported team owners are retained by the parser
/// but ignored when reviewer accounts are resolved.
pub fn parse_codeowners(contents: &str) -> Vec<CodeownerRule> {
    contents
        .lines()
        .filter_map(|line| {
            let line = strip_comment(line).trim();
            if line.is_empty() {
                return None;
            }
            let mut fields = line.split_whitespace();
            let pattern = fields.next()?.to_string();
            let owners = fields
                .filter_map(|owner| owner.strip_prefix('@'))
                .filter(|owner| !owner.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>();
            (!owners.is_empty()).then_some(CodeownerRule { pattern, owners })
        })
        .collect()
}

/// Resolve owners for changed paths. As in GitHub CODEOWNERS, the last
/// matching rule wins for each path.
pub fn owners_for_paths(rules: &[CodeownerRule], paths: &[String]) -> Vec<String> {
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
                    owners.push(owner.clone());
                }
            }
        }
    }
    owners
}

/// Load CODEOWNERS from the base branch using the standard location priority.
pub fn load_codeowners(repo_path: &Path, base_branch: &str) -> Result<Option<Vec<CodeownerRule>>> {
    let git = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    for candidate in [".github/CODEOWNERS", "CODEOWNERS", "docs/CODEOWNERS"] {
        let object = format!("refs/heads/{base_branch}:{candidate}");
        let output = git.run(&["show", &object], Some(repo_path))?;
        if output.success() {
            return Ok(Some(parse_codeowners(&output.stdout_str())));
        }
    }
    Ok(None)
}

/// Request readable, active user accounts selected by CODEOWNERS. A team owner
/// (`@org/team`) is accepted only for this repository's organization and only
/// when the team has write/admin permission.
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
) -> Result<Vec<String>> {
    let repo_path = repo_path.to_path_buf();
    let base_branch = base_branch.to_string();
    let Some(rules) =
        tokio::task::spawn_blocking(move || load_codeowners(&repo_path, &base_branch)).await??
    else {
        return Ok(Vec::new());
    };

    let mut requested = Vec::new();
    let mut seen_users = HashSet::new();
    for owner in owners_for_paths(&rules, changed_paths) {
        let mut candidates = Vec::new();
        if let Some((org_name, team_name)) = owner.split_once('/') {
            if team_name.contains('/') {
                continue;
            }
            let Some(org_id) = repository.org_id else {
                continue;
            };
            let Some(org) = rg_db::ops::org_ops::get_org(db, org_id).await? else {
                continue;
            };
            if !org.name.eq_ignore_ascii_case(org_name) {
                continue;
            }
            let Some(team) = rg_db::ops::org_ops::find_team_by_name(db, org_id, team_name).await?
            else {
                continue;
            };
            if !matches!(team.permission.as_str(), "write" | "admin") {
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
            if !crate::repo::service::can_read_repo(db, repository, Some(user.id))
                .await
                .unwrap_or(false)
            {
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
    Ok(requested)
}

fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'#' && (index == 0 || bytes[index - 1] != b'\\') {
            return &line[..index];
        }
    }
    line
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

    #[test]
    fn parser_ignores_comments_and_preserves_owner_order() {
        let rules = parse_codeowners(
            "# defaults\n* @alice\n/docs/ @writers @org/docs # prose\n*.rs @rustacean\n",
        );
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[1].pattern, "/docs/");
        assert_eq!(rules[1].owners, ["writers", "org/docs"]);
    }

    #[test]
    fn last_matching_rule_wins_per_path() {
        let rules = parse_codeowners("* @default\n*.rs @rust\n/src/api/** @api\n");
        let owners = owners_for_paths(&rules, &["src/api/pulls.rs".into(), "README.md".into()]);
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
}
