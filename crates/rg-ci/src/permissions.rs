//! `permissions:` — what a job's `CI_JOB_TOKEN` may do, as an Actions
//! workflow declares it at the workflow root or on a job.
//!
//! The token here is narrower than GitHub's `GITHUB_TOKEN` to begin with: it
//! reads the repository (`contents`) and its packages (`packages`), and with
//! `id-token: write` it may be exchanged for a workload-identity token at
//! `CI_OIDC_TOKEN_URL`. It can write nothing. So a declaration is honoured
//! where it narrows, and refused by name where it asks for a write the token
//! cannot do — a workflow that relies on pushing with the job token fails when
//! it is read, not halfway through a release.
//!
//! The other scopes GitHub knows — `issues`, `pull-requests`, `checks` and the
//! rest — name APIs the token never reaches: `read` and `none` are accepted and
//! change nothing, `write` is refused like the others.

use std::collections::BTreeMap;

/// Every scope GitHub's `permissions:` accepts.
pub(crate) const KNOWN_SCOPES: &[&str] = &[
    "actions",
    "attestations",
    "checks",
    "contents",
    "deployments",
    "discussions",
    "id-token",
    "issues",
    "models",
    "packages",
    "pages",
    "pull-requests",
    "repository-projects",
    "security-events",
    "statuses",
];

/// One scope's level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Level {
    None,
    Read,
    Write,
}

impl Level {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "read" => Some(Self::Read),
            "write" => Some(Self::Write),
            _ => None,
        }
    }
}

/// A `permissions:` block, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Permissions {
    /// Scope → level; a scope the block does not name is `none`, as on GitHub.
    levels: BTreeMap<&'static str, Level>,
}

impl Permissions {
    /// Read a `permissions:` value: `read-all`, `write-all`, `{}` or a map of
    /// scope → `read` / `write` / `none`. `at` names where it was written.
    pub(crate) fn parse(value: &serde_json::Value, at: &str) -> Result<Self, String> {
        let all = |level| Self {
            levels: KNOWN_SCOPES.iter().map(|scope| (*scope, level)).collect(),
        };
        let permissions = match value {
            serde_json::Value::String(text) if text == "read-all" => all(Level::Read),
            serde_json::Value::String(text) if text == "write-all" => all(Level::Write),
            serde_json::Value::Object(map) => {
                let mut levels = BTreeMap::new();
                for (scope, level) in map {
                    let Some(known) = KNOWN_SCOPES.iter().find(|known| **known == scope) else {
                        return Err(format!("{at}.{scope} is not a permission scope"));
                    };
                    let parsed = level
                        .as_str()
                        .and_then(Level::parse)
                        .ok_or_else(|| format!("{at}.{scope} must be `read`, `write` or `none`"))?;
                    levels.insert(*known, parsed);
                }
                Self { levels }
            }
            _ => {
                return Err(format!(
                    "{at} must be `read-all`, `write-all` or a map of scope to level"
                ))
            }
        };
        if let Some(scope) = permissions
            .levels
            .iter()
            .find(|(scope, level)| **level == Level::Write && **scope != "id-token")
            .map(|(scope, _)| *scope)
        {
            return Err(format!(
                "{at} asks for `{scope}: write`, and the job token here cannot write — it reads \
                 the repository and its packages and nothing else. Push, release or comment \
                 with a deploy key or a token kept in a secret; `read` and `none` are honoured"
            ));
        }
        Ok(permissions)
    }

    fn level(&self, scope: &str) -> Level {
        self.levels.get(scope).copied().unwrap_or(Level::None)
    }

    /// The narrower of two declarations, scope by scope — what a reusable
    /// workflow's job gets under a caller job that declares its own.
    pub(crate) fn intersect(&self, other: &Self) -> Self {
        Self {
            levels: KNOWN_SCOPES
                .iter()
                .map(|scope| (*scope, self.level(scope).min(other.level(scope))))
                .collect(),
        }
    }

    /// The `CI_JOB_TOKEN` scopes this declaration leaves.
    pub(crate) fn token_scopes(&self) -> String {
        let mut scopes = Vec::new();
        if self.level("contents") >= Level::Read {
            scopes.push("repo:read");
        }
        if self.level("packages") >= Level::Read {
            scopes.push("packages:read");
        }
        if self.level("id-token") == Level::Write {
            scopes.push("id-token:write");
        }
        scopes.join(" ")
    }
}

impl Default for Permissions {
    /// What a job that declares nothing gets — GitHub's restricted default:
    /// `contents: read`, `packages: read`, no workload identity
    /// ([`rg_core::ci::DEFAULT_ACTIONS_JOB_TOKEN_SCOPES`]).
    fn default() -> Self {
        Self {
            levels: [("contents", Level::Read), ("packages", Level::Read)]
                .into_iter()
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> Result<Permissions, String> {
        let value: serde_json::Value = rg_core::yaml::from_str(yaml).unwrap();
        Permissions::parse(&value, "permissions")
    }

    #[test]
    fn a_declaration_narrows_the_token_and_a_write_it_cannot_do_is_refused() {
        assert_eq!(parse("contents: read").unwrap().token_scopes(), "repo:read");
        assert_eq!(parse("{}").unwrap().token_scopes(), "");
        assert_eq!(
            parse("read-all").unwrap().token_scopes(),
            "repo:read packages:read"
        );
        assert_eq!(
            parse(
                "contents: read
id-token: write
issues: none"
            )
            .unwrap()
            .token_scopes(),
            "repo:read id-token:write"
        );
        assert_eq!(
            Permissions::default().token_scopes(),
            rg_core::ci::DEFAULT_ACTIONS_JOB_TOKEN_SCOPES
        );
        for (yaml, named) in [
            ("contents: write", "contents: write"),
            ("write-all", ": write"),
            ("pull-requests: write", "pull-requests: write"),
            ("contents: admin", "permissions.contents must be"),
            (
                "secrets: read",
                "permissions.secrets is not a permission scope",
            ),
            ("[contents]", "must be"),
        ] {
            let error = parse(yaml).unwrap_err();
            assert!(error.contains(named), "{yaml}: {error}");
        }
    }

    #[test]
    fn a_callee_never_gets_more_than_its_caller() {
        let caller = parse("contents: read").unwrap();
        let callee = parse(
            "contents: read
packages: read
id-token: write",
        )
        .unwrap();
        assert_eq!(callee.intersect(&caller).token_scopes(), "repo:read");
    }
}
