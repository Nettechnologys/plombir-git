//! Personal Access Token scope parsing and enforcement.

use anyhow::{bail, Result};

/// Supported PAT scopes. `admin` permits every scope but never bypasses the
/// authenticated user's own administrator/repository permissions.
pub const ALLOWED_SCOPES: &[&str] = &["repo", "user", "admin"];

/// Parse, validate and canonicalize a comma/whitespace separated scope list.
pub fn normalize_scopes(input: &str) -> Result<String> {
    let requested: std::collections::HashSet<&str> = input
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|scope| !scope.is_empty())
        .collect();

    if requested.is_empty() {
        bail!("at least one token scope is required");
    }
    if let Some(scope) = requested
        .iter()
        .find(|scope| !ALLOWED_SCOPES.contains(scope))
    {
        bail!(
            "unsupported token scope '{}'; allowed scopes: {}",
            scope,
            ALLOWED_SCOPES.join(", ")
        );
    }

    Ok(ALLOWED_SCOPES
        .iter()
        .filter(|scope| requested.contains(**scope))
        .copied()
        .collect::<Vec<_>>()
        .join(","))
}

/// Return whether the stored scopes grant the required scope.
pub fn has_scope(granted: &str, required: &str) -> bool {
    granted
        .split(|c: char| c == ',' || c.is_whitespace())
        .map(str::trim)
        .any(|scope| scope == "admin" || scope == required)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_and_orders_scopes() {
        assert_eq!(normalize_scopes("user repo,repo").unwrap(), "repo,user");
    }

    #[test]
    fn rejects_unknown_or_empty_scopes() {
        assert!(normalize_scopes(" ").is_err());
        assert!(normalize_scopes("repo,delete_everything").is_err());
    }

    #[test]
    fn admin_implies_all_scopes() {
        assert!(has_scope("admin", "repo"));
        assert!(has_scope("admin", "user"));
        assert!(has_scope("repo,user", "user"));
        assert!(!has_scope("user", "repo"));
    }
}
