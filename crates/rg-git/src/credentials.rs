//! Handing a secret to an outbound `git` subprocess without leaking it.
//!
//! Two callers speak to remotes the user supplied — mirror sync and repository
//! import — and both start from the same kind of secret: a password or a
//! platform token typed into a form. This module is the only sanctioned way in,
//! so the two cannot drift apart on the question of where the secret ends up.
//!
//! The rule it encodes: the secret travels through the **environment**, read
//! back by an inline credential helper. Never through argv, never through the
//! remote URL.

/// Environment variables the credential helper reads the secret out of. Named
/// after the product so they cannot collide with something the operator has
/// already exported for their own git usage.
pub const USERNAME_ENV: &str = "FORGEKEEP_GIT_USERNAME";
pub const PASSWORD_ENV: &str = "FORGEKEEP_GIT_PASSWORD";

/// A remote's credential, in plaintext, for the duration of a single git
/// invocation.
///
/// Deliberately has no `Debug` and no field access: a password that cannot be
/// printed by a stray `{:?}` and cannot be pasted into a URL by the next caller
/// is one fewer way to leak it.
pub struct GitCredentials {
    username: Option<String>,
    password: String,
}

impl GitCredentials {
    /// A username/password pair as an operator typed it.
    ///
    /// An empty username is the same as none: git handed half an answer is
    /// refused by the remote, and with prompting disabled the failure reads as
    /// a protocol error rather than the honest "no credential" it is.
    pub fn new(username: Option<String>, password: String) -> Self {
        Self {
            username: username.filter(|username| !username.is_empty()),
            password,
        }
    }

    /// A platform token, which travels in the password field of HTTP Basic.
    ///
    /// The username is the fixed placeholder the platform documents
    /// (`x-access-token` for GitHub, `oauth2` for GitLab); it carries no
    /// information, but Basic auth has nowhere to put a lone token.
    pub fn token(username: &str, token: &str) -> Self {
        Self::new(Some(username.to_string()), token.to_string())
    }

    /// The secret, for masking it out of text that is about to be logged or
    /// persisted.
    pub fn password(&self) -> &str {
        &self.password
    }
}

/// Extra `git` arguments and environment that let the subprocess authenticate.
///
/// The secret is passed through the environment and read by an inline
/// credential helper, so:
///
/// - it never appears in argv (`ps` on a shared host would show it, and the
///   gateway echoes the command line into its error text and trace span);
/// - it never appears in the remote URL, which git copies verbatim into
///   `.git/config` — an on-disk plaintext copy that outlives the operation.
///
/// The empty `credential.helper=` in front resets the helper list, so a helper
/// configured system- or user-wide on the host can neither answer first nor be
/// handed this credential to store.
///
/// `GIT_TERMINAL_PROMPT=0` is set whether or not a credential exists: nothing
/// here runs with a terminal behind it, so a remote that asks for
/// authentication must fail fast instead of blocking until the gateway's
/// timeout.
pub fn credential_invocation(
    credentials: Option<&GitCredentials>,
) -> (Vec<String>, Vec<(String, String)>) {
    let mut env = vec![("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())];
    let Some(credentials) = credentials else {
        return (Vec::new(), env);
    };

    // git runs a `!`-prefixed helper through `sh -c '<value> "$@"' <value> get`,
    // so the trailing `f` becomes the call and takes the operation as `$1`.
    let mut helper = String::from("!f() { ");
    if credentials.username.is_some() {
        helper.push_str(&format!("echo username=\"${USERNAME_ENV}\"; "));
    }
    helper.push_str(&format!("echo password=\"${PASSWORD_ENV}\"; }}; f"));

    let args = vec![
        "-c".to_string(),
        "credential.helper=".to_string(),
        "-c".to_string(),
        format!("credential.helper={helper}"),
    ];
    if let Some(username) = &credentials.username {
        env.push((USERNAME_ENV.to_string(), username.clone()));
    }
    env.push((PASSWORD_ENV.to_string(), credentials.password.clone()));
    (args, env)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the environment hand-off: `ps` (and the gateway's own
    /// error text, which quotes the command line) must never see the password.
    #[test]
    fn the_password_never_reaches_the_command_line() {
        let credentials = GitCredentials::token("sync-bot", "hunter2");
        let (args, env) = credential_invocation(Some(&credentials));

        let command_line = args.join(" ");
        assert!(
            !command_line.contains("hunter2"),
            "the password leaked into argv: {command_line}"
        );
        assert!(
            !command_line.contains("sync-bot"),
            "the username leaked into argv: {command_line}"
        );
        // The helper list is reset first, then our helper is installed.
        assert_eq!(args[0], "-c");
        assert_eq!(args[1], "credential.helper=");
        assert!(args[3].contains(USERNAME_ENV) && args[3].contains(PASSWORD_ENV));

        let env: std::collections::HashMap<_, _> = env.into_iter().collect();
        assert_eq!(env.get(PASSWORD_ENV).map(String::as_str), Some("hunter2"));
        assert_eq!(env.get(USERNAME_ENV).map(String::as_str), Some("sync-bot"));
        assert_eq!(
            env.get("GIT_TERMINAL_PROMPT").map(String::as_str),
            Some("0")
        );
    }

    /// With no credential there is nothing to hand over — but prompting still
    /// has to be off, or an authenticating remote hangs the caller until the
    /// gateway's timeout instead of failing with "authentication required".
    #[test]
    fn an_anonymous_remote_installs_no_helper_but_still_cannot_prompt() {
        let (args, env) = credential_invocation(None);
        assert!(
            args.is_empty(),
            "an anonymous invocation configured a credential helper"
        );
        assert_eq!(
            env,
            vec![("GIT_TERMINAL_PROMPT".to_string(), "0".to_string())]
        );
    }

    /// A username the caller left empty is not a username: the helper must
    /// answer with the password alone rather than echo an empty one, which the
    /// remote would take as a real (and wrong) identity.
    #[test]
    fn an_empty_username_is_no_username() {
        let credentials = GitCredentials::new(Some(String::new()), "hunter2".to_string());
        let (args, env) = credential_invocation(Some(&credentials));

        assert!(!args[3].contains(USERNAME_ENV), "{}", args[3]);
        let env: std::collections::HashMap<_, _> = env.into_iter().collect();
        assert!(!env.contains_key(USERNAME_ENV));
        assert_eq!(env.get(PASSWORD_ENV).map(String::as_str), Some("hunter2"));
    }
}
