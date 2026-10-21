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
//!
//! There are two host clients per job (see [`crate::docker_job`]): the one that
//! finds or pulls the image runs in [`docker_cli_environment`] alone, and only
//! it can start a credential helper; the one that starts the container runs in
//! [`docker_run_environment`] plus the job's variables, and never pulls. So a
//! name only a credential helper reads is the operator's on the first and free
//! for the job on the second — `AWS_PROFILE` is an ordinary variable inside a
//! deploy job.

/// Does only a credential helper read this — never the Docker CLI itself?
///
/// `AWS_*` for `docker-credential-ecr-login`, Google Cloud's for
/// `docker-credential-gcr`, `pass` and the Secret Service for theirs. These are
/// inherited by the client that pulls and left out of the one that runs.
fn credential_helper_variable(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    matches!(
        name.as_str(),
        "GOOGLE_APPLICATION_CREDENTIALS"
            | "GOOGLE_EXTERNAL_ACCOUNT_ALLOW_EXECUTABLES"
            | "DBUS_SESSION_BUS_ADDRESS"
            | "GNUPGHOME"
    ) || ["AWS_", "CLOUDSDK_", "PASSWORD_STORE_"]
        .iter()
        .any(|prefix| name.starts_with(prefix))
}

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
/// Variables whose values belong to the pipeline runner, never to a committed
/// job variable, a repository secret or a step's `$GITHUB_ENV`.
///
/// Both runners inject this vocabulary — the embedded one and, through the
/// server, `plombir-git-runner` — and both drop a job's own copy of any of
/// these names. Kept here, below both, so a new built-in is protected on every
/// path at once; `rg_core::ci::BUILTIN_CI_VARIABLES` is this list.
pub const RUNNER_OWNED_VARIABLES: [&str; 15] = [
    "CI",
    "PLOMBIR_GIT",
    "CI_PIPELINE_ID",
    "CI_COMMIT_SHA",
    "CI_SHA",
    "CI_REF",
    // `github.ref_name`: `CI_REF` by its short name.
    "CI_REF_NAME",
    "CI_EVENT",
    // `github.event.schedule`: the `- cron:` entry a `schedule` run fired for.
    "CI_EVENT_SCHEDULE",
    // `github.head_ref` / `github.base_ref`: the branches of a `pull_request`
    // run, `''` for every other event.
    "CI_HEAD_REF",
    "CI_BASE_REF",
    "CI_REPOSITORY",
    "CI_REPOSITORY_OWNER",
    "CI_JOB_TOKEN",
    "CI_OIDC_TOKEN_URL",
];

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

/// Would a job's value for this name change how the host `docker run` client
/// behaves?
///
/// Matched case-insensitively: Windows reads `path` and `PATH` as one variable,
/// and the lowercase proxy spellings are the ones Go and libcurl honour.
/// Everything that client inherits from the operator is in here: a job's copy
/// of one of them would be the one it reads. What only a credential helper
/// reads is not — that client starts no helper.
pub fn is_host_sensitive_variable(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    docker_run_inherits_variable(&upper)
        || GO_RUNTIME_VARIABLES.contains(&upper.as_str())
        || ["LD_", "DYLD_", "SSL_CERT_"]
            .iter()
            .any(|prefix| upper.starts_with(prefix))
        || upper.ends_with("_PROXY")
}

/// Does the `docker run` client take this from the operator? What the CLI
/// reads, without what only a helper reads.
fn docker_run_inherits_variable(name: &str) -> bool {
    docker_cli_inherits_variable(name) && !credential_helper_variable(name)
}

/// Can a job variable of this name be handed to a container?
pub fn reaches_container(name: &str) -> bool {
    valid_environment_name(name) && !is_host_sensitive_variable(name)
}

/// The environment the host `docker` client that finds and pulls images starts
/// from: the runner process's own, reduced to [`docker_cli_inherits_variable`].
/// No job variable is ever added to it.
pub fn docker_cli_environment() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    std::env::vars_os()
        .filter(|(name, _)| name.to_str().is_some_and(docker_cli_inherits_variable))
        .collect()
}

/// The environment the host `docker run` client starts from, before the job's
/// variables join it: [`docker_cli_environment`] without the credential
/// helpers' variables, so the operator's cloud credentials are not in a
/// process that also holds the job's.
pub fn docker_run_environment() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    std::env::vars_os()
        .filter(|(name, _)| name.to_str().is_some_and(docker_run_inherits_variable))
        .collect()
}

/// Is Docker Content Trust on for the operator's client?
///
/// The CLI reads `DOCKER_CONTENT_TRUST` with Go's `strconv.ParseBool`, so these
/// are the spellings that turn it on.
pub fn content_trust_enabled() -> bool {
    std::env::var("DOCKER_CONTENT_TRUST")
        .is_ok_and(|value| matches!(value.as_str(), "1" | "t" | "T" | "true" | "TRUE" | "True"))
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
            "XDG_RUNTIME_DIR",
            "SystemRoot",
        ] {
            assert!(is_host_sensitive_variable(name), "{name}");
            assert!(!reaches_container(name), "{name}");
        }
    }

    /// card_031aaa2348fe: these used to be refused because the client that
    /// starts the container could also pull, and a pull hands its environment
    /// to a credential helper. That client no longer pulls, so a deploy job's
    /// `AWS_PROFILE` is the job's own business.
    #[test]
    fn what_only_a_credential_helper_reads_reaches_the_container() {
        for name in [
            "AWS_ACCESS_KEY_ID",
            "AWS_SECRET_ACCESS_KEY",
            "AWS_SESSION_TOKEN",
            "AWS_PROFILE",
            "AWS_CONFIG_FILE",
            "AWS_SHARED_CREDENTIALS_FILE",
            "AWS_WEB_IDENTITY_TOKEN_FILE",
            "AWS_ROLE_ARN",
            "AWS_ENDPOINT_URL",
            "aws_default_region",
            "GOOGLE_APPLICATION_CREDENTIALS",
            "GOOGLE_EXTERNAL_ACCOUNT_ALLOW_EXECUTABLES",
            "CLOUDSDK_CONFIG",
            "DBUS_SESSION_BUS_ADDRESS",
            "PASSWORD_STORE_DIR",
            "GNUPGHOME",
        ] {
            assert!(credential_helper_variable(name), "{name}");
            assert!(reaches_container(name), "{name}");
            assert!(!docker_run_inherits_variable(name), "{name}");
        }
    }

    #[test]
    fn ordinary_job_variables_reach_the_container() {
        for name in [
            "DEPLOY_TARGET",
            "CI",
            "CI_JOB_TOKEN",
            "MATRIX_NODE_VERSION",
            "AWS_PROFILE",
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

    /// The two lists are a pair: a name the `docker run` client takes from the
    /// operator must not be one a job can also set, or the job's copy is the
    /// one it reads. The client that pulls inherits the helpers' names too, and
    /// never carries a job variable at all.
    #[test]
    fn whatever_the_run_client_inherits_a_job_cannot_set() {
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
            "XDG_RUNTIME_DIR",
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
            assert!(docker_run_inherits_variable(name), "{name}");
            assert!(
                is_host_sensitive_variable(name),
                "{name} is inherited by the run client yet a job may set it"
            );
        }
        for name in [
            "AWS_PROFILE",
            "AWS_ACCESS_KEY_ID",
            "CLOUDSDK_CONFIG",
            "GNUPGHOME",
        ] {
            assert!(docker_cli_inherits_variable(name), "{name}");
            assert!(!docker_run_inherits_variable(name), "{name}");
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
        for (name, _) in docker_run_environment() {
            assert!(
                name.to_str().is_some_and(docker_run_inherits_variable),
                "{name:?} reached the docker run environment"
            );
        }
        if let Some(path) = std::env::var_os("PATH") {
            assert!(inherited
                .iter()
                .any(|(name, value)| name == "PATH" && *value == path));
        }
    }
}
