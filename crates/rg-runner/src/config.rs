//! Runner configuration file handling and auth-token/environment resolution.

use std::path::PathBuf;

use anyhow::{Context, Result};

/// Runner configuration file.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunnerConfig {
    pub(crate) server: Option<String>,
    /// Permit Bearer credentials on the one configured remote HTTP server.
    /// Loopback HTTP remains available without this exception.
    pub(crate) allow_insecure_http: Option<bool>,
    pub(crate) token: Option<String>,
    pub(crate) runner_id: Option<i64>,
    /// Human-addressable repository scope used for fresh registration.
    pub(crate) repository: Option<String>,
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
/// the config file by [`resolve_runner`]. Value-taking fields use `None` for
/// "flag not passed"; the safety opt-in is an additive boolean switch.
#[derive(Debug, Default)]
pub(crate) struct RunnerCliArgs {
    pub(crate) server: Option<String>,
    pub(crate) allow_insecure_http: bool,
    pub(crate) repository: Option<String>,
    pub(crate) name: Option<String>,
    /// Raw comma-separated value of the legacy `--labels` flag.
    pub(crate) labels: Option<String>,
    /// Structural values of repeatable `--label`; commas stay inside an item.
    pub(crate) label: Vec<String>,
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
    pub(crate) allow_insecure_http: bool,
    pub(crate) identity: RunnerIdentity,
    pub(crate) repository: Option<String>,
    pub(crate) name: String,
    pub(crate) labels: Vec<String>,
}

/// Split a legacy `--labels` value: comma-separated, trimmed, empty entries dropped so
/// `"docker, ,linux,"` cannot register a runner carrying a blank label.
pub(crate) fn parse_legacy_labels(raw: &str) -> Vec<String> {
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
    let cli_labels = if cli.label.is_empty() {
        cli.labels.as_deref().map(parse_legacy_labels)
    } else {
        anyhow::ensure!(
            cli.labels.is_none(),
            "`--label` cannot be combined with legacy `--labels`; use repeated `--label` for an unambiguous list"
        );
        Some(cli.label.clone())
    };

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
        // This is a one-way safety opt-in rather than an ordinary value: an
        // explicit `true` from either source enables it, while absence/false
        // cannot accidentally weaken a true setting from the other source.
        allow_insecure_http: cli.allow_insecure_http
            || cfg
                .and_then(|config| config.allow_insecure_http)
                .unwrap_or(false),
        identity,
        repository: cli
            .repository
            .filter(|repository| !repository.trim().is_empty())
            .or_else(|| {
                cfg.and_then(|config| config.repository.clone())
                    .filter(|repository| !repository.trim().is_empty())
            }),
        name: cli
            .name
            .filter(|name| !name.trim().is_empty())
            .or_else(|| {
                cfg.and_then(|c| c.name.clone())
                    .filter(|name| !name.trim().is_empty())
            })
            .unwrap_or_else(|| system_hostname().unwrap_or_else(|| FALLBACK_NAME.to_string())),
        labels: cli_labels
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

    ensure_owner_only(&p)?;

    let config = toml::from_str(&content).with_context(|| {
        format!("failed to parse runner config `{shown}` as TOML — {RUNNER_CONFIG_HINT}")
    })?;
    tracing::debug!(path = %shown, "Loaded runner configuration file");
    Ok(Some(config))
}

/// Refuse a runner config another local account can read.
///
/// The file carries `token` — the runner's whole identity against the server —
/// so "this process can read it" is not the question that matters; "can a
/// different local account read it" is. A runner token claims jobs for its
/// repository and receives their secrets, so anyone who can read this file can
/// take the runner's place.
///
/// Mirrors `rg_core::platform::fs::ensure_owner_only` down to the message: the
/// runner links against neither `rg-core` nor `rg-cli` (it ships as a separate,
/// deliberately small binary), which is the same reason [`load_config`]
/// duplicates the server's config-file diagnostics.
///
/// Unix permission bits have no portable equivalent, so this is a no-op on
/// other platforms, which rely on their own ACLs.
fn ensure_owner_only(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = std::fs::metadata(path)
            .with_context(|| {
                format!(
                    "failed to read runner config permissions `{}`",
                    path.display()
                )
            })?
            .permissions()
            .mode()
            & 0o777;
        if mode & 0o077 != 0 {
            anyhow::bail!(
                "runner config `{}` has mode {mode:04o} and carries the runner token, so every \
                 other local account can read it; run chmod 600 {}",
                path.display(),
                path.display()
            );
        }
    }

    #[cfg(not(unix))]
    {
        let _ = path;
    }

    Ok(())
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

    write_owner_only(&p, &content).with_context(|| {
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

/// Write the runner config so that it is owner-only from its very first byte.
///
/// Three things a plain `std::fs::write` gets wrong for a file that holds a
/// credential:
///
/// * a fresh file takes its mode from the ambient umask, so a permissive umask
///   (`0o002`, and the `0o000` a container entrypoint sometimes sets) publishes
///   the token to every local account;
/// * an *existing* file keeps whatever mode it already had — re-registering
///   into a `0644` file leaves it `0644`;
/// * truncate-then-write means a crash mid-write leaves a half-written config,
///   which [`load_config`] then reports as broken TOML.
///
/// Writing a fresh `0600` temp file next to the target and renaming it into
/// place answers all three at once: the mode is set before the secret is
/// written, the rename replaces the old inode (and its mode) atomically, and a
/// crash leaves the previous config intact.
fn write_owner_only(path: &std::path::Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;

    let temp = path.with_file_name(format!(
        ".{}.{}.tmp",
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "runner-config".to_string()),
        std::process::id()
    ));

    let write = || -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // `OpenOptions::mode` applies to a file this call *creates*. A temp
            // left behind by a killed run with the same pid would otherwise be
            // reused with whatever mode it already carries.
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)
    };

    write().inspect_err(|_| {
        // The rename never happened, so this temp file is nobody's config —
        // leaving it behind would drop a stray copy of the token next to the
        // path the operator is looking at. A failure to open it in the first
        // place leaves nothing to remove, which is the `NotFound` case.
        match std::fs::remove_file(&temp) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(
                path = %temp.display(),
                %error,
                "failed to remove the temporary runner config after a failed save; it carries the \
                 runner token and stays on disk until an operator removes it"
            ),
        }
    })
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
    auth_token.or_else(|| std::env::var("FORGEKEEP_AUTH_TOKEN").ok())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        config_not_persisted_warning, load_config, parse_legacy_labels, resolve_runner,
        save_config, ResolvedRunner, RunnerCliArgs, RunnerConfig, RunnerIdentity, DEFAULT_SERVER,
    };

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    fn sample_config() -> RunnerConfig {
        RunnerConfig {
            server: Some("http://127.0.0.1:8080".to_string()),
            allow_insecure_http: Some(false),
            token: Some("tok".to_string()),
            runner_id: Some(7),
            repository: Some("owner/project".to_string()),
            name: Some("builder-1".to_string()),
            labels: Some(vec!["linux".to_string()]),
        }
    }

    /// Create a config file the loader will accept on its permissions, so a
    /// test about parsing fails on parsing rather than on mode bits. `0644` is
    /// what a bare `std::fs::write` produces under the usual umask, and that is
    /// now a refusal in its own right.
    fn write_test_config(path: &std::path::Path, content: &str) {
        std::fs::write(path, content).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
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
        write_test_config(
            &path,
            r#"
server = "http://127.0.0.1:8080"
allow_insecure_http = false
runner_id = 7
token = "tok"
name = "builder-1"
labels = ["linux", "docker"]
"#,
        );

        let cfg = load_config(path.to_str().unwrap())
            .expect("a valid config must load")
            .expect("a config file that exists must yield Some");

        assert_eq!(cfg.server.as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(cfg.allow_insecure_http, Some(false));
        assert_eq!(cfg.runner_id, Some(7));
        assert_eq!(cfg.token.as_deref(), Some("tok"));
        assert_eq!(cfg.name.as_deref(), Some("builder-1"));
        assert_eq!(
            cfg.labels.as_deref(),
            Some(["linux".to_string(), "docker".to_string()].as_slice())
        );
    }

    /// A typo in the remote server key used to deserialize successfully with
    /// `server = None`, after which resolution quietly selected localhost. Drive
    /// the production loader so the path, rejected key and remediation all stay
    /// attached to the refusal.
    #[test]
    fn a_misspelled_server_key_is_rejected_before_it_can_fall_back_to_localhost() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runner.toml");
        write_test_config(&path, "sever = \"https://forge.example\"\n");

        let error = load_config(path.to_str().unwrap())
            .expect_err("an unknown runner setting must not be discarded");

        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(path.to_str().unwrap()),
            "error must name the config path: {rendered}"
        );
        assert!(
            rendered.contains("unknown field `sever`"),
            "error must name the rejected setting: {rendered}"
        );
        assert!(
            rendered.contains("forgekeep-runner register --save"),
            "error must carry the remediation hint: {rendered}"
        );
    }

    /// The bug: a broken TOML went through `.ok()?` and became `None`, so the
    /// runner behaved exactly as if the operator had never written the file.
    #[test]
    fn a_malformed_config_file_is_reported_with_path_and_cause() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runner.toml");
        write_test_config(
            &path,
            "server = \"http://127.0.0.1:8080\"\nthis is not toml\n",
        );

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
        assert_eq!(loaded.allow_insecure_http, Some(false));
        assert_eq!(loaded.runner_id, Some(7));
        assert_eq!(loaded.token.as_deref(), Some("tok"));
        assert_eq!(loaded.name.as_deref(), Some("builder-1"));
        assert_eq!(
            loaded.labels.as_deref(),
            Some(["linux".to_string()].as_slice())
        );
    }

    /// The runner token is the runner's whole identity, so the file that holds
    /// it must not be readable by other local accounts — the same rule the
    /// server applies to `forgekeep.toml` and to its at-rest key file.
    #[cfg(unix)]
    #[test]
    fn a_group_or_world_readable_runner_config_is_refused_until_it_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runner.toml");
        write_test_config(
            &path,
            "server = \"http://127.0.0.1:8080\"\ntoken = \"tok\"\n",
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let error = load_config(path.to_str().unwrap())
            .expect_err("a runner config readable by other local accounts must be refused");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(path.to_str().unwrap()),
            "error must name the config path: {rendered}"
        );
        assert!(
            rendered.contains("mode 0644"),
            "error must report the observed mode: {rendered}"
        );
        assert!(
            rendered.contains("chmod 600"),
            "error must carry the remediation: {rendered}"
        );

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        load_config(path.to_str().unwrap())
            .expect("the same owner-only config must load")
            .expect("a config file that exists must yield Some");
    }

    /// `register --save` is what creates this file in the first place, so the
    /// token must never touch a group- or world-readable inode: a file created
    /// through the ambient umask is `0644` on a stock host, and telling the
    /// operator to `chmod` afterwards would be a race they cannot win.
    ///
    /// The assertion is `0600` exactly rather than "no group/world bits":
    /// `O_CREAT` masks the requested mode with the umask, which can only
    /// *remove* bits, so a permissive umask cannot widen the result — while the
    /// `std::fs::write` this replaced would land on `0644` under the umask this
    /// test runs with.
    #[cfg(unix)]
    #[test]
    fn a_saved_config_is_owner_only_from_the_first_byte() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("runner.toml");
        let shown = path.to_str().unwrap();

        save_config(shown, &sample_config()).expect("a writable path must save");

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a freshly saved runner config must be 0600");
    }

    /// Rewriting an existing config must also *narrow* it: `OpenOptions::mode`
    /// only applies to a file the call creates, so a save that opened the target
    /// in place would leave an already-`0644` file exactly as wide as it found
    /// it — and re-registration is precisely when an operator expects the file
    /// to be fixed, not preserved.
    #[cfg(unix)]
    #[test]
    fn saving_over_a_world_readable_config_narrows_it() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runner.toml");
        std::fs::write(&path, "server = \"http://127.0.0.1:8080\"\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        save_config(path.to_str().unwrap(), &sample_config()).expect("an existing path must save");

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "a rewritten runner config must not stay 0644");
        let loaded = load_config(path.to_str().unwrap())
            .expect("the narrowed config must load")
            .expect("a config file that exists must yield Some");
        assert_eq!(loaded.token.as_deref(), Some("tok"));
    }

    /// A failed save must not leave the token lying next to the config path in
    /// a temp file nobody will ever look at.
    #[test]
    fn a_failed_save_leaves_no_temp_copy_of_the_token_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runner.toml");
        std::fs::create_dir(&path).unwrap();

        save_config(path.to_str().unwrap(), &sample_config())
            .expect_err("writing onto a directory must fail");

        let leftovers: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|entry| {
                let name = entry.ok()?.file_name().to_string_lossy().into_owned();
                (name != "runner.toml").then_some(name)
            })
            .collect();
        assert!(
            leftovers.is_empty(),
            "a failed save left files behind: {leftovers:?}"
        );
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
                allow_insecure_http: false,
                identity: RunnerIdentity::Existing {
                    runner_id: 7,
                    token: "tok".to_string(),
                },
                repository: Some("owner/project".to_string()),
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
            allow_insecure_http: true,
            repository: Some("flag/project".to_string()),
            name: Some("from-flag".to_string()),
            labels: Some("docker,amd64".to_string()),
            label: Vec::new(),
            token: Some("cli-tok".to_string()),
            runner_id: Some(42),
        };

        let resolved = resolve_runner(cli, Some(&sample_config())).unwrap();

        assert_eq!(
            resolved,
            ResolvedRunner {
                server: "https://ci.example.com".to_string(),
                allow_insecure_http: true,
                identity: RunnerIdentity::Existing {
                    runner_id: 42,
                    token: "cli-tok".to_string(),
                },
                repository: Some("flag/project".to_string()),
                name: "from-flag".to_string(),
                labels: vec!["docker".to_string(), "amd64".to_string()],
            }
        );
    }

    #[test]
    fn insecure_http_opt_in_can_come_from_the_config_or_the_cli() {
        let cfg = RunnerConfig {
            allow_insecure_http: Some(true),
            ..sample_config()
        };
        assert!(
            resolve_runner(RunnerCliArgs::default(), Some(&cfg))
                .unwrap()
                .allow_insecure_http
        );

        let cli = RunnerCliArgs {
            allow_insecure_http: true,
            ..RunnerCliArgs::default()
        };
        assert!(
            resolve_runner(cli, Some(&sample_config()))
                .unwrap()
                .allow_insecure_http
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

    #[test]
    fn structural_label_flags_preserve_commas_and_whitespace() {
        let cli = RunnerCliArgs {
            label: vec!["gpu,a100".to_string(), " linux ".to_string()],
            ..RunnerCliArgs::default()
        };

        let resolved = resolve_runner(cli, Some(&sample_config())).unwrap();

        assert_eq!(resolved.labels, ["gpu,a100", " linux "]);
    }

    #[test]
    fn runner_toml_labels_preserve_a_comma_bearing_element() {
        let cfg = RunnerConfig {
            labels: Some(vec!["gpu,a100".to_string()]),
            ..sample_config()
        };

        let resolved = resolve_runner(RunnerCliArgs::default(), Some(&cfg)).unwrap();

        assert_eq!(resolved.labels, ["gpu,a100"]);
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
    fn legacy_labels_trim_and_drop_blank_entries() {
        assert_eq!(
            parse_legacy_labels("docker, linux ,,  ,amd64,"),
            vec![
                "docker".to_string(),
                "linux".to_string(),
                "amd64".to_string()
            ]
        );
        assert!(parse_legacy_labels("  ").is_empty());
    }

    // ---------------------------------------------------------------------
    // `runner.toml` against the page that describes it.
    //
    // The file is written by `register --save`, but an operator edits it by
    // hand the moment a runner moves to another server — and `RunnerConfig` is
    // `deny_unknown_fields`, so there is no half-working middle here: a key is
    // either spelled the way the model declares it or the runner refuses to
    // start. Until the README grew a section for it, the only place to look the
    // spelling up was this file.
    // ---------------------------------------------------------------------

    /// The page an operator of `forgekeep-runner` is pointed at.
    ///
    /// `include_str!` rather than a runtime read: the path resolves at compile
    /// time (a moved README breaks the build instead of silently skipping the
    /// checks), and editing the page rebuilds — and so re-runs — these tests.
    const README: (&str, &str) = ("README.md", include_str!("../../../README.md"));

    /// The heading that opens the runner's half of the README. Everything up to
    /// the next `## ` heading is the section these checks read.
    const RUNNER_SECTION: &str = "## CI runner (`forgekeep-runner`)";

    /// The production code of this file, with complete test items blanked so a
    /// key that exists only in a fixture cannot pass for a key of the model.
    fn production_config_source() -> String {
        rust_source::production_rust_code_only(include_str!("config.rs"))
    }

    /// The production code and doc comments of `cli.rs`, where the `--help` an
    /// operator reads is generated.
    ///
    /// Its text rather than its types: `cli.rs` is compiled into the
    /// `forgekeep-runner` *binary* and this file into the library, so the two
    /// never see each other's items — but the check below only needs the help
    /// text, and reading the declaration is the whole point, since a marker
    /// added to a flag joins the contract by existing.
    fn production_cli_source() -> String {
        rust_source::production_rust_code_with_doc_comments(include_str!("cli.rs"))
    }

    /// Every `[config: key]` marker of the help text, with the line it sits on.
    fn help_config_markers(source: &str) -> Vec<(usize, &str)> {
        const MARKER: &str = "[config: ";

        let mut markers = Vec::new();

        for (index, line) in source.lines().enumerate() {
            let line_no = index + 1;
            let Some((_, rest)) = line.split_once(MARKER) else {
                continue;
            };

            // `[config: [server].http_addr]` is the *server's* spelling, checked
            // in `rg-cli` against its `ConfigFile`. One here would point an
            // operator of the runner at the wrong file entirely.
            assert!(
                !rest.starts_with('['),
                "cli.rs:{line_no}: `{MARKER}[section].key]` names a key of `forgekeep.toml`, \
                 but `forgekeep-runner` reads `runner.toml`, whose keys have no section"
            );

            let (key, _) = rest
                .split_once(']')
                .unwrap_or_else(|| panic!("cli.rs:{line_no}: `{MARKER}` marker never closes"));
            markers.push((line_no, key));
        }

        markers
    }

    /// `--help` is the only place a runner flag's `runner.toml` equivalent is
    /// named, and the file is `deny_unknown_fields`: renaming a field of
    /// `RunnerConfig` leaves the help text pointing at a key the model has not
    /// got, and the operator who follows it gets `unknown field` on the next
    /// start instead of a runner.
    ///
    /// Only the key's *existence* is asserted, not its type — the probe value is
    /// arbitrary, so a type mismatch is this test's noise while `unknown field`
    /// is exactly its signal.
    #[test]
    fn every_config_key_named_in_the_runner_help_is_a_real_key() {
        fn unknown_field_error(document: &str) -> Option<String> {
            let error = toml::from_str::<RunnerConfig>(document).err()?;
            let error = error.to_string();
            error.contains("unknown field").then_some(error)
        }

        // The detector has to bite before its silence means anything.
        assert!(
            unknown_field_error("not_a_real_key = \"probe\"\n").is_some(),
            "RunnerConfig no longer rejects unknown keys, so this test cannot tell a real \
             `runner.toml` key from an invented one"
        );

        let source = production_cli_source();
        let markers = help_config_markers(&source);
        let named: BTreeSet<&str> = markers.iter().map(|(_, key)| *key).collect();

        // A floor, not a count: `RunnerConfig` declares five keys and the help
        // names every one of them, so a scanner that stopped matching would
        // otherwise read as agreement.
        assert!(
            named.len() >= 5,
            "only {} distinct `[config: …]` keys found in cli.rs — the help-text scanner has \
             stopped matching them",
            named.len()
        );

        for (line_no, key) in &markers {
            if let Some(error) = unknown_field_error(&format!("{key} = \"probe\"\n")) {
                panic!(
                    "cli.rs:{line_no}: `--help` tells the operator that this flag's \
                     `runner.toml` equivalent is `{key}`, and `RunnerConfig` has no such key \
                     — writing it into the file refuses the next start. ({error})"
                );
            }
        }
    }

    /// The body of the markdown section introduced by `heading`.
    fn markdown_section<'a>(name: &str, content: &'a str, heading: &str) -> &'a str {
        let (_, rest) = content.split_once(heading).unwrap_or_else(|| {
            panic!(
                "{name} no longer has a `{heading}` section — the runner's own configuration \
                 is documented nowhere else"
            )
        });

        match rest.split_once("\n## ") {
            Some((section, _)) => section,
            None => rest,
        }
    }

    /// The ```toml fenced blocks of a markdown fragment.
    fn toml_code_blocks(name: &str, content: &str) -> Vec<String> {
        let mut blocks = Vec::new();
        let mut body: Vec<&str> = Vec::new();
        let mut inside = false;

        for line in content.lines() {
            let trimmed = line.trim();
            if inside {
                if trimmed == "```" {
                    blocks.push(body.join("\n"));
                    body.clear();
                    inside = false;
                } else {
                    body.push(line);
                }
            } else if trimmed == "```toml" {
                inside = true;
            }
        }

        assert!(
            !inside,
            "{name}: a ```toml block in the `{RUNNER_SECTION}` section is never closed"
        );
        blocks
    }

    /// The body of the `RunnerConfig` declaration, delimited on the code-only
    /// view and returned from both views at once.
    ///
    /// The boundaries have to come from the code-only view, where a
    /// declaration-shaped comment or literal cannot open a body and a `}` inside
    /// a literal cannot close one early. The source half has to come back with
    /// it, because a `#[serde(rename = "…")]` spells its key in a string
    /// literal — which is precisely what the code-only view has blanked. The
    /// two views are byte-aligned, so one pair of offsets addresses both.
    fn runner_config_body<'a, 'c>(source: &'a str, code: &'c str) -> Option<(&'a str, &'c str)> {
        const DECLARATION: &str = "pub(crate) struct RunnerConfig {";

        let open = code.find(DECLARATION)?;
        let start = open + DECLARATION.len();
        let mut braces = 1usize;

        for (relative, byte) in code.as_bytes()[start..].iter().enumerate() {
            match byte {
                b'{' => braces += 1,
                b'}' => {
                    braces = braces.saturating_sub(1);
                    if braces == 0 {
                        let end = start + relative;
                        return Some((source.get(start..end)?, code.get(start..end)?));
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// The keys `RunnerConfig` declares, under the names an operator writes.
    ///
    /// Read off the declaration rather than listed beside it: a key added to the
    /// model joins the contract below by existing, not by being remembered.
    ///
    /// Takes the file's bytes and builds both production views itself, so the
    /// self-check below exercises the path the census really walks. It used to
    /// take one view and read the renamed key out of it, which cannot work in
    /// either view alone: structure lives where literals are blanked, and the
    /// key's text lives where declaration-shaped prose is not.
    fn declared_keys(text: &str) -> Vec<String> {
        let source = rust_source::production_rust_source(text);
        let code = rust_source::production_rust_code_only(text);
        let (source_body, code_body) = runner_config_body(&source, &code)
            .expect("the RunnerConfig declaration must be present in config.rs");

        let mut keys = Vec::new();
        let mut attributes = String::new();

        // Byte-aligned views blank in place, so the two bodies have the same
        // lines in the same order: structure is read off `code`, the renamed
        // key off `source`, one line at a time.
        for (structure, literal) in code_body.lines().zip(source_body.lines()) {
            let structure = structure.trim();
            if structure.is_empty() {
                attributes.clear();
                continue;
            }
            if structure.starts_with("#[") {
                attributes.push_str(literal.trim());
                continue;
            }
            let Some((field, _)) = structure
                .strip_prefix("pub(crate) ")
                .and_then(|declaration| declaration.split_once(':'))
            else {
                continue;
            };
            let renamed = attributes
                .split_once("rename = \"")
                .and_then(|(_, rest)| rest.split_once('"'))
                .map(|(name, _)| name.to_owned());
            keys.push(renamed.unwrap_or_else(|| field.trim().to_owned()));
            attributes.clear();
        }
        keys
    }

    /// The example an operator copies has to be a file the runner accepts.
    #[test]
    fn every_toml_block_in_the_readme_runner_section_loads_as_a_runner_config() {
        // `deny_unknown_fields` is what makes a drifted key fatal rather than
        // cosmetic; if it ever comes off, this whole check stops meaning
        // anything and says so instead of staying green.
        assert!(
            toml::from_str::<RunnerConfig>("not_a_runner_key = \"probe\"\n").is_err(),
            "RunnerConfig no longer rejects unknown keys, so a key that drifted in the \
             README would load fine here and still fail on a real runner"
        );

        let (name, content) = README;
        let blocks = toml_code_blocks(name, markdown_section(name, content, RUNNER_SECTION));
        assert!(
            !blocks.is_empty(),
            "{name}: the `{RUNNER_SECTION}` section shows no ```toml block — the operator \
             has no `runner.toml` to copy"
        );

        for block in &blocks {
            if let Err(error) = toml::from_str::<RunnerConfig>(block) {
                panic!(
                    "{name}: a ```toml block of the `{RUNNER_SECTION}` section is not a file \
                     `forgekeep-runner` accepts — pasting it fails the start. ({error})\n{block}"
                );
            }
        }
    }

    /// The mirror of the check above: that one asks that everything the page
    /// shows is real, this asks that everything real is shown. A key nobody can
    /// discover is guessed, and a guess is a runner that will not start.
    #[test]
    fn every_key_the_runner_config_accepts_is_shown_in_the_readme() {
        // The fixture goes in as file bytes, so it walks the very path the
        // census walks: both production views, boundaries off the code-only
        // one, the renamed key off its string-bearing twin. Fed a single view
        // instead — which is what this self-check used to do — a `rename` is
        // unreadable in principle: the code-only view has blanked the literal,
        // and the source view lets declaration-shaped prose move the body.
        assert_eq!(
            declared_keys(
                "pub(crate) struct RunnerConfig {\n    pub(crate) server: Option<String>,\n    \
                 #[serde(rename = \"id\")]\n    pub(crate) runner_id: Option<i64>,\n}\n"
            ),
            vec!["server".to_string(), "id".to_string()],
            "the declaration scan does not read a renamed key the way serde does — it would \
             hold the README to the Rust field name while the model accepts only the rename"
        );

        // Declaration-shaped prose in all four spellings the lexer has to tell
        // from code. Each decoy carries a whole fake `RunnerConfig`, closing
        // brace included, and sits *above* the real one: a reader that took its
        // boundaries from the source view would census the first decoy instead.
        assert_eq!(
            declared_keys(
                r##"
// pub(crate) struct RunnerConfig {
//     pub(crate) decoy_comment: Option<String>,
// }
const NORMAL: &str = "pub(crate) struct RunnerConfig {\n    pub(crate) decoy_normal: Option<String>,\n}";
const RAW: &str = r#"pub(crate) struct RunnerConfig {
    pub(crate) decoy_raw: Option<String>,
}"#;
const BYTES: &[u8] = b"pub(crate) struct RunnerConfig {\n    pub(crate) decoy_byte: Option<String>,\n}";

pub(crate) struct RunnerConfig {
    #[serde(rename = "id")]
    pub(crate) runner_id: Option<i64>,
    pub(crate) note: Option<String>,
}
"##
            ),
            vec!["id".to_string(), "note".to_string()],
            "a declaration-shaped comment or literal moved the body the key census reads"
        );

        let keys = declared_keys(include_str!("config.rs"));
        assert!(
            keys.len() >= 5,
            "only {} keys found in the RunnerConfig declaration — the scan has stopped \
             matching it",
            keys.len()
        );

        let (name, content) = README;
        let section = markdown_section(name, content, RUNNER_SECTION);
        let shown = toml_code_blocks(name, section).join("\n");

        for key in &keys {
            assert!(
                shown.lines().any(|line| line
                    .trim()
                    .split_once(" =")
                    .is_some_and(|(shown, _)| shown == key)),
                "`{key}` is a key of `runner.toml` that no ```toml block of the \
                 `{RUNNER_SECTION}` README section shows — the only place left to look it \
                 up is this file, and `deny_unknown_fields` turns a guess into a refused \
                 start"
            );
            assert!(
                section.contains(&format!("`{key}`")),
                "`{key}` appears in the README's `runner.toml` example but is explained \
                 nowhere in the `{RUNNER_SECTION}` section — say what an operator writes \
                 there"
            );
        }
    }

    // ---------------------------------------------------------------------
    // The *values* those same lines promise.
    //
    // The checks above pin the names: every `[config: …]` marker names a key
    // `RunnerConfig` really has, and every key it has is shown. What none of
    // them looks at is the address written beside one. `--server` deliberately
    // carries no clap `default_value` — with one, "the operator typed
    // localhost" and "the flag was not passed" become the same thing, which is
    // how a runner registered against a remote server used to go back to
    // localhost on its next start — so clap cannot print the default itself.
    // Every `[default: …]` is therefore a value a person typed next to
    // `DEFAULT_SERVER` and joined to it by memory alone, and there were four
    // such copies: two in this binary's help, one in the README table an
    // operator reads before running anything, and one in the deprecated
    // `forgekeep runner` alias of the *other* binary — a crate this constant is
    // not even visible from.
    // ---------------------------------------------------------------------

    /// A built-in default the operator-facing pages state, bound to the constant
    /// that actually produces it.
    struct DocumentedDefault {
        /// The `runner.toml` key named by the flag's `[config: …]` marker, which
        /// is also how the README table spells its row. Keys here carry no
        /// section — that is what tells them from `forgekeep.toml`'s.
        key: &'static str,
        /// The `config::DEFAULT_*` this row pairs, for the census below.
        constant: &'static str,
        /// Its value, read from the constant rather than copied beside it.
        value: String,
    }

    /// The pairing table. The key spellings have to be written out — no rule
    /// derives `DEFAULT_SERVER` from `server` — but the *values* never are: each
    /// row reads its constant, so renaming one breaks the build and changing one
    /// fails every check below.
    fn documented_defaults() -> Vec<DocumentedDefault> {
        macro_rules! defaults {
            ($(($key:literal, $konst:ident)),+ $(,)?) => {
                vec![$(DocumentedDefault {
                    key: $key,
                    constant: stringify!($konst),
                    value: super::$konst.to_string(),
                }),+]
            };
        }

        defaults![("server", DEFAULT_SERVER)]
    }

    /// `[default: …]` notes that state a *behaviour* rather than a value, each
    /// with the reason. Without the list such a note would have to be either
    /// banned or waved through, and waving one through is what lets an unpaired
    /// value in beside it.
    const DEFAULTS_NOT_FROM_A_CONSTANT: [(&str, &str, &str); 1] = [(
        "name",
        "system hostname",
        "behaviour, not a value: the name is asked of the machine at startup \
         (`system_hostname`), so there is no constant stating it — `FALLBACK_NAME` is only \
         what a host that cannot name itself falls back to",
    )];

    /// The `const DEFAULT_*` names `source` declares, whatever their visibility:
    /// a default that is private today is still a default an operator meets.
    /// Reading the declarations rather than keeping a list beside them is the
    /// whole point — a constant added to the model joins the census by existing.
    fn declared_default_constants(source: &str) -> BTreeSet<&str> {
        source
            .lines()
            .map(str::trim_start)
            .map(|line| line.strip_prefix("pub(crate) ").unwrap_or(line))
            .filter_map(|line| line.strip_prefix("const "))
            .filter_map(|rest| rest.split_once(':'))
            .map(|(name, _)| name.trim())
            .filter(|name| name.starts_with("DEFAULT_"))
            .collect()
    }

    /// The doc-comment paragraph that starts at `line_no`: the marker's own line
    /// plus every `///` line following it.
    ///
    /// Stopping at the first line that is not a doc comment is what keeps the
    /// *next* flag's note out of this flag's paragraph — clap builds one
    /// paragraph per flag, and a reader that ran on would let a neighbour's
    /// default stand in for a missing one.
    fn help_paragraph(lines: &[&str], line_no: usize) -> String {
        let start = line_no - 1;
        let mut paragraph = vec![lines[start]];
        paragraph.extend(
            lines
                .get(start + 1..)
                .unwrap_or_default()
                .iter()
                .copied()
                .take_while(|line| line.trim_start().starts_with("///")),
        );
        paragraph.join(" ")
    }

    /// The value the help paragraph starting at `line_no` promises as its
    /// default, if it promises one. Both spellings `cli.rs` uses are read: the
    /// note on the marker's own line and the one wrapped onto the next.
    fn help_promised_default(lines: &[&str], line_no: usize) -> Option<String> {
        let paragraph = help_paragraph(lines, line_no);
        let (_, rest) = paragraph.split_once("[default: ")?;
        let (value, _) = rest.split_once(']')?;
        Some(value.to_string())
    }

    /// Is a `[default: …]` promise one this crate accounts for — produced by a
    /// paired constant, or excused as a behaviour?
    fn default_is_accounted_for(documented: &[DocumentedDefault], key: &str, value: &str) -> bool {
        documented
            .iter()
            .any(|entry| entry.key == key && entry.value == value)
            || DEFAULTS_NOT_FROM_A_CONSTANT
                .iter()
                .any(|&(excused, promised, _)| excused == key && promised == value)
    }

    /// Every `[config: key]` marker of `lines`, as `(line number, key)`.
    ///
    /// The bracket-form guard of [`help_config_markers`] is deliberately absent:
    /// this one also reads the *other* crate's file, where `[config: [section].key]`
    /// is the normal spelling and `rg-cli`'s own tests are what police it.
    fn config_markers<'a>(lines: &[&'a str]) -> Vec<(usize, &'a str)> {
        lines
            .iter()
            .enumerate()
            .filter_map(|(index, line)| {
                let (_, rest) = line.split_once("[config: ")?;
                let (key, _) = rest.split_once(']')?;
                Some((index + 1, key))
            })
            .collect()
    }

    /// A sample carrying both spellings and the boundary between two flags, so
    /// every check below can show its reader answering "no" before its "yes" is
    /// worth anything.
    const HELP_SAMPLE: [&str; 8] = [
        "        /// ForgeKeep server URL [config: server]",
        "        /// [default: http://probe]",
        "        #[arg(long)]",
        "        server: Option<String>,",
        "",
        "        /// Runner labels [config: labels]",
        "        #[arg(long)]",
        "        /// Runner name [config: name] [default: probe hostname]",
    ];

    /// `--server` carries no clap `default_value` on purpose, so `--help` cannot
    /// print the default: the address beside `[default: …]` is typed by hand next
    /// to `DEFAULT_SERVER`. Changing the constant leaves both subcommands' help
    /// naming an address the runner will not use, and the operator debugging
    /// "why did it register against localhost" reads the stale one.
    #[test]
    fn every_default_the_runner_help_promises_is_the_constant_that_produces_it() {
        assert_eq!(
            help_promised_default(&HELP_SAMPLE, 1).as_deref(),
            Some("http://probe"),
            "the reader misses a `[default: …]` note wrapped onto the next line"
        );
        assert_eq!(
            help_promised_default(&HELP_SAMPLE, 8).as_deref(),
            Some("probe hostname"),
            "the reader misses a `[default: …]` note on the marker's own line"
        );
        assert_eq!(
            help_promised_default(&HELP_SAMPLE, 6),
            None,
            "the reader runs past the line that ends a flag's paragraph, so the next flag's \
             default would pass for one this flag never states"
        );

        let source = production_cli_source();
        let lines: Vec<&str> = source.lines().collect();
        let markers = config_markers(&lines);
        let mut checked = 0;

        for entry in &documented_defaults() {
            let mut seen = 0;
            for &(line_no, key) in &markers {
                if key != entry.key {
                    continue;
                }
                seen += 1;
                assert_eq!(
                    help_promised_default(&lines, line_no).as_deref(),
                    Some(entry.value.as_str()),
                    "cli.rs:{line_no}: `--help` points this flag at the `{}` key of \
                     `runner.toml` but does not promise `[default: {}]` — `config::{}` is \
                     what the runner actually falls back to, so the help states a value it \
                     will not use",
                    entry.key,
                    entry.value,
                    entry.constant
                );
            }
            // A floor, not a count: `register` and `run` both name this key, so
            // a marker scan that stopped matching would otherwise read as
            // agreement.
            assert!(
                seen >= 2,
                "only {seen} `[config: {}]` markers left in cli.rs, so nothing pins \
                 `config::{}` to the help of both subcommands — drop the row or restore the \
                 marker",
                entry.key,
                entry.constant
            );
            checked += seen;
        }

        assert!(
            checked >= 2,
            "only {checked} help markers were matched against a `config::DEFAULT_*` — the \
             pairing table has drifted away from the help text"
        );
    }

    /// Both directions of the pairing table, so neither side can rot in silence:
    /// a new `DEFAULT_*` that no page names, and a `[default: …]` promise no
    /// constant produces.
    #[test]
    fn every_runner_default_is_either_paired_with_a_constant_or_excused() {
        assert_eq!(
            declared_default_constants(
                "pub(crate) const DEFAULT_X: &str = \"1\";\nconst DEFAULT_Y: u8 = 2;\n\
                 const FALLBACK_Z: u8 = 3;\n"
            ),
            BTreeSet::from(["DEFAULT_X", "DEFAULT_Y"]),
            "the declaration scan does not read `const DEFAULT_*` the way config.rs writes \
             it — a private one would escape the census entirely"
        );

        let source = production_config_source();
        let declared = declared_default_constants(&source);
        assert!(
            !declared.is_empty(),
            "no `DEFAULT_*` constant found in config.rs — the declaration scan has stopped \
             matching them"
        );

        let documented = documented_defaults();
        let paired: BTreeSet<&str> = documented.iter().map(|entry| entry.constant).collect();

        for name in &declared {
            assert!(
                paired.contains(name),
                "`config::{name}` is a built-in default that no row of documented_defaults() \
                 pins to a `[config: …]` marker — pair it with the flag whose `--help` \
                 promises it, so the two cannot drift apart"
            );
        }

        // Renaming a paired constant breaks the build, but *moving* one out of
        // config.rs would not: it would simply leave the census, taking its row
        // with it.
        for entry in &documented {
            assert!(
                declared.contains(entry.constant),
                "documented_defaults() pairs `config::{}`, which config.rs no longer declares \
                 — the census reads that one file, so a constant that moved elsewhere escapes \
                 it",
                entry.constant
            );
        }

        let source = production_cli_source();
        let lines: Vec<&str> = source.lines().collect();
        let markers = config_markers(&lines);
        let mut promises = 0;

        for &(line_no, key) in &markers {
            let Some(value) = help_promised_default(&lines, line_no) else {
                continue;
            };
            promises += 1;
            assert!(
                default_is_accounted_for(&documented, key, &value),
                "cli.rs:{line_no}: `--help` promises `[default: {value}]` for the `{key}` key \
                 of `runner.toml`, and nothing in config.rs produces that value — pair the \
                 key with the constant it comes from, or name it in \
                 DEFAULTS_NOT_FROM_A_CONSTANT with the reason it states a behaviour rather \
                 than a value"
            );
        }

        // Four today: `--server` and `--name`, in `register` and in `run`.
        assert!(
            promises >= 4,
            "only {promises} `[default: …]` promises found in cli.rs — the reader has stopped \
             matching them, so this census would agree with anything"
        );

        for (key, value, _) in DEFAULTS_NOT_FROM_A_CONSTANT {
            assert!(
                markers.iter().any(|&(line_no, marker)| marker == key
                    && help_promised_default(&lines, line_no).as_deref() == Some(value)),
                "DEFAULTS_NOT_FROM_A_CONSTANT still excuses `[default: {value}]` for `{key}`, \
                 which cli.rs no longer promises — drop the entry so the list keeps meaning \
                 something"
            );
        }
    }

    /// The value a `(default …)` note in `text` states, when it states one as a
    /// literal. A note describing a behaviour — `(default: system hostname)` —
    /// carries no backticked value and is not one of these.
    fn prose_promised_default(text: &str) -> Option<&str> {
        let (_, rest) = text.split_once("default")?;
        let (_, rest) = rest.split_once('`')?;
        let (value, _) = rest.split_once('`')?;
        Some(value)
    }

    /// The row of the section's `| Key | … |` table whose first cell names `key`.
    fn table_row<'a>(section: &'a str, key: &str) -> Option<&'a str> {
        let cell = format!("`{key}`");
        section.lines().find(|line| {
            line.trim_start()
                .strip_prefix('|')
                .and_then(|row| row.split('|').next())
                .is_some_and(|first| first.trim() == cell)
        })
    }

    /// The README states the same address a third time, in the one page an
    /// operator reads *before* running anything — and it is the page that
    /// explains what to write into `runner.toml` by hand when a runner moves to
    /// another server, which is exactly when a stale address costs an afternoon.
    #[test]
    fn every_default_the_readme_runner_table_states_is_the_constant_that_produces_it() {
        assert_eq!(
            prose_promised_default("| `server` | `--server` | Base URL (default `http://probe`) |"),
            Some("http://probe"),
            "the cell reader misses a backticked `(default …)` note"
        );
        assert_eq!(
            prose_promised_default("| `name` | `--name` | Display name (default: a hostname) |"),
            None,
            "the cell reader invents a literal default out of prose that only describes a \
             behaviour"
        );
        assert!(
            table_row("| `server` | `--server` | probe |\n", "serve").is_none(),
            "the row reader matches a prefix of a key, so the wrong row would be checked"
        );

        let (name, content) = README;
        let section = markdown_section(name, content, RUNNER_SECTION);
        let mut stated = 0;

        for entry in &documented_defaults() {
            let row = table_row(section, entry.key).unwrap_or_else(|| {
                panic!(
                    "{name}: the `{RUNNER_SECTION}` table has no `{}` row — that table is \
                     where an operator looks the key up, and `config::{}` is the value it \
                     gets without one",
                    entry.key, entry.constant
                )
            });

            let Some(promised) = prose_promised_default(row) else {
                continue;
            };
            stated += 1;
            assert_eq!(
                promised, entry.value,
                "{name}: the `{}` row of the `{RUNNER_SECTION}` table states \
                 `default \\`{promised}\\``, but `config::{}` is `{}` — the page an operator \
                 reads first names an address the runner will not use",
                entry.key, entry.constant, entry.value
            );
        }

        // A floor, not a count: the table states one literal default today, and
        // a reader that stopped matching it would otherwise pass silently.
        assert!(
            stated >= 1,
            "no literal `(default …)` note found in the `{RUNNER_SECTION}` table — either the \
             cell reader has stopped matching it, or the table stopped stating the address, \
             in which case drop this check with it"
        );
    }

    /// The other binary's deprecated `forgekeep runner` alias declares these very
    /// flags a second time.
    ///
    /// Its `--server` help used to restate the address as a third copy, in a
    /// crate `DEFAULT_SERVER` is not visible from — `config` is private to this
    /// library, so nothing there could have bound it and it could only drift. The
    /// note is gone and the alias now sends the reader to `forgekeep-runner run
    /// --help`; this check is what keeps a re-added one bound to the constant.
    #[test]
    fn the_deprecated_alias_promises_no_runner_default_of_its_own() {
        let (offset, block) = alias_help_block();
        let lines: Vec<&str> = block.lines().collect();
        let documented = documented_defaults();
        let mut checked = 0;

        for (line_no, key) in config_markers(&lines) {
            let Some(value) = help_promised_default(&lines, line_no) else {
                continue;
            };
            checked += 1;
            assert!(
                default_is_accounted_for(&documented, key, &value),
                "{}:{}: the deprecated `forgekeep runner` alias promises \
                 `[default: {value}]` for the `{key}` key of `runner.toml`, and nothing in \
                 this crate produces that value. The alias delegates to \
                 `forgekeep-runner run`, so its help must state that runner's defaults or \
                 none at all — `config::DEFAULT_SERVER` is unreachable from there, which is \
                 precisely how a third copy survives going stale.",
                ALIAS.0,
                offset + line_no
            );
        }

        assert!(
            checked >= 1,
            "no `[default: …]` promise found in the alias's help — either the reader has \
             stopped matching it, or the alias stopped stating defaults altogether, in which \
             case drop this check with them"
        );
    }

    /// The other binary's `cli.rs`, by path rather than by type: `rg-cli` depends
    /// on this crate and not the other way round, so its `Commands` is out of
    /// reach — and it is the help text that has to be read anyway. `include_str!`
    /// makes a moved file break the build instead of quietly skipping the check.
    const ALIAS: (&str, &str) = (
        "crates/rg-cli/src/cli.rs",
        include_str!("../../rg-cli/src/cli.rs"),
    );

    /// The `Runner { … }` variant of the other binary's `Commands`, with the
    /// number of the `Runner {` line itself — add that to a line number inside
    /// the block to get the one an editor shows, since the block starts on the
    /// line *after* it.
    fn alias_help_block() -> (usize, String) {
        const OPENING: &str = "\n    Runner {\n";
        const CLOSING: &str = "\n    },\n";

        let (name, source) = ALIAS;
        let production = rust_source::production_rust_code_with_doc_comments(source);

        let (before, rest) = production.split_once(OPENING).unwrap_or_else(|| {
            panic!(
                "{name} no longer declares a `Runner {{` variant — if the deprecated alias is \
                 gone, drop this check along with it"
            )
        });
        let (block, _) = rest
            .split_once(CLOSING)
            .unwrap_or_else(|| panic!("{name}: the `Runner {{` variant never closes"));

        (before.lines().count() + 1, block.to_owned())
    }
}
