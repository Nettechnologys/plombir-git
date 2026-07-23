//! Runner configuration file handling and auth-token/environment resolution.

use std::path::PathBuf;

use anyhow::Result;

/// Runner configuration file.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, Default)]
pub(crate) struct RunnerConfig {
    pub(crate) server: Option<String>,
    pub(crate) token: Option<String>,
    pub(crate) runner_id: Option<i64>,
    pub(crate) name: Option<String>,
    pub(crate) labels: Option<Vec<String>>,
}

fn config_path(path: &str) -> PathBuf {
    let expanded = if let Some(remain) = path.strip_prefix('~') {
        match home::home_dir() {
            Some(home) => {
                let trimmed = remain.trim_start_matches('/');
                let mut result = home;
                if !trimmed.is_empty() {
                    result.push(trimmed);
                }
                result.to_string_lossy().to_string()
            }
            None => path.to_string(),
        }
    } else {
        path.to_string()
    };

    PathBuf::from(expanded)
}

pub(crate) fn load_config(path: &str) -> Option<RunnerConfig> {
    let p = config_path(path);
    if p.exists() {
        let content = std::fs::read_to_string(&p).ok()?;
        toml::from_str(&content).ok()
    } else {
        None
    }
}

pub(crate) fn save_config(path: &str, config: &RunnerConfig) -> Result<()> {
    let p = config_path(path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(config)?;
    std::fs::write(&p, content)?;
    Ok(())
}

pub(crate) fn resolve_auth_token(auth_token: Option<String>) -> Option<String> {
    auth_token.or_else(|| env_var_compat("FORGEKEEP_AUTH_TOKEN", "IRONFORGE_AUTH_TOKEN"))
}

/// Read `new` from the environment, falling back to the deprecated `old` name
/// (IronForge → ForgeKeep rebrand) with a one-time deprecation warning.
fn env_var_compat(new: &str, old: &str) -> Option<String> {
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
