//! Command-line interface definitions for the runner agent.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "forgekeep-runner", about = "ForgeKeep CI Runner Agent")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Register a new runner and get a token
    ///
    /// Settings that also exist as a `--config` key resolve in the order
    /// CLI arg > config file > built-in default, exactly as in `run`.
    // The clap `default_value` on `--server` is gone for the same reason it is
    // gone from `run`: it made "the operator typed localhost" indistinguishable
    // from "the flag was not passed", so `runner.toml`'s `server` could never
    // win — and with `--save` that localhost then overwrote the operator's own
    // server in the file.
    Register {
        /// ForgeKeep server URL [config: server]
        /// [default: http://127.0.0.1:8080]
        #[arg(long)]
        server: Option<String>,

        /// Allow the admin JWT on a non-loopback plaintext HTTP server
        /// [config: allow_insecure_http]
        #[arg(long)]
        allow_insecure_http: bool,

        /// Runner name [config: name] [default: system hostname]
        #[arg(long)]
        name: Option<String>,

        /// Runner labels (comma-separated, e.g. "docker,linux,amd64") [config: labels]
        #[arg(long)]
        labels: Option<String>,

        /// Save token to config file
        #[arg(long)]
        save: bool,

        /// Admin user JWT used only for runner registration
        #[arg(long)]
        auth_token: Option<String>,

        /// Config file `--save` writes to — must be the same path `run` reads
        #[arg(long, default_value = "~/.forgekeep/runner.toml")]
        config: String,
    },

    /// Start the runner (register if needed, then poll and execute jobs)
    ///
    /// Settings that also exist as a `--config` key resolve in the order
    /// CLI arg > config file > built-in default.
    // Hence `--server` is an `Option` with no clap `default_value`: a clap
    // default is indistinguishable from a value the operator typed, so with one
    // the config file's `server` could never win over "the flag was not passed".
    // The default lives in `config::DEFAULT_SERVER` and is named in the help.
    Run {
        /// ForgeKeep server URL [config: server]
        /// [default: http://127.0.0.1:8080]
        #[arg(long)]
        server: Option<String>,

        /// Allow admin/runner tokens on a non-loopback plaintext HTTP server
        /// [config: allow_insecure_http]
        #[arg(long)]
        allow_insecure_http: bool,

        /// Runner name [config: name] [default: system hostname]
        #[arg(long)]
        name: Option<String>,

        /// Runner labels (comma-separated) [config: labels]
        #[arg(long)]
        labels: Option<String>,

        /// Existing runner token (skip registration) [config: token]
        #[arg(long)]
        token: Option<String>,

        /// Existing runner ID (used with --token) [config: runner_id]
        #[arg(long)]
        runner_id: Option<i64>,

        /// Admin user JWT used only when this command needs to register a runner
        #[arg(long)]
        auth_token: Option<String>,

        /// Path to config file
        #[arg(long, default_value = "~/.forgekeep/runner.toml")]
        config: String,
    },
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    use clap::{CommandFactory, Parser};

    use super::{Cli, Commands};

    /// A clap `default_value` on `--server` is indistinguishable from a typed
    /// value, which is exactly how `register` came to ignore `runner.toml` and
    /// then overwrite it with localhost on `--save`. Both subcommands must leave
    /// "flag not passed" observable as `None`.
    #[test]
    fn an_omitted_server_flag_stays_none_for_both_subcommands() {
        let register = Cli::try_parse_from(["forgekeep-runner", "register", "--name", "builder-1"])
            .expect("register parses without --server");
        let Commands::Register { server, name, .. } = register.command else {
            panic!("expected the register subcommand");
        };
        assert_eq!(server, None);
        assert_eq!(name.as_deref(), Some("builder-1"));

        let run = Cli::try_parse_from(["forgekeep-runner", "run"]).expect("run parses bare");
        let Commands::Run { server, .. } = run.command else {
            panic!("expected the run subcommand");
        };
        assert_eq!(server, None);
    }

    // ---------------------------------------------------------------------
    // The `forgekeep-runner run` command the shipped deployment files invite an
    // operator to uncomment.
    //
    // Nothing has ever checked it. The block is a YAML *comment*, so
    // `docker compose config` — the gate that validates those files — never
    // parses it, and the flags it spells out are declared here while the files
    // live beside the server's. Renaming one turns a block an operator
    // uncomments into `error: unexpected argument`, found on a build machine
    // rather than by a compiler.
    //
    // The other half of this binary's operator-facing surface — the
    // `runner.toml` key each flag's `--help` names as its equivalent — is
    // checked in `config.rs`, where `RunnerConfig` is: this file is compiled
    // into the *binary* target, so only the text of it is reachable from the
    // library where the model lives.
    // ---------------------------------------------------------------------

    /// The command the deployment files spell out for this binary.
    const RUN_INVOCATION: &str = "forgekeep-runner run";

    /// The files an operator copies a command out of: the shipped compose
    /// files, the image's own default command, and the two guides that quote
    /// them. The root `README.md` is here because it is the only page that
    /// spells out `register` at all.
    ///
    /// Walked at run time rather than pinned with `include_str!` so that a
    /// compose file added to `deploy/` joins the contract by existing. The
    /// floors in each test are what keep a walk that stopped matching from
    /// passing for agreement.
    fn deployment_files() -> Vec<(String, String)> {
        fn read(path: &Path) -> String {
            std::fs::read_to_string(path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        }

        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("the repository root must be reachable from the crate directory");

        let deploy = root.join("deploy");
        let mut files = Vec::new();

        for entry in std::fs::read_dir(&deploy)
            .unwrap_or_else(|error| panic!("{}: {error}", deploy.display()))
        {
            let path = entry.expect("a readable directory entry").path();
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if name.starts_with("docker-compose") && name.ends_with(".yml") {
                files.push((format!("deploy/{name}"), read(&path)));
            }
        }

        // Directory order is not stable across machines; failures should not be
        // either the first or the last one depending on the filesystem.
        files.sort_by(|(left, _), (right, _)| left.cmp(right));

        for extra in ["Dockerfile", "deploy/README.md", "README.md"] {
            files.push((extra.to_string(), read(&root.join(extra))));
        }

        files
    }

    /// A deployment file reduced to one command line per line: the `#` of a
    /// commented-out block dropped, the punctuation that only holds a command
    /// together turned into whitespace, and runs of whitespace collapsed.
    ///
    /// Three spellings have to survive it — a folded `command: >` block with one
    /// flag per line, an exec-form `command: ["forgekeep-runner", "run", …]` on
    /// a single line, and the Dockerfile's backslash-continued `CMD` — plus the
    /// backticks a markdown guide wraps the same command in. Most of them sit
    /// behind a `#`, which is exactly why nothing checks them today:
    /// `docker compose config` validates the file and never sees a comment.
    fn command_lines(text: &str) -> Vec<String> {
        text.lines()
            .map(|line| {
                let line = line.trim();
                let line = line.strip_prefix('#').unwrap_or(line);
                let line = line.replace(['[', ']', '"', ',', '\\', '`'], " ");
                line.split_whitespace().collect::<Vec<_>>().join(" ")
            })
            .collect()
    }

    /// Every invocation of `leader` in `lines`, as the command text following it.
    ///
    /// An invocation is the rest of the line the leader starts on plus every
    /// line after it that starts with a flag — the one rule that collapses all
    /// three spellings above, since only a folded block puts its flags on lines
    /// of their own, and the first line that is not a flag is the next YAML key.
    fn invocations(lines: &[String], leader: &str) -> Vec<String> {
        let mut found = Vec::new();

        for (index, line) in lines.iter().enumerate() {
            let Some((before, rest)) = line.split_once(leader) else {
                continue;
            };

            // A whole word on both sides: neither a longer binary name nor a
            // longer subcommand is this invocation. A path separator ends the
            // word too — an install guide that spells the binary out in full is
            // running the same command.
            if before
                .chars()
                .next_back()
                .is_some_and(|c| !c.is_whitespace() && c != '/')
                || rest.starts_with(|c: char| !c.is_whitespace())
            {
                continue;
            }

            let mut parts = Vec::new();
            let rest = rest.trim();
            if !rest.is_empty() {
                parts.push(rest);
            }
            for next in &lines[index + 1..] {
                if !next.starts_with("--") {
                    break;
                }
                parts.push(next);
            }

            found.push(parts.join(" "));
        }

        found
    }

    /// The `--long-flag` words of a command.
    ///
    /// The counterpart of `rg-cli`'s `long_flags`, which has the harder job of
    /// picking flags out of markdown prose; here the input is already a command,
    /// so a word either is a flag or is not. The two crates keep their own `Cli`
    /// private and share no dependency, hence the small duplication — the same
    /// reason `config.rs` re-states `rg-cli`'s config-file reader.
    fn long_flags(command: &str) -> BTreeSet<&str> {
        command
            .split_whitespace()
            .filter(|word| {
                word.starts_with("--") && word[2..].starts_with(|c: char| c.is_ascii_alphanumeric())
            })
            .map(|word| word.split_once('=').map_or(word, |(flag, _)| flag))
            .collect()
    }

    /// Every subcommand of this binary, keyed by the invocation an operator
    /// types, paired with the long flags clap accepts for it.
    fn subcommand_flags() -> BTreeMap<String, BTreeSet<String>> {
        let command = Cli::command();
        let binary = command.get_name().to_string();

        command
            .get_subcommands()
            .map(|sub| {
                let mut accepted: BTreeSet<String> = sub
                    .get_arguments()
                    .filter_map(|arg| arg.get_long().map(|long| format!("--{long}")))
                    .collect();
                // clap generates `--help` in a build step this walk does not
                // run, so the declaration it reads never carries it.
                accepted.insert("--help".to_string());
                (format!("{binary} {}", sub.get_name()), accepted)
            })
            .collect()
    }

    /// Both shipped compose files carry a ready-to-uncomment `runner` service,
    /// and the README carries the registration command an operator runs on the
    /// build machine. Nothing checks either: the compose block is a comment, so
    /// `docker compose config` skips it, and the flags are declared in this
    /// crate while every page that spells them out lives elsewhere.
    ///
    /// `register` is scanned alongside `run` because it is the command that is
    /// typed once, by hand, with an admin JWT in the environment — a renamed
    /// `--labels` there is discovered by a person mid-install, and `--save`
    /// getting it wrong leaves a second runner registered.
    ///
    /// Only this direction is checked: a flag no page mentions is the intent,
    /// not drift.
    #[test]
    fn every_runner_flag_the_operator_pages_offer_exists() {
        // Every shape the shipped files use, each followed by the line that ends
        // it. A scanner that swallowed the next key, or stopped matching a
        // shape, is how this test would go quietly green.
        let fixture = command_lines(concat!(
            "  # command: >\n",
            "  #   forgekeep-runner run\n",
            "  #   --server http://forgekeep:8080\n",
            "  #   --token ${FORGEKEEP_RUNNER_TOKEN}\n",
            "  # environment:\n",
            "  #   - FORGEKEEP_RUNNER_ID=1\n",
            "    command: [\"forgekeep-runner\", \"run\", \"--config\", \"/app/runner.toml\"]\n",
            "    networks:\n",
            "      - forgekeep-net\n",
        ));
        assert_eq!(
            invocations(&fixture, RUN_INVOCATION),
            vec![
                "--server http://forgekeep:8080 --token ${FORGEKEEP_RUNNER_TOKEN}".to_string(),
                "--config /app/runner.toml".to_string(),
            ],
            "the invocation scanner no longer reads the deployment files the way they spell \
             the command"
        );
        assert!(
            invocations(
                &command_lines("forgekeep-runner run-forever --nope\n"),
                RUN_INVOCATION
            )
            .is_empty(),
            "the invocation scanner reads a longer subcommand as `{RUN_INVOCATION}`"
        );

        let by_subcommand = subcommand_flags();
        let mut documented = BTreeSet::new();
        let mut offered = 0;
        let mut checked = 0;

        for (name, text) in deployment_files() {
            let lines = command_lines(&text);
            for (invocation, accepted) in &by_subcommand {
                for command in invocations(&lines, invocation) {
                    offered += 1;
                    for flag in long_flags(&command) {
                        assert!(
                            accepted.contains(flag),
                            "{name} offers `{invocation} {flag}`, which clap does not accept — \
                             an operator who pastes that line, or uncomments that block, gets \
                             `error: unexpected argument`. Rename it on the page too, or \
                             restore the flag."
                        );
                        checked += 1;
                        documented.insert(invocation.clone());
                    }
                }
            }
        }

        // Floors, not counts: both shipped compose files carry the `run` block
        // with three flags each, and the README's `register` example names five.
        assert!(
            offered >= 3,
            "only {offered} subcommand invocations found across the operator pages — \
             the scanner has stopped matching them"
        );
        assert!(
            checked >= 11,
            "only {checked} flags found across those invocations — the flag scanner has \
             stopped matching them"
        );
        assert_eq!(
            documented,
            by_subcommand.keys().cloned().collect::<BTreeSet<_>>(),
            "a subcommand of this binary is documented with no flag anywhere — an operator \
             meets it for the first time by running it"
        );
    }
}
