//! Instance-owned trust material for commit signatures.
//!
//! Git's `%G?` is only meaningful with an explicitly supplied keyring. A
//! process home or a repository's `gpg.*` configuration must never decide
//! which contributor is trusted by this instance.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use anyhow::{bail, Context, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SigningKeyKind {
    Gpg,
    Ssh,
}

#[derive(Clone, Debug)]
pub struct RegisteredSigningKey {
    pub kind: SigningKeyKind,
    pub public_key: String,
    pub fingerprint: String,
    /// The account's verified email, bound to the key at registration time.
    pub email: String,
}

/// Return the canonical public half and primary fingerprint of one OpenPGP key.
/// Private key material is never returned to the caller or stored in the DB.
pub fn validate_gpg_public_key(input: &str, expected_email: &str) -> Result<(String, String)> {
    if input.len() > 65_536 || !input.contains("-----BEGIN PGP PUBLIC KEY BLOCK-----") {
        bail!("expected one armored GPG public key (at most 64 KiB)");
    }
    let home = tempfile::tempdir().context("create temporary GPG home")?;
    let source = home.path().join("submitted.asc");
    std::fs::write(&source, input).context("write temporary GPG key")?;
    run_gpg(
        home.path(),
        &[
            "--import",
            source.to_str().context("GPG path is not UTF-8")?,
        ],
    )
    .context("invalid GPG public key")?;
    let list = run_gpg(
        home.path(),
        &["--with-colons", "--fingerprint", "--list-keys"],
    )?;
    let text = String::from_utf8(list.stdout).context("GPG key listing is not UTF-8")?;
    let mut primary = None;
    let mut public_count = 0;
    let mut awaiting_fingerprint = false;
    let mut email_present = false;
    for line in text.lines() {
        let fields: Vec<_> = line.split(':').collect();
        match fields.first().copied() {
            Some("pub") => {
                public_count += 1;
                awaiting_fingerprint = true;
            }
            Some("fpr") if awaiting_fingerprint => {
                primary = fields.get(9).map(|value| (*value).to_string());
                awaiting_fingerprint = false;
            }
            Some("uid") => {
                email_present |= fields.get(9).is_some_and(|uid| {
                    uid.to_ascii_lowercase()
                        .contains(&format!("<{}>", expected_email.to_ascii_lowercase()))
                });
            }
            _ => {}
        }
    }
    if public_count != 1 {
        bail!("expected exactly one GPG public key");
    }
    if !email_present {
        bail!("GPG key must contain the verified account email as a user ID");
    }
    let fingerprint = primary.context("GPG public key has no fingerprint")?;
    let exported = run_gpg(home.path(), &["--armor", "--export", &fingerprint])?;
    let public_key = String::from_utf8(exported.stdout).context("GPG export is not UTF-8")?;
    if public_key.is_empty() {
        bail!("GPG public key export was empty");
    }
    Ok((public_key, fingerprint))
}

fn run_gpg(home: &Path, args: &[&str]) -> Result<Output> {
    let mut command = Command::new("gpg");
    command
        .env_clear()
        .arg("--batch")
        .arg("--no-tty")
        .arg("--no-options")
        .arg("--homedir")
        .arg(home)
        .args(args);
    let output = match rg_process::output_in_process_tree_with_timeout_and_limit(
        &mut command,
        Duration::from_secs(10),
        256 * 1024,
        64 * 1024,
    )
    .context("GPG verifier is unavailable")?
    {
        rg_process::TimedOutput::Completed(output) => output,
        rg_process::TimedOutput::TimedOut => bail!("GPG verifier timed out"),
        rg_process::TimedOutput::OutputTooLarge { .. } => {
            bail!("GPG verifier output was too large")
        }
    };
    if !output.status.success() {
        bail!("GPG rejected the public key or trust material");
    }
    Ok(output)
}

/// One request's isolated keyring. Kept alive across all commits in a push.
pub struct SignatureVerifier {
    _home: tempfile::TempDir,
    gpg_home: PathBuf,
    allowed_signers: PathBuf,
    keys: Vec<RegisteredSigningKey>,
}

impl SignatureVerifier {
    pub fn new(keys: &[RegisteredSigningKey]) -> Result<Self> {
        let home = tempfile::tempdir().context("create signature verifier home")?;
        let gpg_home = home.path().join("gnupg");
        std::fs::create_dir(&gpg_home).context("create isolated GPG keyring")?;
        run_gpg(&gpg_home, &["--version"])?;
        let allowed_signers = home.path().join("allowed-signers");
        let mut ssh_lines = String::new();
        let mut trust = String::new();
        for (index, key) in keys.iter().enumerate() {
            if !safe_signing_email(&key.email) {
                bail!("registered signing key has an unsafe email principal");
            }
            match key.kind {
                SigningKeyKind::Gpg => {
                    let source = home.path().join(format!("gpg-{index}.asc"));
                    std::fs::write(&source, &key.public_key)?;
                    run_gpg(
                        &gpg_home,
                        &[
                            "--import",
                            source.to_str().context("GPG path is not UTF-8")?,
                        ],
                    )?;
                    if !key.fingerprint.chars().all(|c| c.is_ascii_hexdigit()) {
                        bail!("registered GPG fingerprint is malformed");
                    }
                    trust.push_str(&format!("{}:6:\n", key.fingerprint));
                }
                SigningKeyKind::Ssh => {
                    let mut fields = key.public_key.split_whitespace();
                    let algorithm = fields.next().context("registered SSH key has no type")?;
                    let blob = fields.next().context("registered SSH key has no body")?;
                    if !algorithm.starts_with("ssh-")
                        && !algorithm.starts_with("ecdsa-")
                        && !algorithm.starts_with("sk-")
                        || !blob
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || "+/=".contains(c))
                    {
                        bail!("registered SSH public key is malformed");
                    }
                    ssh_lines.push_str(&format!("{} {} {}\n", key.email, algorithm, blob));
                }
            }
        }
        if !trust.is_empty() {
            let trust_file = home.path().join("ownertrust");
            std::fs::write(&trust_file, trust)?;
            run_gpg(
                &gpg_home,
                &[
                    "--import-ownertrust",
                    trust_file.to_str().context("GPG path is not UTF-8")?,
                ],
            )?;
        }
        std::fs::write(&allowed_signers, ssh_lines)?;
        Ok(Self {
            _home: home,
            gpg_home,
            allowed_signers,
            keys: keys.to_vec(),
        })
    }

    /// Git status, key fingerprint, signer name/email and committer email.
    pub fn check_commit(
        &self,
        repo_path: &Path,
        sha: &str,
        env: &[(&str, &str)],
    ) -> Result<SignatureCheck> {
        let signers = format!(
            "gpg.ssh.allowedSignersFile={}",
            self.allowed_signers.display()
        );
        let gpg_home = self.gpg_home.to_str().context("GPG home is not UTF-8")?;
        let mut git_env = env.to_vec();
        git_env.push(("GNUPGHOME", gpg_home));
        let gateway = crate::cli_gateway::global_gateway()
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let output = gateway.run_with_env(
            &[
                "-c",
                "gpg.program=gpg",
                "-c",
                "gpg.openpgp.program=gpg",
                "-c",
                "gpg.ssh.program=ssh-keygen",
                "-c",
                &signers,
                "-c",
                "gpg.ssh.revocationFile=",
                "-c",
                "gpg.minTrustLevel=fully",
                "log",
                "--format=%G?%x00%GF%x00%GP%x00%GS%x00%ce",
                "-1",
                sha,
            ],
            Some(repo_path),
            &git_env,
        )?;
        output
            .ensure_success()
            .context("git could not verify commit signature")?;
        let text = output.stdout_str();
        let fields: Vec<&str> = text.trim_end_matches('\n').split('\0').collect();
        let [status, fingerprint, primary_fingerprint, signer_name, committer_email] =
            fields.as_slice()
        else {
            bail!("git returned a malformed commit signature report");
        };
        let status = status.trim().to_string();
        let fingerprint = fingerprint.trim().to_string();
        let primary_fingerprint = primary_fingerprint.trim().to_string();
        let signer_name = signer_name.trim().to_string();
        let committer_email = committer_email.trim().to_string();
        let signer_email = signer_name
            .rsplit_once('<')
            .and_then(|(_, email)| email.strip_suffix('>'))
            .unwrap_or_default()
            .to_string();
        // A cryptographically good key owned by another account does not
        // verify the identity claimed in the commit. It is unknown here.
        let status = if status == "G"
            && !self.keys.iter().any(|key| {
                (key.fingerprint.eq_ignore_ascii_case(&fingerprint)
                    || key.fingerprint.eq_ignore_ascii_case(&primary_fingerprint))
                    && key.email.eq_ignore_ascii_case(&committer_email)
                    && (key.kind == SigningKeyKind::Ssh
                        || key.email.eq_ignore_ascii_case(&signer_email))
            }) {
            "U".to_string()
        } else {
            status
        };
        Ok(SignatureCheck {
            status,
            fingerprint,
            signer_name,
            signer_email,
        })
    }
}

pub fn safe_signing_email(email: &str) -> bool {
    email.len() <= 254
        && email.contains('@')
        && email
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"@._%+-".contains(&byte))
}

#[derive(Debug)]
pub struct SignatureCheck {
    pub status: String,
    pub fingerprint: String,
    pub signer_name: String,
    pub signer_email: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(repo: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
        let output = crate::cli_gateway::global_gateway()
            .as_ref()
            .unwrap()
            .run_with_env(args, Some(repo), env)
            .unwrap();
        assert!(output.success(), "git {args:?}: {}", output.stderr_str());
        output.stdout_str().trim().to_string()
    }

    fn commit(repo: &Path, env: &[(&str, &str)]) -> String {
        git(repo, &["init"], env);
        git(repo, &["config", "user.name", "Alice"], env);
        git(repo, &["config", "user.email", "alice@example.com"], env);
        std::fs::write(repo.join("file.txt"), "signed content\n").unwrap();
        git(repo, &["add", "file.txt"], env);
        git(repo, &["commit", "-m", "signed"], env);
        git(repo, &["rev-parse", "HEAD"], env)
    }

    #[test]
    fn gpg_keyring_is_registered_and_isolated_from_repository_config() {
        let client = tempfile::tempdir().unwrap();
        let client_home = client.path().to_str().unwrap();
        run_gpg(
            client.path(),
            &[
                "--pinentry-mode",
                "loopback",
                "--passphrase",
                "",
                "--quick-generate-key",
                "Alice <alice@example.com>",
                "ed25519",
                "sign",
                "0",
            ],
        )
        .unwrap();
        let armored = run_gpg(client.path(), &["--armor", "--export"])
            .unwrap()
            .stdout;
        let (public_key, fingerprint) =
            validate_gpg_public_key(std::str::from_utf8(&armored).unwrap(), "alice@example.com")
                .unwrap();
        let key = RegisteredSigningKey {
            kind: SigningKeyKind::Gpg,
            public_key,
            fingerprint: fingerprint.clone(),
            email: "alice@example.com".into(),
        };
        let repo = tempfile::tempdir().unwrap();
        let env = [("GNUPGHOME", client_home)];
        git(repo.path(), &["init"], &env);
        git(
            repo.path(),
            &["config", "user.signingkey", &fingerprint],
            &env,
        );
        git(repo.path(), &["config", "commit.gpgsign", "true"], &env);
        let sha = commit(repo.path(), &env);
        // Repository-local configuration must not pick the verifier program.
        git(repo.path(), &["config", "gpg.program", "false"], &env);
        git(
            repo.path(),
            &["config", "gpg.openpgp.program", "false"],
            &env,
        );
        let trusted = SignatureVerifier::new(std::slice::from_ref(&key)).unwrap();
        let checked = trusted.check_commit(repo.path(), &sha, &[]).unwrap();
        assert_eq!(checked.status, "G", "{checked:?}");
        assert_eq!(checked.signer_name, "Alice <alice@example.com>");
        assert_eq!(checked.signer_email, "alice@example.com");
        let required = ["refs/heads/main".to_string()];
        assert_eq!(
            crate::protocol::receive_pack::unsigned_commit_for_required_signature_with_keys(
                repo.path(),
                &"0".repeat(40),
                &sha,
                "refs/heads/main",
                &required,
                std::slice::from_ref(&key),
            )
            .unwrap(),
            None,
        );
        assert_eq!(
            crate::protocol::receive_pack::unsigned_commit_for_required_signature_with_keys(
                repo.path(),
                &"0".repeat(40),
                &sha,
                "refs/heads/main",
                &required,
                &[],
            )
            .unwrap(),
            Some(sha.clone()),
        );
        let unknown = SignatureVerifier::new(&[]).unwrap();
        assert_ne!(
            unknown.check_commit(repo.path(), &sha, &[]).unwrap().status,
            "G"
        );
        let wrong_email = SignatureVerifier::new(&[RegisteredSigningKey {
            email: "bob@example.com".into(),
            ..key
        }])
        .unwrap();
        assert_eq!(
            wrong_email
                .check_commit(repo.path(), &sha, &[])
                .unwrap()
                .status,
            "U"
        );
    }

    #[test]
    fn ssh_allowed_signers_are_registered_and_isolated_from_repository_config() {
        let client = tempfile::tempdir().unwrap();
        let private_key = client.path().join("id_ed25519");
        let generated = Command::new("ssh-keygen")
            .env_clear()
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&private_key)
            .output()
            .unwrap();
        assert!(generated.status.success());
        let public_key = std::fs::read_to_string(private_key.with_extension("pub")).unwrap();
        let fingerprint = String::from_utf8(
            Command::new("ssh-keygen")
                .env_clear()
                .arg("-lf")
                .arg(private_key.with_extension("pub"))
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_string();
        let key = RegisteredSigningKey {
            kind: SigningKeyKind::Ssh,
            public_key,
            fingerprint,
            email: "alice@example.com".into(),
        };
        let repo = tempfile::tempdir().unwrap();
        let private_path = private_key.to_str().unwrap();
        git(repo.path(), &["init"], &[]);
        git(repo.path(), &["config", "gpg.format", "ssh"], &[]);
        git(
            repo.path(),
            &["config", "user.signingkey", private_path],
            &[],
        );
        git(repo.path(), &["config", "commit.gpgsign", "true"], &[]);
        let sha = commit(repo.path(), &[]);
        git(repo.path(), &["config", "gpg.ssh.program", "false"], &[]);
        git(
            repo.path(),
            &["config", "gpg.ssh.allowedSignersFile", "/dev/null"],
            &[],
        );
        let revocations = repo.path().join("revoked-keys");
        std::fs::write(&revocations, &key.public_key).unwrap();
        git(
            repo.path(),
            &[
                "config",
                "gpg.ssh.revocationFile",
                revocations.to_str().unwrap(),
            ],
            &[],
        );
        let trusted = SignatureVerifier::new(&[key]).unwrap();
        assert_eq!(
            trusted.check_commit(repo.path(), &sha, &[]).unwrap().status,
            "G"
        );
        let unknown = SignatureVerifier::new(&[]).unwrap();
        assert_ne!(
            unknown.check_commit(repo.path(), &sha, &[]).unwrap().status,
            "G"
        );
    }

    #[test]
    fn signing_principals_cannot_contain_git_allowed_signers_patterns() {
        assert!(safe_signing_email("alice+git@example.com"));
        assert!(!safe_signing_email("*@example.com"));
        assert!(!safe_signing_email("alice@example.com,other@example.com"));
        assert!(!safe_signing_email("alice@example.com\n*"));
    }
}
