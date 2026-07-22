//! Backwards-compatible environment variable lookup for the IronForge → ForgeKeep
//! rebrand.
//!
//! During the fork the `IRONFORGE_*` environment variables were renamed to
//! `FORGEKEEP_*`. To avoid breaking existing deployments in the release that ships
//! the rename, every read site first looks up the new name and, only if it is
//! unset, falls back to the deprecated `IRONFORGE_*` name while emitting a
//! one-time deprecation warning. The fallback is scheduled for removal in a
//! future release.

/// Read `new` from the environment, falling back to the deprecated `old` name.
///
/// Returns the value of `$new` if set; otherwise the value of `$old` (logging a
/// deprecation warning); otherwise `None`.
pub fn env_var_compat(new: &str, old: &str) -> Option<String> {
    if let Ok(value) = std::env::var(new) {
        return Some(value);
    }
    match std::env::var(old) {
        Ok(value) => {
            tracing::warn!(
                "environment variable `{old}` is deprecated and will be removed in a future \
                 release; use `{new}` instead"
            );
            Some(value)
        }
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_new_over_old() {
        // Unique names so parallel tests don't collide on the process env.
        let new = "FORGEKEEP_ENVCOMPAT_TEST_A";
        let old = "IRONFORGE_ENVCOMPAT_TEST_A";
        std::env::set_var(new, "new-value");
        std::env::set_var(old, "old-value");
        assert_eq!(env_var_compat(new, old).as_deref(), Some("new-value"));
        std::env::remove_var(new);
        std::env::remove_var(old);
    }

    #[test]
    fn falls_back_to_old() {
        let new = "FORGEKEEP_ENVCOMPAT_TEST_B";
        let old = "IRONFORGE_ENVCOMPAT_TEST_B";
        std::env::remove_var(new);
        std::env::set_var(old, "old-value");
        assert_eq!(env_var_compat(new, old).as_deref(), Some("old-value"));
        std::env::remove_var(old);
    }

    #[test]
    fn none_when_unset() {
        let new = "FORGEKEEP_ENVCOMPAT_TEST_C";
        let old = "IRONFORGE_ENVCOMPAT_TEST_C";
        std::env::remove_var(new);
        std::env::remove_var(old);
        assert_eq!(env_var_compat(new, old), None);
    }
}
