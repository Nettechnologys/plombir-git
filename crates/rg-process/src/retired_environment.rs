//! Refuse to start while the environment still carries the variables of the
//! project's former name.
//!
//! Plombir Git was called ForgeKeep, and the rename kept no compatibility
//! layer: `FORGEKEEP_*` variables are not read any more. Ignoring them silently
//! is the dangerous half of that decision, because an unread variable is
//! indistinguishable from an unset one. A forgotten `FORGEKEEP_CORS_ORIGINS`
//! falls back to reflecting any origin, a forgotten `FORGEKEEP_DATABASE_URL`
//! points `migrate` at a fresh default database, and a forgotten
//! `FORGEKEEP_URL` sends the MCP server's requests to `localhost`. So every
//! binary asks this module first and stops with the list of names to rename.
//!
//! Only names are reported, never values: these variables carry the JWT secret
//! and the at-rest encryption key.

use std::ffi::OsString;
use std::fmt;

/// The prefix every variable carried before the rename.
const RETIRED_PREFIX: &str = "FORGEKEEP_";

/// The prefix that replaced it.
const CURRENT_PREFIX: &str = "PLOMBIR_GIT_";

/// Variables of the former name found in the process environment.
#[derive(Debug, PartialEq, Eq)]
pub struct RetiredEnvironment {
    names: Vec<String>,
}

impl fmt::Display for RetiredEnvironment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "the environment still sets {} from before the rename of ForgeKeep to Plombir Git: {}.\n  \
             These are no longer read, so starting would silently fall back to defaults for every \
             one of them.\n  hint: rename each to the {CURRENT_PREFIX}* spelling (for example \
             {} -> {}), or unset it",
            if self.names.len() == 1 { "a variable" } else { "variables" },
            self.names.join(", "),
            self.names[0],
            self.names[0].replacen(RETIRED_PREFIX, CURRENT_PREFIX, 1),
        )
    }
}

impl std::error::Error for RetiredEnvironment {}

/// Fail when the process environment sets any `FORGEKEEP_*` variable.
///
/// Call it at the top of `main`, before anything reads configuration.
pub fn refuse_retired_environment() -> Result<(), RetiredEnvironment> {
    retired_variables(std::env::vars_os().map(|(name, _)| name))
}

fn retired_variables(names: impl IntoIterator<Item = OsString>) -> Result<(), RetiredEnvironment> {
    let mut retired: Vec<String> = names
        .into_iter()
        .filter(|name| {
            name.as_encoded_bytes()
                .starts_with(RETIRED_PREFIX.as_bytes())
        })
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    if retired.is_empty() {
        return Ok(());
    }
    retired.sort();
    Err(RetiredEnvironment { names: retired })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn an_environment_without_the_former_prefix_starts() {
        assert_eq!(
            retired_variables(names(&[
                "PATH",
                "PLOMBIR_GIT_CORS_ORIGINS",
                "MY_FORGEKEEP_NOTE"
            ])),
            Ok(())
        );
    }

    #[test]
    fn every_retired_name_is_reported_sorted_and_without_its_value() {
        let error = retired_variables(names(&[
            "PATH",
            "FORGEKEEP_JWT_SECRET",
            "PLOMBIR_GIT_HTTP_PORT",
            "FORGEKEEP_CORS_ORIGINS",
        ]))
        .expect_err("FORGEKEEP_* must refuse the start");
        assert_eq!(
            error.names,
            ["FORGEKEEP_CORS_ORIGINS", "FORGEKEEP_JWT_SECRET"]
        );
        let message = error.to_string();
        assert!(
            message.contains("FORGEKEEP_CORS_ORIGINS, FORGEKEEP_JWT_SECRET"),
            "{message}"
        );
        assert!(
            message.contains("FORGEKEEP_CORS_ORIGINS -> PLOMBIR_GIT_CORS_ORIGINS"),
            "{message}"
        );
    }

    #[test]
    fn the_bare_former_name_without_a_suffix_is_not_a_variable_of_ours() {
        assert_eq!(retired_variables(names(&["FORGEKEEP"])), Ok(()));
    }
}
