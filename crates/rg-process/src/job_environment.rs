//! Which variable names a CI job may place in a host process.
//!
//! A job's variables become the environment of the host `docker` CLI: `docker
//! run -e KEY` reads the value from there, which is how secrets stay out of
//! `argv`. That client runs on the host, outside the container's
//! `--cap-drop=ALL`, and a handful of names reconfigure it before any container
//! exists: the dynamic loader (`LD_PRELOAD` pointing at a library the push
//! itself delivered into the workspace runs that library as the runner user),
//! the client's own settings (`DOCKER_HOST` ships the whole job — every `-e`
//! secret included — to a daemon of the job's choosing), the Go runtime, TLS
//! trust and proxies. Both executors depend on this crate and nothing heavier,
//! so the verdict is written once here; the server re-exports it next to its
//! own reserved names.

/// Would a variable of this name change how a *host* process behaves?
///
/// Matched case-insensitively on purpose: Windows reads `path` and `PATH` as
/// one variable, and the lowercase proxy spellings are the ones libcurl and
/// Go honour. `GO*` is a prefix rather than a list because the Go runtime
/// behind the Docker CLI grows new `GO…` knobs with every release.
pub fn is_host_sensitive_variable(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    matches!(
        name.as_str(),
        "PATH" | "HOME" | "TMPDIR" | "TMP" | "TEMP" | "LANG"
    ) || ["LD_", "DYLD_", "DOCKER_", "GO", "SSL_CERT_", "LC_"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
        || name.ends_with("_PROXY")
}

/// Does the host `docker` CLI inherit this variable from the runner process?
///
/// The CLI is spawned with an empty environment and only these are copied back
/// from the runner's own: what it needs to find and reach the daemon
/// (`DOCKER_*`, proxies, the trust store, `PATH` and `HOME` for its config and
/// credential helpers) plus locale. Every one of them is also refused as a job
/// variable by [`is_host_sensitive_variable`], which is the point: they come
/// from the operator's environment and from nowhere else.
fn docker_cli_inherits_variable(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    matches!(
        name.as_str(),
        "PATH"
            | "HOME"
            | "TMPDIR"
            | "LANG"
            | "DOCKER_HOST"
            | "DOCKER_CONFIG"
            | "DOCKER_CERT_PATH"
            | "DOCKER_TLS_VERIFY"
            | "DOCKER_CONTEXT"
            | "DOCKER_API_VERSION"
            | "HTTP_PROXY"
            | "HTTPS_PROXY"
            | "NO_PROXY"
            | "SSL_CERT_FILE"
            | "SSL_CERT_DIR"
            // What a Windows process cannot start without, and where the CLI
            // keeps its config there.
            | "SYSTEMROOT"
            | "SYSTEMDRIVE"
            | "WINDIR"
            | "COMSPEC"
            | "PATHEXT"
            | "TEMP"
            | "TMP"
            | "USERPROFILE"
            | "HOMEDRIVE"
            | "HOMEPATH"
            | "APPDATA"
            | "LOCALAPPDATA"
            | "PROGRAMDATA"
    ) || name.starts_with("LC_")
}

/// The environment a host `docker` CLI starts from: the runner process's own,
/// reduced to [`docker_cli_inherits_variable`].
///
/// Callers apply it as `command.env_clear().envs(docker_cli_environment())`
/// and only then add the job's variables, each one checked against
/// [`is_host_sensitive_variable`].
pub fn docker_cli_environment() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    std::env::vars_os()
        .filter(|(name, _)| name.to_str().is_some_and(docker_cli_inherits_variable))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_that_reconfigure_the_host_are_refused_in_every_spelling() {
        for name in [
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "ld_preload",
            "DYLD_INSERT_LIBRARIES",
            "DOCKER_HOST",
            "DOCKER_CONFIG",
            "DOCKER_CERT_PATH",
            "DOCKER_TLS_VERIFY",
            "GODEBUG",
            "GOTRACEBACK",
            "GOFLAGS",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "HTTP_PROXY",
            "https_proxy",
            "NO_PROXY",
            "all_proxy",
            "PATH",
            "Path",
            "HOME",
            "TMPDIR",
            "TMP",
            "TEMP",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
        ] {
            assert!(
                is_host_sensitive_variable(name),
                "{name} must be refused as a job variable"
            );
        }
    }

    #[test]
    fn ordinary_job_variables_pass() {
        for name in [
            "DEPLOY_TARGET",
            "CI_JOB_TOKEN",
            "MATRIX_OS",
            "INPUT_TARGET",
            "NODE_OPTIONS",
            "PROXY_TIMEOUT",
            "MY_LD_FLAGS",
            "LANGUAGE",
            "HOMEPAGE",
            "PATH_PREFIX",
        ] {
            assert!(
                !is_host_sensitive_variable(name),
                "{name} is an ordinary job variable"
            );
        }
    }

    /// The two lists are a pair: nothing the CLI inherits from the operator may
    /// be something a job can also set, or the job's copy would be the one the
    /// CLI reads.
    #[test]
    fn whatever_the_cli_inherits_a_job_cannot_set() {
        for name in [
            "PATH",
            "HOME",
            "TMPDIR",
            "LANG",
            "LC_ALL",
            "DOCKER_HOST",
            "DOCKER_CONFIG",
            "DOCKER_CERT_PATH",
            "DOCKER_TLS_VERIFY",
            "DOCKER_CONTEXT",
            "DOCKER_API_VERSION",
            "HTTP_PROXY",
            "https_proxy",
            "no_proxy",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "SystemRoot",
            "TEMP",
            "TMP",
        ] {
            assert!(docker_cli_inherits_variable(name), "{name}");
            assert!(
                is_host_sensitive_variable(name)
                    || matches!(
                        name.to_ascii_uppercase().as_str(),
                        "SYSTEMROOT"
                            | "SYSTEMDRIVE"
                            | "WINDIR"
                            | "COMSPEC"
                            | "PATHEXT"
                            | "USERPROFILE"
                            | "HOMEDRIVE"
                            | "HOMEPATH"
                            | "APPDATA"
                            | "LOCALAPPDATA"
                            | "PROGRAMDATA"
                    ),
                "{name} is inherited by the CLI yet a job may set it"
            );
        }
        for name in [
            "CI_JOB_TOKEN",
            "DEPLOY_TARGET",
            "SECRET_TOKEN",
            "AWS_SECRET_ACCESS_KEY",
        ] {
            assert!(
                !docker_cli_inherits_variable(name),
                "{name} must not be copied from the runner's own environment"
            );
        }
    }

    #[test]
    fn the_cli_environment_is_the_filtered_process_environment() {
        let inherited = docker_cli_environment();
        for (name, _) in &inherited {
            assert!(
                name.to_str().is_some_and(docker_cli_inherits_variable),
                "{name:?} reached the Docker CLI environment"
            );
        }
        if let Some(path) = std::env::var_os("PATH") {
            assert!(
                inherited
                    .iter()
                    .any(|(name, value)| name == "PATH" && *value == path),
                "PATH must be inherited so the CLI can be found and can find its helpers"
            );
        }
    }
}
