//! Runner configuration file handling and auth-token/environment resolution.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// Runner configuration file.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, Default)]
pub(crate) struct RunnerConfig {
    pub(crate) server: Option<String>,
    pub(crate) token: Option<String>,
    pub(crate) runner_id: Option<i64>,
    pub(crate) name: Option<String>,
    pub(crate) labels: Option<Vec<String>>,
}

/// Built-in default for `--server`.
///
/// It lives here rather than in clap's `default_value` on purpose: a clap
/// default is indistinguishable from a value the operator typed, so with one the
/// `server` key of `runner.toml` could never win over "the flag was not passed"
/// — a runner registered against a remote server silently went back to
/// localhost on its next start. Same reasoning (and same fix) as
/// `rg-cli/src/serve.rs::DEFAULT_*`.
pub(crate) const DEFAULT_SERVER: &str = "http://127.0.0.1:8080";

/// Last-resort runner name, used when neither the CLI, nor the config file, nor
/// the system hostname yields one.
const FALLBACK_NAME: &str = "unnamed-runner";

/// The CLI half of every knob that also lives in `runner.toml`, resolved against
/// the config file by [`resolve_runner`]. `None` means "flag not passed" — never
/// a default.
#[derive(Debug, Default)]
pub(crate) struct RunnerCliArgs {
    pub(crate) server: Option<String>,
    pub(crate) name: Option<String>,
    /// Raw comma-separated value of `--labels`, parsed by [`parse_labels`].
    pub(crate) labels: Option<String>,
    pub(crate) token: Option<String>,
    pub(crate) runner_id: Option<i64>,
}

/// How the runner identifies itself to the server.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RunnerIdentity {
    /// A complete `(runner_id, token)` pair was found — on the CLI or in the
    /// config file. Registration must be **skipped**.
    Existing { runner_id: i64, token: String },
    /// Neither source carried a complete pair: the runner has to register and
    /// persist the result for the next start.
    Register,
}

/// Runner settings after `CLI arg > config file > built-in default`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ResolvedRunner {
    pub(crate) server: String,
    pub(crate) identity: RunnerIdentity,
    pub(crate) name: String,
    pub(crate) labels: Vec<String>,
}

/// Split a `--labels` value: comma-separated, trimmed, empty entries dropped so
/// `"docker, ,linux,"` cannot register a runner carrying a blank label.
pub(crate) fn parse_labels(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .map(str::to_string)
        .collect()
}

/// Apply the documented `CLI arg > config file > built-in default` precedence to
/// every runner setting that has both a flag and a config key.
///
/// Extracted as a pure function so the wiring is unit-testable without talking
/// to a server — the bug this replaces was a missing wire, not a bad value:
/// `cmd_run` read the config file's `name`/`labels` but never its `server`,
/// `runner_id`, or `token`, so a runner that had already registered and saved
/// its identity re-registered on **every** start, piling up duplicate runner
/// rows and tokens on the server (and failing outright without `--auth-token`).
///
/// `runner_id` + `token` are resolved as one **atomic pair**, per source: a
/// token only authenticates the runner id it was issued for, so completing a
/// CLI-supplied id with a token from the config file would just produce a
/// confident 401.
pub(crate) fn resolve_runner(
    cli: RunnerCliArgs,
    cfg: Option<&RunnerConfig>,
) -> Result<ResolvedRunner> {
    let identity = match (cli.runner_id, cli.token) {
        (Some(runner_id), Some(token)) => RunnerIdentity::Existing { runner_id, token },
        (None, None) => resolve_config_identity(cfg),
        // Half a credential pair is always an operator mistake. Silently
        // dropping into registration (the previous behaviour) hid it behind a
        // duplicate runner; say so instead.
        (id, _) => {
            let (given, missing) = if id.is_some() {
                ("--runner-id", "--token")
            } else {
                ("--token", "--runner-id")
            };
            anyhow::bail!(
                "`{given}` was passed without `{missing}` — a runner token only authenticates the \
                 runner id it was issued for, so the two must be given together. Pass both, or \
                 pass neither and let them come from the config file (or from a fresh \
                 registration)"
            );
        }
    };

    Ok(ResolvedRunner {
        server: cli
            .server
            .filter(|server| !server.trim().is_empty())
            .or_else(|| {
                cfg.and_then(|c| c.server.clone())
                    .filter(|server| !server.trim().is_empty())
            })
            .unwrap_or_else(|| DEFAULT_SERVER.to_string()),
        identity,
        name: cli
            .name
            .filter(|name| !name.trim().is_empty())
            .or_else(|| {
                cfg.and_then(|c| c.name.clone())
                    .filter(|name| !name.trim().is_empty())
            })
            .unwrap_or_else(|| system_hostname().unwrap_or_else(|| FALLBACK_NAME.to_string())),
        labels: cli
            .labels
            .as_deref()
            .map(parse_labels)
            .or_else(|| cfg.and_then(|c| c.labels.clone()))
            .unwrap_or_default(),
    })
}

/// Take the `(runner_id, token)` pair out of the config file, if it holds one.
///
/// A file carrying exactly one half is broken — hand-edited, or truncated by a
/// failed write. Registration is the only way forward, but it is announced:
/// otherwise the operator sees a runner that keeps re-registering while its
/// config file looks populated.
fn resolve_config_identity(cfg: Option<&RunnerConfig>) -> RunnerIdentity {
    let (runner_id, token) = match cfg {
        Some(cfg) => (cfg.runner_id, cfg.token.clone()),
        None => return RunnerIdentity::Register,
    };

    match (runner_id, token) {
        (Some(runner_id), Some(token)) => RunnerIdentity::Existing { runner_id, token },
        (None, None) => RunnerIdentity::Register,
        (id, _) => {
            let present = if id.is_some() { "runner_id" } else { "token" };
            let missing = if id.is_some() { "token" } else { "runner_id" };
            tracing::warn!(
                "the runner config file has `{present}` but no `{missing}`, so it cannot be used \
                 to log in; registering a new runner instead — delete the file and re-run \
                 `forgekeep-runner register --save` to get a consistent one"
            );
            RunnerIdentity::Register
        }
    }
}

/// The system hostname, used as the default runner name.
fn system_hostname() -> Option<String> {
    let output = std::process::Command::new("hostname").output().ok()?;
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
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

/// Remediation appended to every runner config-file read failure. The file is
/// generated by `forgekeep-runner register --save`, so a broken one can always
/// be removed and regenerated.
const RUNNER_CONFIG_HINT: &str = "fix or delete it, then re-run \
     `forgekeep-runner register --save` to regenerate it (and bind-mount the file \
     itself, not a directory)";

/// Load the runner configuration file.
///
/// `Ok(None)` means the file genuinely **does not exist** — the legitimate
/// "no config yet" case that lets the runner fall back to CLI flags and
/// auto-registration. A file that exists but cannot be read (wrong permissions,
/// or a directory that a Docker bind-mount auto-created because the source file
/// was missing) or cannot be parsed (broken TOML) is an error and is reported
/// with the path and the cause.
///
/// Before this, both cases went through `.ok()?` and became a plain `None`, i.e.
/// "as if there were no config": the runner silently ignored an unreadable file
/// and re-registered under a fresh identity on every start, and "why does the
/// runner ignore my config" had to be debugged blind. Mirrors
/// `rg-cli/src/serve.rs::ensure_regular_file` + `load_config_file` for the
/// server-side config file (the crates share no dependency, hence the small
/// duplication).
pub(crate) fn load_config(path: &str) -> Result<Option<RunnerConfig>> {
    let p = config_path(path);
    let shown = p.display();

    let content = match std::fs::read_to_string(&p) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            if p.is_dir() {
                anyhow::bail!(
                    "runner config path `{shown}` is a directory, not a file — a Docker \
                     bind-mount likely auto-created it because the source file was missing; \
                     {RUNNER_CONFIG_HINT}"
                );
            }
            return Err(anyhow::Error::new(error)).with_context(|| {
                format!("failed to read runner config `{shown}` — {RUNNER_CONFIG_HINT}")
            });
        }
    };

    let config = toml::from_str(&content).with_context(|| {
        format!("failed to parse runner config `{shown}` as TOML — {RUNNER_CONFIG_HINT}")
    })?;
    tracing::debug!(path = %shown, "Loaded runner configuration file");
    Ok(Some(config))
}

/// Remediation appended to every runner config-file **write** failure. Unlike
/// the read path, the file is not there to be fixed — the directory or the
/// ownership of the target is what has to change.
const RUNNER_CONFIG_WRITE_HINT: &str = "make sure it is a writable file owned by the user \
     running the runner — inside a container that uid is unrelated to the host user of the same \
     name, so create the file on the host, `chown` it to the container uid, and bind-mount the \
     file itself, not a directory";

/// Persist the runner configuration file.
///
/// Every failure names the path it happened on: an unwritable `--config` target
/// (a read-only volume, a directory a Docker bind-mount auto-created because the
/// source file was missing, or a file owned by another uid) used to surface as a
/// bare `Permission denied (os error 13)` / `Is a directory (os error 21)` with
/// no hint of *which* path was involved. Same treatment as [`load_config`] on the
/// read side.
pub(crate) fn save_config(path: &str, config: &RunnerConfig) -> Result<()> {
    let p = config_path(path);
    let shown = p.display();

    if let Some(parent) = p.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create the directory `{}` for runner config `{shown}` — \
                 {RUNNER_CONFIG_WRITE_HINT}",
                parent.display()
            )
        })?;
    }

    let content = toml::to_string_pretty(config)
        .with_context(|| format!("failed to serialize the runner config for `{shown}` as TOML"))?;

    std::fs::write(&p, content).with_context(|| {
        if p.is_dir() {
            format!(
                "failed to write runner config `{shown}`: the path is a directory, not a file — \
                 a Docker bind-mount likely auto-created it because the source file was missing; \
                 {RUNNER_CONFIG_WRITE_HINT}"
            )
        } else {
            format!("failed to write runner config `{shown}` — {RUNNER_CONFIG_WRITE_HINT}")
        }
    })?;

    tracing::debug!(path = %shown, "Saved runner configuration file");
    Ok(())
}

/// Warning text for a runner config that could not be persisted after an
/// auto-registration.
///
/// The write failure itself is **not** fatal — the runner already holds a valid
/// identity for this process and can work. What the operator must learn is the
/// consequence: without a persisted config the next start registers yet another
/// runner, so the server slowly fills with dead duplicates. Before this the whole
/// error was dropped on the floor (`if save_config(..).is_ok()`), leaving only a
/// missing "Config saved to …" line to notice.
pub(crate) fn config_not_persisted_warning(path: &str, error: &anyhow::Error) -> String {
    format!(
        "could not persist the runner config `{path}`: {error:#}. This run continues with the \
         identity it just registered, but every restart will register a NEW runner until the \
         file becomes writable"
    )
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

#[cfg(test)]
mod tests {
    use super::{
        config_not_persisted_warning, load_config, parse_labels, resolve_runner, save_config,
        ResolvedRunner, RunnerCliArgs, RunnerConfig, RunnerIdentity, DEFAULT_SERVER,
    };

    fn sample_config() -> RunnerConfig {
        RunnerConfig {
            server: Some("http://127.0.0.1:8080".to_string()),
            token: Some("tok".to_string()),
            runner_id: Some(7),
            name: Some("builder-1".to_string()),
            labels: Some(vec!["linux".to_string()]),
        }
    }

    /// A missing config file is the legitimate "no config yet" case: the runner
    /// falls back to CLI flags and auto-registration, no diagnostics needed.
    #[test]
    fn a_missing_config_file_is_a_quiet_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runner.toml");

        let loaded =
            load_config(path.to_str().unwrap()).expect("missing file must not be an error");

        assert!(loaded.is_none());
    }

    #[test]
    fn a_readable_config_file_is_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runner.toml");
        std::fs::write(
            &path,
            r#"
server = "http://127.0.0.1:8080"
runner_id = 7
token = "tok"
name = "builder-1"
labels = ["linux", "docker"]
"#,
        )
        .unwrap();

        let cfg = load_config(path.to_str().unwrap())
            .expect("a valid config must load")
            .expect("a config file that exists must yield Some");

        assert_eq!(cfg.server.as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(cfg.runner_id, Some(7));
        assert_eq!(cfg.token.as_deref(), Some("tok"));
        assert_eq!(cfg.name.as_deref(), Some("builder-1"));
        assert_eq!(
            cfg.labels.as_deref(),
            Some(["linux".to_string(), "docker".to_string()].as_slice())
        );
    }

    /// The bug: a broken TOML went through `.ok()?` and became `None`, so the
    /// runner behaved exactly as if the operator had never written the file.
    #[test]
    fn a_malformed_config_file_is_reported_with_path_and_cause() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runner.toml");
        std::fs::write(
            &path,
            "server = \"http://127.0.0.1:8080\"\nthis is not toml\n",
        )
        .unwrap();

        let error = load_config(path.to_str().unwrap())
            .expect_err("a malformed config must not be silently treated as absent");

        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(path.to_str().unwrap()),
            "error must name the config path: {rendered}"
        );
        assert!(
            rendered.contains("as TOML"),
            "error must say the file failed to parse: {rendered}"
        );
        assert!(
            rendered.contains("forgekeep-runner register --save"),
            "error must carry the remediation hint: {rendered}"
        );
    }

    /// The deploy incident that started this phase: a Docker bind-mount whose
    /// source file was missing leaves a *directory* at the config path.
    #[test]
    fn a_directory_at_the_config_path_is_reported_as_such() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runner.toml");
        std::fs::create_dir(&path).unwrap();

        let error = load_config(path.to_str().unwrap())
            .expect_err("a directory at the config path must not be treated as absent");

        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(path.to_str().unwrap()),
            "error must name the config path: {rendered}"
        );
        assert!(
            rendered.contains("is a directory"),
            "error must explain the directory case: {rendered}"
        );
    }

    /// An existing but unreadable file (mode 000) must be an error too, not a
    /// silent `None` — the "wrong uid inside the container" case.
    #[cfg(unix)]
    #[test]
    fn an_unreadable_config_file_is_reported_with_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runner.toml");
        std::fs::write(&path, "server = \"http://127.0.0.1:8080\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();

        // root (and CI containers running as uid 0) bypass the permission bits
        // entirely, so there is nothing to observe there — probe instead of
        // guessing the uid.
        if std::fs::read_to_string(&path).is_ok() {
            return;
        }

        let error = load_config(path.to_str().unwrap())
            .expect_err("an unreadable config must not be silently treated as absent");

        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(path.to_str().unwrap()),
            "error must name the config path: {rendered}"
        );
        assert!(
            rendered.contains("failed to read"),
            "error must say the read failed: {rendered}"
        );
    }

    #[test]
    fn a_saved_config_round_trips_through_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("runner.toml");
        let shown = path.to_str().unwrap();

        save_config(shown, &sample_config()).expect("a writable path must save");

        let loaded = load_config(shown)
            .expect("the file just written must load")
            .expect("a config file that exists must yield Some");
        assert_eq!(loaded.server.as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(loaded.runner_id, Some(7));
        assert_eq!(loaded.token.as_deref(), Some("tok"));
    }

    /// The bug: both `create_dir_all` and `write` used a bare `?`, so the deploy
    /// case (a bind-mount that left a directory at the config path) surfaced as
    /// `Is a directory (os error 21)` with no path and nothing to act on.
    #[test]
    fn saving_onto_a_directory_is_reported_with_path_and_cause() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runner.toml");
        std::fs::create_dir(&path).unwrap();

        let error = save_config(path.to_str().unwrap(), &sample_config())
            .expect_err("writing onto a directory must fail");

        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(path.to_str().unwrap()),
            "error must name the config path: {rendered}"
        );
        assert!(
            rendered.contains("is a directory"),
            "error must explain the directory case: {rendered}"
        );
        assert!(
            rendered.contains("bind-mount"),
            "error must carry the remediation hint: {rendered}"
        );
    }

    /// The "wrong uid inside the container" case on the write side: the config
    /// directory exists but the runner may not write into it.
    #[cfg(unix)]
    #[test]
    fn saving_into_an_unwritable_directory_is_reported_with_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let readonly = dir.path().join("readonly");
        std::fs::create_dir(&readonly).unwrap();
        std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o555)).unwrap();
        let path = readonly.join("runner.toml");

        // root ignores the permission bits entirely — probe instead of guessing
        // the uid, exactly as the read-side test does.
        if std::fs::write(&path, "probe").is_ok() {
            return;
        }

        let error = save_config(path.to_str().unwrap(), &sample_config())
            .expect_err("writing into an unwritable directory must fail");

        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(path.to_str().unwrap()),
            "error must name the config path: {rendered}"
        );
        assert!(
            rendered.contains("failed to write"),
            "error must say the write failed: {rendered}"
        );
        assert!(
            rendered.contains("chown"),
            "error must carry the ownership remediation: {rendered}"
        );
    }

    /// A parent directory that cannot even be created must name the *parent* —
    /// that is the path whose permissions actually blocked the save.
    #[cfg(unix)]
    #[test]
    fn a_parent_directory_that_cannot_be_created_is_reported_with_its_path() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let readonly = dir.path().join("readonly");
        std::fs::create_dir(&readonly).unwrap();
        std::fs::set_permissions(&readonly, std::fs::Permissions::from_mode(0o555)).unwrap();
        let parent = readonly.join("forgekeep");
        let path = parent.join("runner.toml");

        if std::fs::create_dir(&parent).is_ok() {
            return; // running as root
        }

        let error = save_config(path.to_str().unwrap(), &sample_config())
            .expect_err("an uncreatable parent directory must fail");

        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(parent.to_str().unwrap()),
            "error must name the directory it failed to create: {rendered}"
        );
        assert!(
            rendered.contains("failed to create the directory"),
            "error must say the directory creation failed: {rendered}"
        );
    }

    /// The call-site half of the bug: `cmd_run` threw the whole error away with
    /// `if save_config(..).is_ok()`, so a failed save was invisible and the
    /// runner silently re-registered on every start.
    #[test]
    fn the_call_site_warning_carries_the_path_the_cause_and_the_consequence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runner.toml");
        std::fs::create_dir(&path).unwrap();
        let shown = path.to_str().unwrap();

        let error = save_config(shown, &sample_config()).expect_err("writing onto a directory");
        let warning = config_not_persisted_warning(shown, &error);

        assert!(
            warning.contains(shown),
            "warning must name the config path: {warning}"
        );
        assert!(
            warning.contains("is a directory"),
            "warning must carry the underlying cause: {warning}"
        );
        assert!(
            warning.contains("register a NEW runner"),
            "warning must spell out the consequence: {warning}"
        );
    }

    /// The bug this card fixes: `cmd_run` never read `server`, `runner_id`, or
    /// `token` out of the config file, so a runner that had already registered
    /// and saved its identity went straight back into registration on the next
    /// start — a new runner row and token on the server every time.
    #[test]
    fn a_saved_config_supplies_server_identity_name_and_labels_without_any_flags() {
        let resolved = resolve_runner(RunnerCliArgs::default(), Some(&sample_config()))
            .expect("a complete config must resolve");

        assert_eq!(
            resolved,
            ResolvedRunner {
                server: "http://127.0.0.1:8080".to_string(),
                identity: RunnerIdentity::Existing {
                    runner_id: 7,
                    token: "tok".to_string(),
                },
                name: "builder-1".to_string(),
                labels: vec!["linux".to_string()],
            }
        );
    }

    /// A config pointing at a remote server must not lose to the built-in
    /// localhost default — the clap `default_value` on `--server` made those two
    /// indistinguishable, so the config always lost.
    #[test]
    fn a_config_server_beats_the_built_in_default() {
        let cfg = RunnerConfig {
            server: Some("https://ci.example.com".to_string()),
            ..sample_config()
        };

        let resolved = resolve_runner(RunnerCliArgs::default(), Some(&cfg)).unwrap();

        assert_eq!(resolved.server, "https://ci.example.com");
    }

    /// The `register` shape of the same bug: the operator names only the runner
    /// and expects the server from `runner.toml`. `register` used to resolve
    /// nothing at all — `--server` carried a clap default, so it registered
    /// against localhost and, with `--save`, wrote that back over the file.
    #[test]
    fn registering_with_only_a_name_takes_the_server_and_labels_from_the_config() {
        let cfg = RunnerConfig {
            server: Some("https://git.example.com".to_string()),
            ..sample_config()
        };
        let cli = RunnerCliArgs {
            name: Some("builder-2".to_string()),
            ..RunnerCliArgs::default()
        };

        let resolved = resolve_runner(cli, Some(&cfg)).unwrap();

        assert_eq!(resolved.server, "https://git.example.com");
        assert_eq!(resolved.name, "builder-2");
        // `--labels` was not passed, so the file's value has to survive the
        // registration rather than be replaced by an empty list.
        assert_eq!(resolved.labels, vec!["linux".to_string()]);
    }

    #[test]
    fn cli_flags_beat_the_config_file() {
        let cli = RunnerCliArgs {
            server: Some("https://ci.example.com".to_string()),
            name: Some("from-flag".to_string()),
            labels: Some("docker,amd64".to_string()),
            token: Some("cli-tok".to_string()),
            runner_id: Some(42),
        };

        let resolved = resolve_runner(cli, Some(&sample_config())).unwrap();

        assert_eq!(
            resolved,
            ResolvedRunner {
                server: "https://ci.example.com".to_string(),
                identity: RunnerIdentity::Existing {
                    runner_id: 42,
                    token: "cli-tok".to_string(),
                },
                name: "from-flag".to_string(),
                labels: vec!["docker".to_string(), "amd64".to_string()],
            }
        );
    }

    /// The genuine first-start case: nothing on the CLI, no config file yet.
    #[test]
    fn no_config_and_no_flags_registers_against_the_default_server() {
        let resolved = resolve_runner(RunnerCliArgs::default(), None).unwrap();

        assert_eq!(resolved.server, DEFAULT_SERVER);
        assert_eq!(resolved.identity, RunnerIdentity::Register);
        assert!(resolved.labels.is_empty());
        // Hostname-derived, so the exact value is environment-dependent; what
        // matters is that the runner never registers under an empty name.
        assert!(
            !resolved.name.trim().is_empty(),
            "the fallback name must not be blank: {:?}",
            resolved.name
        );
    }

    /// `--runner-id` and `--token` are one credential pair: a token only
    /// authenticates the id it was issued for. Half a pair used to fall through
    /// into registration, hiding the mistake behind a duplicate runner.
    #[test]
    fn half_a_credential_pair_on_the_cli_is_rejected() {
        for (cli, given, missing) in [
            (
                RunnerCliArgs {
                    runner_id: Some(7),
                    ..RunnerCliArgs::default()
                },
                "--runner-id",
                "--token",
            ),
            (
                RunnerCliArgs {
                    token: Some("tok".to_string()),
                    ..RunnerCliArgs::default()
                },
                "--token",
                "--runner-id",
            ),
        ] {
            let error = resolve_runner(cli, Some(&sample_config()))
                .expect_err("half a credential pair must not resolve");

            let rendered = format!("{error:#}");
            assert!(
                rendered.contains(given) && rendered.contains(missing),
                "error must name both flags: {rendered}"
            );
        }
    }

    /// A hand-edited or half-written config cannot log in, so registration is the
    /// only way forward — but the identity resolution must still be explicit
    /// about it rather than looking like a first start.
    #[test]
    fn a_config_holding_only_one_half_of_the_pair_falls_back_to_registration() {
        for cfg in [
            RunnerConfig {
                token: None,
                ..sample_config()
            },
            RunnerConfig {
                runner_id: None,
                ..sample_config()
            },
        ] {
            let resolved = resolve_runner(RunnerCliArgs::default(), Some(&cfg)).unwrap();

            assert_eq!(resolved.identity, RunnerIdentity::Register);
            // The rest of the file is still honoured — registration should reuse
            // the operator's server and name, not fall back to localhost.
            assert_eq!(resolved.server, "http://127.0.0.1:8080");
            assert_eq!(resolved.name, "builder-1");
        }
    }

    /// An explicitly empty flag means "no labels", and must not silently hand the
    /// decision back to the config file.
    #[test]
    fn an_empty_labels_flag_clears_the_config_labels() {
        let cli = RunnerCliArgs {
            labels: Some(String::new()),
            ..RunnerCliArgs::default()
        };

        let resolved = resolve_runner(cli, Some(&sample_config())).unwrap();

        assert!(resolved.labels.is_empty());
    }

    /// A blank `--server` / `--name` is not a value: it must not shadow the
    /// config file (the shell-expansion case, `--server "$FORGEKEEP_SERVER"`
    /// with the variable unset).
    #[test]
    fn blank_flags_do_not_shadow_the_config() {
        let cli = RunnerCliArgs {
            server: Some("   ".to_string()),
            name: Some(String::new()),
            ..RunnerCliArgs::default()
        };

        let resolved = resolve_runner(cli, Some(&sample_config())).unwrap();

        assert_eq!(resolved.server, "http://127.0.0.1:8080");
        assert_eq!(resolved.name, "builder-1");
    }

    #[test]
    fn parse_labels_trims_and_drops_blank_entries() {
        assert_eq!(
            parse_labels("docker, linux ,,  ,amd64,"),
            vec![
                "docker".to_string(),
                "linux".to_string(),
                "amd64".to_string()
            ]
        );
        assert!(parse_labels("  ").is_empty());
    }
}
