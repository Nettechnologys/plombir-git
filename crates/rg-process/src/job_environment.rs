//! Which variables a CI job may hand to the host `docker` CLI.
//!
//! `docker run -e KEY` reads the value from the CLI's own environment, which is
//! how a job's secrets stay out of `argv`. That client — and every credential
//! helper it starts to pull an image — runs on the host, outside the container's
//! `--cap-drop=ALL`, and a handful of names reconfigure it before any container
//! exists: the dynamic loader (`LD_PRELOAD` pointing at a library the push
//! itself delivered into the workspace runs that library as the runner user),
//! the client's own settings (`DOCKER_HOST` ships the job and every `-e` secret
//! to a daemon of the job's choosing), the Go runtime, proxies, the trust store,
//! and the files a credential helper reads (`AWS_CONFIG_FILE` can name a
//! `credential_process` to run). Both runners link this crate and nothing
//! heavier, so the rule is written once here.

/// Credential variables a job may set although the Docker CLI also inherits
/// the `AWS_*` family from the operator.
///
/// They are values, not paths, commands or endpoints: an ECR credential helper
/// that sees the job's own key pulls the job's image with credentials the job
/// already holds. Every other `AWS_*` name can point a helper at a file, a
/// process or a URL, and stays the operator's.
const JOB_SETTABLE_AWS_VARIABLES: &[&str] = &[
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_REGION",
    "AWS_DEFAULT_REGION",
    "AWS_DEFAULT_OUTPUT",
];

/// Names the Go runtime of the Docker CLI reads. An explicit list rather than a
/// `GO*` prefix: `GOFLAGS`, `GOPROXY` or `GOPRIVATE` configure the Go
/// *toolchain* a job runs inside its container, never the CLI.
const GO_RUNTIME_VARIABLES: &[&str] = &[
    "GODEBUG",
    "GOTRACEBACK",
    "GOMEMLIMIT",
    "GOMAXPROCS",
    "GOGC",
    "GOTMPDIR",
    "GOENV",
    "GORACE",
];

/// What a Windows process cannot start without, and where the CLI keeps its
/// configuration there.
const WINDOWS_PROCESS_VARIABLES: &[&str] = &[
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMDATA",
];

/// Is this a name `execve` carries as itself?
///
/// Environment entries are `NAME=VALUE` strings and every reader stops at the
/// first `=`, so a variable called `PATH=/tmp` becomes the entry
/// `PATH=/tmp=<value>`, whose effective `PATH` is the job's.
pub fn valid_environment_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('_' | 'A'..='Z' | 'a'..='z'))
        && chars.all(|ch| matches!(ch, '_' | 'A'..='Z' | 'a'..='z' | '0'..='9'))
}

/// Does the host `docker` CLI take this variable from the runner's own
/// environment?
///
/// The CLI starts from an empty environment and gets back what it needs to find
/// and reach the daemon (`DOCKER_*`, including `DOCKER_CONTENT_TRUST`), the
/// proxies and the trust store, `PATH` and `HOME` for its configuration and
/// credential helpers, the locale, and what those helpers read (`AWS_*`,
/// Google Cloud, `pass`, the Secret Service bus). The operator's runner
/// environment used to reach the CLI whole; what is not listed here is not
/// something the CLI or a helper reads.
fn docker_cli_inherits_variable(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    matches!(
        name.as_str(),
        "PATH"
            | "HOME"
            | "TMPDIR"
            | "TMP"
            | "TEMP"
            | "LANG"
            | "HTTP_PROXY"
            | "HTTPS_PROXY"
            | "NO_PROXY"
            | "ALL_PROXY"
            | "SSL_CERT_FILE"
            | "SSL_CERT_DIR"
            | "GOOGLE_APPLICATION_CREDENTIALS"
            | "DBUS_SESSION_BUS_ADDRESS"
            | "XDG_RUNTIME_DIR"
            | "GNUPGHOME"
    ) || WINDOWS_PROCESS_VARIABLES.contains(&name.as_str())
        || ["LC_", "DOCKER_", "AWS_", "CLOUDSDK_", "PASSWORD_STORE_"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

/// Would a job's value for this name change how a *host* process behaves?
///
/// Matched case-insensitively: Windows reads `path` and `PATH` as one variable,
/// and the lowercase proxy spellings are the ones Go and libcurl honour.
/// Everything the CLI inherits from the operator is in here (bar the plain
/// AWS credentials, see [`JOB_SETTABLE_AWS_VARIABLES`]): a job's copy of one of
/// them would be the one the CLI reads.
pub fn is_host_sensitive_variable(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    if JOB_SETTABLE_AWS_VARIABLES.contains(&upper.as_str()) {
        return false;
    }
    docker_cli_inherits_variable(&upper)
        || GO_RUNTIME_VARIABLES.contains(&upper.as_str())
        || upper == "GOOGLE_EXTERNAL_ACCOUNT_ALLOW_EXECUTABLES"
        || ["LD_", "DYLD_", "SSL_CERT_"]
            .iter()
            .any(|prefix| upper.starts_with(prefix))
        || upper.ends_with("_PROXY")
}

/// Can a job variable of this name be handed to a container?
pub fn reaches_container(name: &str) -> bool {
    valid_environment_name(name) && !is_host_sensitive_variable(name)
}

/// The environment a host `docker` CLI starts from: the runner process's own,
/// reduced to [`docker_cli_inherits_variable`].
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
            "ld_preload",
            "LD_LIBRARY_PATH",
            "DYLD_INSERT_LIBRARIES",
            "DOCKER_HOST",
            "DOCKER_CONFIG",
            "DOCKER_CONTENT_TRUST",
            "docker_host",
            "GODEBUG",
            "GOMAXPROCS",
            "GOMEMLIMIT",
            "SSL_CERT_FILE",
            "HTTP_PROXY",
            "https_proxy",
            "no_proxy",
            "ALL_PROXY",
            "FTP_PROXY",
            "PATH",
            "Path",
            "HOME",
            "TMPDIR",
            "TEMP",
            "LANG",
            "LC_ALL",
            "AWS_CONFIG_FILE",
            "AWS_SHARED_CREDENTIALS_FILE",
            "AWS_WEB_IDENTITY_TOKEN_FILE",
            "AWS_CONTAINER_CREDENTIALS_FULL_URI",
            "AWS_ENDPOINT_URL",
            "AWS_PROFILE",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "GOOGLE_EXTERNAL_ACCOUNT_ALLOW_EXECUTABLES",
            "CLOUDSDK_CONFIG",
            "DBUS_SESSION_BUS_ADDRESS",
            "PASSWORD_STORE_DIR",
            "GNUPGHOME",
            "SystemRoot",
        ] {
            assert!(is_host_sensitive_variable(name), "{name}");
            assert!(!reaches_container(name), "{name}");
        }
    }

    #[test]
    fn ordinary_job_variables_reach_the_container() {
        for name in [
            "DEPLOY_TARGET",
            "CI",
            "CI_JOB_TOKEN",
            "MATRIX_NODE_VERSION",
            "NODE_OPTIONS",
            "PROXY_TIMEOUT",
            "MY_LD_FLAGS",
            "LANGUAGE",
            "HOMEPAGE",
            "PATH_PREFIX",
            "GOFLAGS",
            "GOPROXY",
            "GOPRIVATE",
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "aws_default_region",
            "DOCKERFILE_PATH",
        ] {
            assert!(
                reaches_container(name),
                "{name} is an ordinary job variable"
            );
        }
    }

    #[test]
    fn environment_names_are_judged_by_their_execve_shape() {
        for name in ["DEPLOY_TARGET_2", "a", "_PRIVATE", "x9"] {
            assert!(valid_environment_name(name), "{name}");
        }
        for name in [
            "2TARGET",
            "node-version",
            "PATH=/tmp",
            "LD_PRELOAD=/workspace/evil.so",
            "WITH SPACE",
            "HAS\nNEWLINE",
            "",
        ] {
            assert!(!valid_environment_name(name), "{name:?}");
            assert!(!reaches_container(name), "{name:?}");
        }
    }

    /// The two lists are a pair: a name the CLI takes from the operator must
    /// not be one a job can also set, or the job's copy is the one it reads.
    /// The plain AWS credentials are the one deliberate overlap.
    #[test]
    fn whatever_the_cli_inherits_a_job_cannot_set() {
        let mut names: Vec<String> = [
            "PATH",
            "HOME",
            "TMPDIR",
            "TMP",
            "TEMP",
            "LANG",
            "LC_ALL",
            "DOCKER_HOST",
            "DOCKER_CONTENT_TRUST",
            "DOCKER_DEFAULT_PLATFORM",
            "DOCKER_TLS",
            "HTTP_PROXY",
            "https_proxy",
            "SSL_CERT_DIR",
            "AWS_CONFIG_FILE",
            "AWS_WEB_IDENTITY_TOKEN_FILE",
            "AWS_ROLE_ARN",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "CLOUDSDK_CONFIG",
            "DBUS_SESSION_BUS_ADDRESS",
            "XDG_RUNTIME_DIR",
            "PASSWORD_STORE_DIR",
            "GNUPGHOME",
        ]
        .iter()
        .map(|name| name.to_string())
        .collect();
        names.extend(
            WINDOWS_PROCESS_VARIABLES
                .iter()
                .map(|name| name.to_string()),
        );
        for name in &names {
            assert!(docker_cli_inherits_variable(name), "{name}");
            assert!(
                is_host_sensitive_variable(name),
                "{name} is inherited by the CLI yet a job may set it"
            );
        }
        for name in JOB_SETTABLE_AWS_VARIABLES {
            assert!(docker_cli_inherits_variable(name), "{name}");
            assert!(reaches_container(name), "{name}");
        }
        for name in [
            "CI_JOB_TOKEN",
            "DEPLOY_TARGET",
            "SECRET_TOKEN",
            "LD_PRELOAD",
        ] {
            assert!(!docker_cli_inherits_variable(name), "{name}");
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
            assert!(inherited
                .iter()
                .any(|(name, value)| name == "PATH" && *value == path));
        }
    }
}
