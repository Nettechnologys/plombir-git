//! `plombir-git trial` — a throwaway instance with nothing to prepare.
//!
//! The production path asks the operator for a JWT secret, a config file
//! written under `umask 077`, an SSH host key and a first-account procedure.
//! Each is there for a reason, and none of it is something a person who only
//! wants to see the forge should have to do. `trial` does all of it in one
//! directory and then starts the ordinary `serve` on it:
//!
//! - the data directory is created `0700`;
//! - `plombir-git.toml` is written there once, `0600` from its first byte, with
//!   a generated `jwt_secret`, paths into the same directory, a SQLite database
//!   and `registration = "closed"` — and reused on every later start, so the
//!   sessions and the encryption key of the first start stay valid;
//! - `serve` then creates the host key, the encryption key and the one-time
//!   setup token beside them, exactly as on a production first start, and the
//!   setup link is printed as a banner (see [`TrialMode::announce_setup`]).
//!
//! Nothing here is a default of `serve`: an instance is in trial mode only
//! because someone typed `trial`, and a missing config never turns into one.
//! The flag travels to the web UI as `GET /api/v1/instance` → `trial_mode`,
//! which puts a banner on every page.

use std::io::Write;
use std::path::Path;

use anyhow::Context;

/// What `serve` needs to know about a trial start beyond its settings.
#[derive(Clone, Debug)]
pub(crate) struct TrialMode {
    /// Where a browser reaches the instance, without a trailing slash. Only
    /// used to spell out the setup link.
    pub(crate) public_url: String,
}

impl TrialMode {
    /// The link that opens the register page with the setup token filled in.
    pub(crate) fn setup_link(&self, setup_token: &str) -> String {
        format!("{}/register?setup_token={setup_token}", self.public_url)
    }

    /// The banner a person scanning `docker run`'s output has to find: the
    /// link to create the administrator, and what kind of instance this is.
    pub(crate) fn announce_setup(&self, setup_token: &str) {
        let link = self.setup_link(setup_token);
        // `warn!` so it survives `RUST_LOG=warn`, as the setup-token line does.
        tracing::warn!(
            "\n\n\
             ================================================================\n  \
             Plombir Git TRIAL MODE: not for production.\n  \
             The data lives in the trial directory and goes with it.\n\n  \
             Create the administrator account here:\n\n    \
             {link}\n\n\
             ================================================================\n"
        );
    }
}

/// The file `trial` writes into its data directory, and `serve` reads.
const TRIAL_CONFIG_FILE: &str = "plombir-git.toml";

/// Prepare the data directory and return the config file `serve` is to read.
///
/// Called before logging is set up, so it reports through its error only.
pub(crate) fn prepare_data_dir(data_dir: &Path) -> anyhow::Result<String> {
    rg_core::platform::fs::create_dir_all_owner_only(data_dir).map_err(|error| {
        anyhow::anyhow!(rg_core::platform::fs::describe_path_error(
            "trial data directory",
            data_dir,
            &error,
            "pass --data-dir a directory this user can create or write to",
        ))
    })?;
    // Absolute, so the paths in the config do not depend on where `serve` is
    // started from the next time.
    let data_dir = data_dir
        .canonicalize()
        .with_context(|| format!("failed to resolve {}", data_dir.display()))?;
    let config_path = data_dir.join(TRIAL_CONFIG_FILE);
    let config_text = config_path
        .to_str()
        .with_context(|| format!("{} is not valid UTF-8", config_path.display()))?
        .to_owned();
    if !config_path.exists() {
        write_config(
            &config_path,
            &trial_config(&data_dir, &crate::admin::generate_jwt_secret())?,
        )?;
    }
    Ok(config_text)
}

/// The config of a trial instance: everything inside `data_dir`.
fn trial_config(data_dir: &Path, jwt_secret: &str) -> anyhow::Result<String> {
    let path = |name: &str| -> anyhow::Result<String> {
        let joined = data_dir.join(name);
        let text = joined
            .to_str()
            .with_context(|| format!("{} is not valid UTF-8", joined.display()))?;
        Ok(text.to_owned())
    };
    // `toml::Value`'s `Display` quotes and escapes, so a data directory with a
    // quote or a backslash in its name still makes a file that parses.
    let quoted = |text: String| toml::Value::String(text).to_string();
    Ok(format!(
        "# Written by `plombir-git trial` on its first start. TRIAL MODE: not for\n\
         # production. `plombir-git trial` reuses this file; delete the whole\n\
         # directory to start over.\n\
         \n\
         [server]\n\
         repo_root = {repo_root}\n\
         host_key = {host_key}\n\
         \n\
         [database]\n\
         url = {db_url}\n\
         \n\
         [auth]\n\
         jwt_secret = {jwt_secret}\n\
         registration = \"closed\"\n",
        repo_root = quoted(path("repos")?),
        host_key = quoted(path("ssh_host_key")?),
        db_url = quoted(format!("sqlite://{}?mode=rwc", path("plombir-git.db")?)),
        jwt_secret = quoted(jwt_secret.to_owned()),
    ))
}

fn write_config(path: &Path, contents: &str) -> anyhow::Result<()> {
    let mut file = rg_core::platform::fs::create_new_owner_only(path).map_err(|error| {
        anyhow::anyhow!(rg_core::platform::fs::describe_path_error(
            "trial config file",
            path,
            &error,
            "the trial writes its config into --data-dir and needs write access to it",
        ))
    })?;
    file.write_all(contents.as_bytes())
        .and_then(|()| file.sync_all())
        .with_context(|| format!("failed to write {}", path.display()))
}

/// The public URL as given, without a trailing slash; refused when it is not
/// an `http(s)://` address, since the link built from it would not open.
pub(crate) fn normalize_public_url(raw: &str) -> anyhow::Result<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    let host = trimmed
        .strip_prefix("http://")
        .or_else(|| trimmed.strip_prefix("https://"));
    anyhow::ensure!(
        host.is_some_and(|host| !host.is_empty() && !host.contains(char::is_whitespace)),
        "--public-url {raw:?} must be an http:// or https:// address with a host"
    );
    Ok(trimmed.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_config_is_written_once_owner_only_and_reused() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("trial");
        let config = prepare_data_dir(&data).unwrap();
        let first = std::fs::read_to_string(&config).unwrap();
        let config = Path::new(&config).to_path_buf();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&data), 0o700, "the data directory holds every secret");
            assert_eq!(mode(&config), 0o600, "the config holds the JWT secret");
        }

        // A second start must not mint a new secret: it would sign every
        // session of the first one out.
        assert_eq!(Path::new(&prepare_data_dir(&data).unwrap()), config);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), first);

        // And `serve` has to accept what was written, including its owner-only
        // check and the strict schema.
        let parsed = crate::config::load_config_file(config.to_str().unwrap()).unwrap();
        let secret = parsed.auth.jwt_secret.expect("a generated JWT secret");
        crate::admin::validate_signing_secret(&secret, "trial config").unwrap();
        let canonical = data.canonicalize().unwrap();
        assert_eq!(
            parsed.server.repo_root.as_deref(),
            canonical.join("repos").to_str()
        );
    }

    #[test]
    fn a_data_directory_with_quotes_still_makes_a_config_that_parses() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("a \"quoted\" \\ dir");
        let config = prepare_data_dir(&data).unwrap();
        let parsed = crate::config::load_config_file(&config).unwrap();
        assert!(parsed
            .server
            .host_key
            .as_deref()
            .unwrap()
            .ends_with("a \"quoted\" \\ dir/ssh_host_key"));
    }

    #[test]
    fn the_setup_link_opens_the_register_page_with_the_token() {
        let trial = TrialMode {
            public_url: normalize_public_url("http://localhost:8080/").unwrap(),
        };
        assert_eq!(
            trial.setup_link("abc"),
            "http://localhost:8080/register?setup_token=abc"
        );
        assert!(normalize_public_url("localhost:8080").is_err());
        assert!(normalize_public_url("ftp://localhost").is_err());
    }
}
