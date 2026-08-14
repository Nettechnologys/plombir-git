//! CI configuration types for `.forgekeep-ci.yml`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::HashMap;

/// Concurrency control for CI/CD workflows.
///
/// Prevents multiple pipelines from running simultaneously for the same group.
/// If `cancel_in_progress` is true, any currently running pipeline in the same
/// group will be cancelled before the new one starts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConcurrencyConfig {
    /// Concurrency group name. Pipelines with the same group will be serialized.
    /// Supports template variables: ${{ ref }}, ${{ branch }}
    pub group: String,

    /// If true, cancel any in-progress pipeline in the same group
    /// before starting the new one.
    #[serde(default)]
    pub cancel_in_progress: bool,
}

/// Top-level CI configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiConfig {
    /// Ordered list of stage names.
    #[serde(default)]
    pub stages: Option<Vec<String>>,

    /// Concurrency control configuration.
    /// When set, pipelines in the same concurrency group are serialized.
    #[serde(default)]
    pub concurrency: Option<ConcurrencyConfig>,

    /// Map of job name → job config.
    /// Jobs not listed in `stages` will be placed in a "default" stage.
    #[serde(flatten)]
    pub jobs: HashMap<String, JobConfig>,
}

/// A single CI job configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobConfig {
    /// Which stage this job belongs to.
    pub stage: Option<String>,

    /// Shell commands to execute (in order).
    pub script: Vec<String>,

    /// Container image to run in (future: Docker runner).
    pub image: Option<String>,

    /// Only run this job on these branch names.
    #[serde(default)]
    pub only: Option<Vec<String>>,

    /// Environment variables.
    #[serde(default)]
    pub variables: Option<HashMap<String, String>>,

    /// Whether this job can be manually triggered.
    #[serde(default)]
    pub when: Option<String>,

    /// Static job condition evaluated against CI context before scheduling.
    #[serde(default, rename = "if", alias = "condition")]
    pub condition: Option<String>,

    /// Deployment environment name. Protected environments pause for approval.
    #[serde(default)]
    pub environment: Option<String>,

    /// Allow failure without marking the pipeline as failed.
    #[serde(default)]
    pub allow_failure: Option<bool>,

    /// Per-job timeout in seconds. Accepted range: `1..=86_400`.
    ///
    /// Deliberately signed even though a negative timeout is meaningless. As a
    /// `u64` the range was enforced in two unrelated places with two unrelated
    /// answers: `-1` died inside `serde_yaml` with `invalid value: integer -1,
    /// expected u64` and no job name (`#[serde(flatten)]` on the job map drops
    /// the span), while `0` and `86_401` got the validator's message naming the
    /// job and the rule. Taking the value in and letting
    /// `validate_execution_semantics` judge it puts every rejection on one path.
    #[serde(default)]
    pub timeout_seconds: Option<i64>,

    /// Runner tags/labels required for this job.
    /// Jobs with tags will only be picked up by runners matching those tags.
    /// An empty or missing tags list means any runner can pick up the job.
    #[serde(default)]
    pub tags: Option<Vec<String>>,

    /// Cartesian-product job matrix. At most 256 variants are allowed.
    #[serde(default)]
    pub matrix: Option<BTreeMap<String, Vec<String>>>,

    #[serde(default)]
    pub cache: Option<CacheConfig>,

    /// Compiled GitHub/Gitea Actions expressions for fields resolved while
    /// matrix variants are materialised. Native `.forgekeep-ci.yml` cannot set
    /// this field, so its literal strings keep their existing semantics.
    #[serde(skip)]
    pub(crate) action_templates: Option<ActionJobTemplates>,
}

/// The line every job script starts with, and the reason it has to.
///
/// A job's `script` is handed to `sh -c` as one program, and a shell reports
/// the exit code of the *last* command it ran. Without this line
/// `script: [cargo test, cargo build]` finishes `0` whenever the build succeeds,
/// however the tests went — the pipeline goes green and the only trace of the
/// failure is log text nobody reads under a green job.
const FAIL_FAST: &str = "set -e";

impl JobConfig {
    /// The shell program this job runs, fail-fast guaranteed.
    ///
    /// card_38e374f3ed84: this is deliberately the *one* place either config
    /// format turns a `script` list into the string stored on `pipeline_job`.
    /// The Gitea Actions translation had prepended [`FAIL_FAST`] itself since
    /// `build_job_script` — "GitHub's default bash invocation is fail-fast" —
    /// while the native `.forgekeep-ci.yml` path joined the author's lines
    /// untouched, so the two formats disagreed about whether a failing command
    /// fails the job. Reading the contract off a shared helper is what stops
    /// them drifting apart a second time; the Actions prefix is kept idempotent
    /// rather than removed, so that path's own tests still describe it.
    pub fn shell_script(&self) -> String {
        if self.script.first().map(|line| line.trim()) == Some(FAIL_FAST) {
            return self.script.join("\n");
        }
        std::iter::once(FAIL_FAST)
            .chain(self.script.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ActionJobTemplates {
    pub(crate) image: Option<ActionTemplate>,
    pub(crate) tags: Option<Vec<ActionTemplate>>,
    pub(crate) environment: Option<ActionTemplate>,
}

impl ActionJobTemplates {
    pub(crate) fn is_empty(&self) -> bool {
        self.image.is_none() && self.tags.is_none() && self.environment.is_none()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ActionTemplate {
    parts: Vec<ActionTemplatePart>,
}

impl ActionTemplate {
    pub(crate) fn new(parts: Vec<ActionTemplatePart>) -> Self {
        Self { parts }
    }

    pub(crate) fn literal(value: String) -> Self {
        Self::new(vec![ActionTemplatePart::Literal(value)])
    }

    pub(crate) fn unsupported_expressions(&self) -> Vec<String> {
        self.parts
            .iter()
            .filter_map(|part| match part {
                ActionTemplatePart::Expression(ActionExpression::Unsupported(name)) => {
                    Some(name.clone())
                }
                _ => None,
            })
            .collect()
    }

    pub(crate) fn render(
        &self,
        mut resolve: impl FnMut(&ActionExpression) -> Option<String>,
    ) -> std::result::Result<String, String> {
        let mut rendered = String::new();
        for part in &self.parts {
            match part {
                ActionTemplatePart::Literal(value) => rendered.push_str(value),
                ActionTemplatePart::Expression(expression) => {
                    let value = resolve(expression).ok_or_else(|| expression.source_name())?;
                    rendered.push_str(&value);
                }
            }
        }
        Ok(rendered)
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ActionTemplatePart {
    Literal(String),
    Expression(ActionExpression),
}

#[derive(Debug, Clone)]
pub(crate) enum ActionExpression {
    GithubRef,
    GithubSha,
    GithubEventName,
    GithubRepository,
    GithubRepositoryOwner,
    Matrix(String),
    Input(String),
    Unsupported(String),
}

impl ActionExpression {
    pub(crate) fn source_name(&self) -> String {
        match self {
            Self::GithubRef => "github.ref".into(),
            Self::GithubSha => "github.sha".into(),
            Self::GithubEventName => "github.event_name".into(),
            Self::GithubRepository => "github.repository".into(),
            Self::GithubRepositoryOwner => "github.repository_owner".into(),
            Self::Matrix(name) => format!("matrix.{name}"),
            Self::Input(name) => format!("inputs.{name}"),
            Self::Unsupported(name) => name.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    pub key: String,
    pub paths: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// The reference an author of a `.forgekeep-ci.yml` reads, by the path they
    /// are pointed at.
    ///
    /// `include_str!` rather than a runtime `read_to_string`: the path is
    /// resolved at compile time (so a moved or renamed document breaks the
    /// build instead of silently skipping the checks below), and editing the
    /// document rebuilds — and therefore re-runs — the tests.
    const CI_DOCUMENTATION: (&str, &str) = ("docs/ci.md", include_str!("../../../docs/ci.md"));

    /// The production half of this file. The inventory below is read off the
    /// declaration itself, with the test module cut away so a key that exists
    /// only in a fixture cannot pass for a key of the model.
    fn production_config_source() -> &'static str {
        include_str!("config.rs")
            .split_once("\n#[cfg(test)]\n")
            .map(|(production, _)| production)
            .expect("config.rs must keep its test module behind #[cfg(test)]")
    }

    /// One field of a serde struct, as the reader of the YAML sees it.
    struct SerdeField {
        /// The Rust field name, for error messages.
        field: String,
        /// The key an author actually writes — `#[serde(rename = "…")]` wins.
        key: String,
        /// The type text, so the walk below can find the next config struct.
        type_text: String,
        /// `#[serde(flatten)]`: the field carries no key of its own.
        flattened: bool,
        /// `#[serde(skip)]`: the format cannot express it at all.
        skipped: bool,
    }

    /// Whether a serde attribute list carries a bare flag, as a whole token —
    /// so `skip_serializing_if` is never mistaken for `skip`.
    fn has_serde_flag(attributes: &str, flag: &str) -> bool {
        attributes
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .any(|token| token == flag)
    }

    /// The name `#[serde(rename = "…")]` gives a field, when it renames one.
    fn serde_rename(attributes: &str) -> Option<&str> {
        attributes
            .split_once("rename = \"")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(name, _)| name)
    }

    /// The fields of a struct declared in `source`, in declaration order.
    ///
    /// Reading the declaration rather than keeping a list beside it is the
    /// whole point: a key added to the model joins the contract below by
    /// existing, not by someone remembering to register it.
    fn serde_fields(source: &str, type_name: &str) -> Vec<SerdeField> {
        let declaration = format!("pub struct {type_name} {{");
        let body = source
            .split_once(declaration.as_str())
            .map(|(_, rest)| rest)
            .and_then(|rest| rest.split_once("\n}").map(|(body, _)| body))
            .unwrap_or_else(|| panic!("{type_name} declaration must be present in config.rs"));

        let mut fields = Vec::new();
        let mut attributes = String::new();

        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() {
                attributes.clear();
                continue;
            }
            if line.starts_with("#[") {
                attributes.push_str(line);
                continue;
            }
            let Some((field, type_text)) = line
                .strip_prefix("pub ")
                .or_else(|| line.strip_prefix("pub(crate) "))
                .and_then(|declaration| declaration.split_once(':'))
            else {
                continue;
            };
            fields.push(SerdeField {
                field: field.trim().to_owned(),
                key: serde_rename(&attributes)
                    .unwrap_or_else(|| field.trim())
                    .to_owned(),
                type_text: type_text.trim().trim_end_matches(',').to_owned(),
                flattened: has_serde_flag(&attributes, "flatten"),
                skipped: has_serde_flag(&attributes, "skip"),
            });
            attributes.clear();
        }
        fields
    }

    /// Every config struct the file format reaches, starting at [`CiConfig`]
    /// and following field types that are themselves declared here.
    ///
    /// Derived rather than hand-listed for the same reason as the fields: a new
    /// `FooConfig` hung off a job joins the documentation contract by being
    /// reachable, not by being remembered.
    fn ci_config_types(source: &str) -> Vec<String> {
        let mut reachable = vec!["CiConfig".to_owned()];
        let mut visited = 0;

        while visited < reachable.len() {
            let type_name = reachable[visited].clone();
            visited += 1;

            for field in serde_fields(source, &type_name) {
                if field.skipped {
                    continue;
                }
                for candidate in field
                    .type_text
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                {
                    if candidate.ends_with("Config")
                        && source.contains(&format!("pub struct {candidate} {{"))
                        && !reachable.iter().any(|known| known == candidate)
                    {
                        reachable.push(candidate.to_owned());
                    }
                }
            }
        }
        reachable
    }

    /// The ```yaml fenced blocks of a markdown document, as `(line number of
    /// the block's first content line, block body)`.
    fn yaml_code_blocks(name: &str, content: &str) -> Vec<(usize, String)> {
        let mut blocks = Vec::new();
        let mut body: Vec<&str> = Vec::new();
        let mut start = 0usize;
        let mut inside = false;

        for (index, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if inside {
                if trimmed == "```" {
                    blocks.push((start, body.join("\n")));
                    body.clear();
                    inside = false;
                } else {
                    body.push(line);
                }
            } else if trimmed == "```yaml" {
                inside = true;
                start = index + 2;
            }
        }

        assert!(
            !inside,
            "{name}:{start}: a ```yaml block is never closed — the extractor \
             reads the rest of the document as configuration"
        );
        blocks
    }

    /// Every mapping key a YAML block shows, at any depth.
    fn yaml_keys(body: &str) -> BTreeSet<&str> {
        body.lines()
            .filter_map(|line| {
                let line = line.trim();
                let line = line.strip_prefix("- ").unwrap_or(line);
                let (key, rest) = line.split_once(':')?;
                (!key.is_empty()
                    && key
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                    && (rest.is_empty() || rest.starts_with(' ')))
                .then_some(key)
            })
            .collect()
    }

    /// Fields that carry no key of their own, each with the reason. The list
    /// exists so that a second flattened field is a decision someone made,
    /// rather than a key that quietly stopped needing documentation.
    const NAMED_BY_THE_READER: [(&str, &str, &str); 1] = [(
        "CiConfig",
        "jobs",
        "flattened: every top-level key that is not `stages` or `concurrency` \
         is a job, and the names are the author's own — the document describes \
         the shape instead of a key",
    )];

    /// The example an author copies has to be a file this engine accepts.
    ///
    /// `JobConfig`, `ConcurrencyConfig` and `CacheConfig` are all
    /// `deny_unknown_fields`, so a key that drifted in the documentation is not
    /// cosmetic: pasting it produces a pipeline that will not start, and the
    /// author has nowhere else to look the right name up.
    #[test]
    fn every_yaml_block_in_the_ci_documentation_loads_as_config() {
        let (name, content) = CI_DOCUMENTATION;
        let mut checked = 0;

        for (line, body) in yaml_code_blocks(name, content) {
            let config: CiConfig = serde_yaml::from_str(&body).unwrap_or_else(|error| {
                panic!(
                    "{name}:{line}: this ```yaml block is what a reader commits to their own \
                     repository, and it is not valid `.forgekeep-ci.yml`: {error}"
                )
            });

            // A block that parses can still be a file the trigger refuses —
            // an undeclared `stage:`, a stage name listed twice, a block with
            // no job in it. The rule is not restated here: the example is put
            // through the very function that judges the reader's own commit, so
            // a rule that changes cannot leave the documentation behind.
            crate::validate_execution_semantics(&config).unwrap_or_else(|error| {
                panic!(
                    "{name}:{line}: this ```yaml block parses, but the engine refuses to run it: \
                     {error:#}"
                )
            });
            checked += 1;
        }

        // A floor, not a count: it fails loudly if the fence scanner ever stops
        // matching and the test quietly checks nothing.
        assert!(
            checked >= 3,
            "only {checked} ```yaml blocks found in {name} — the scanner has stopped matching them"
        );
    }

    /// The mirror of the check above: that one asks that everything the
    /// document shows is real, this asks that everything real is shown.
    ///
    /// A key nobody can discover is worse here than in the server's own config.
    /// This file lives in the *author's* repository, every block that takes it
    /// is `deny_unknown_fields`, and there is no half-working middle: the name
    /// is either found in this document or guessed, and a guess is a refused
    /// pipeline.
    #[test]
    fn every_key_the_ci_model_accepts_is_shown_in_the_documentation() {
        let (name, content) = CI_DOCUMENTATION;
        let blocks = yaml_code_blocks(name, content);
        let documented: BTreeSet<&str> = blocks
            .iter()
            .flat_map(|(_, body)| yaml_keys(body))
            .collect();

        let source = production_config_source();
        let mut excused = BTreeSet::new();
        let mut checked = 0;

        for type_name in ci_config_types(source) {
            for field in serde_fields(source, &type_name) {
                if field.skipped {
                    continue;
                }
                if field.flattened {
                    let reason =
                        NAMED_BY_THE_READER
                            .iter()
                            .find(|&&(excused_type, excused_field, _)| {
                                excused_type == type_name && excused_field == field.field
                            });
                    assert!(
                        reason.is_some(),
                        "`{}` of {type_name} is flattened into the document and named in no \
                         `NAMED_BY_THE_READER` entry — say in {name} what an author writes there, \
                         then record the reason it has no key of its own",
                        field.field
                    );
                    excused.insert((type_name.clone(), field.field.clone()));
                    continue;
                }
                assert!(
                    documented.contains(field.key.as_str()),
                    "no ```yaml block in {name} shows `{}` of {type_name}, so the only way to \
                     learn the key exists is to read config.rs — and `deny_unknown_fields` means \
                     an author who guesses the name gets a refused pipeline instead of a hint",
                    field.key
                );
                checked += 1;
            }
        }

        for (type_name, field, reason) in NAMED_BY_THE_READER {
            assert!(
                excused.contains(&(type_name.to_owned(), field.to_owned())),
                "`{field}` of {type_name} is excused from the documentation as {reason:?}, but it \
                 is no longer a flattened field of the model — drop the excuse or restore it"
            );
        }

        // The floor guards the scanner, not the model: a declaration format it
        // stopped recognising would leave this test asserting nothing.
        assert!(
            checked >= 15,
            "only {checked} keys read off the model — the declaration scanner has stopped matching"
        );
    }

    #[test]
    fn test_parse_simple_config() {
        let yml = r#"
stages:
  - build
  - test

build_app:
  stage: build
  script:
    - echo "Building..."
    - make build

test_unit:
  stage: test
  script:
    - make test
"#;
        let config: CiConfig = serde_yaml::from_str(yml).unwrap();
        assert_eq!(config.stages.as_ref().unwrap().len(), 2);
        assert_eq!(config.jobs.len(), 2);

        let build = config.jobs.get("build_app").unwrap();
        assert_eq!(build.stage.as_deref(), Some("build"));
        assert_eq!(build.script.len(), 2);

        let test = config.jobs.get("test_unit").unwrap();
        assert_eq!(test.stage.as_deref(), Some("test"));
    }

    /// card_38e374f3ed84: a native job whose first command fails used to finish
    /// `success`, because `sh -c` reports the last command's exit code and the
    /// native path handed the author's lines over untouched.
    ///
    /// This runs the script the way the runner does rather than comparing it to
    /// an expected string — the claim is about what the shell *does*, and a
    /// string assertion would keep passing if `sh` stopped honouring `set -e`.
    #[cfg(unix)]
    #[test]
    fn a_failing_command_fails_the_native_job_even_when_a_later_one_succeeds() {
        let config: CiConfig = serde_yaml::from_str(
            r#"
verify:
  stage: test
  script:
    - "false"
    - echo second-ran
"#,
        )
        .unwrap();
        let script = config.jobs.get("verify").unwrap().shell_script();

        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .output()
            .expect("sh is available on unix");

        assert!(
            !output.status.success(),
            "a job whose first command failed must not exit 0, script was:\n{script}"
        );
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("second-ran"),
            "the shell must stop at the failure rather than run on: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }

    /// The discrimination, run the same way: a script whose commands all
    /// succeed still succeeds, and every one of them still runs.
    #[cfg(unix)]
    #[test]
    fn a_healthy_native_job_still_runs_every_command_and_succeeds() {
        let config: CiConfig = serde_yaml::from_str(
            r#"
verify:
  script:
    - echo first-ran
    - echo second-ran
"#,
        )
        .unwrap();

        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(config.jobs.get("verify").unwrap().shell_script())
            .output()
            .expect("sh is available on unix");

        assert!(output.status.success());
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("first-ran") && stdout.contains("second-ran"),
            "{stdout}"
        );
    }

    /// The Actions translation prepends the same line itself
    /// (`GiteaWorkflow::build_job_script`), and the shared helper must not
    /// double it — the two formats agree on the contract, they do not stack it.
    #[test]
    fn a_script_that_already_declares_fail_fast_is_not_prefixed_twice() {
        let config: CiConfig = serde_yaml::from_str(
            r#"
verify:
  script:
    - set -e
    - echo one
"#,
        )
        .unwrap();

        assert_eq!(
            config.jobs.get("verify").unwrap().shell_script(),
            "set -e\necho one"
        );
    }

    #[test]
    fn test_parse_with_only() {
        let yml = r#"
stages:
  - deploy

deploy_prod:
  stage: deploy
  script:
    - make deploy
  only:
    - main
"#;
        let config: CiConfig = serde_yaml::from_str(yml).unwrap();
        let deploy = config.jobs.get("deploy_prod").unwrap();
        assert_eq!(deploy.only.as_ref().unwrap().len(), 1);
        assert_eq!(deploy.only.as_ref().unwrap()[0], "main");
    }

    #[test]
    fn test_parse_minimal_config() {
        // A job with only script (no stage, no image, etc.)
        let yml = r#"
hello:
  script:
    - echo "hello"
"#;
        let config: CiConfig = serde_yaml::from_str(yml).unwrap();
        assert!(config.stages.is_none());
        assert_eq!(config.jobs.len(), 1);
        let job = config.jobs.get("hello").unwrap();
        assert!(job.stage.is_none());
        assert_eq!(job.script, vec!["echo \"hello\""]);
        assert!(job.image.is_none());
        assert!(job.only.is_none());
        assert!(job.allow_failure.is_none());
    }

    #[test]
    fn test_parse_with_all_fields() {
        let yml = r#"
stages:
  - build

full_job:
  stage: build
  script:
    - cargo build
  image: rust:1.75
  only:
    - main
    - develop
  variables:
    RUST_BACKTRACE: "1"
    CARGO_HOME: /cargo
  when: manual
  environment: production
  allow_failure: true
  tags:
    - docker
    - linux
  cache:
    key: cargo-main
    paths:
      - target
"#;
        let config: CiConfig = serde_yaml::from_str(yml).unwrap();
        let job = config.jobs.get("full_job").unwrap();
        assert_eq!(job.image.as_deref(), Some("rust:1.75"));
        assert_eq!(job.only.as_ref().unwrap().len(), 2);
        assert_eq!(
            job.variables
                .as_ref()
                .unwrap()
                .get("RUST_BACKTRACE")
                .unwrap(),
            "1"
        );
        assert_eq!(job.when.as_deref(), Some("manual"));
        assert_eq!(job.environment.as_deref(), Some("production"));
        assert_eq!(job.allow_failure, Some(true));
        assert_eq!(
            job.tags.as_ref().unwrap(),
            &vec!["docker".to_string(), "linux".to_string()]
        );
        let cache = job.cache.as_ref().unwrap();
        assert_eq!(cache.key, "cargo-main");
        assert_eq!(cache.paths, vec!["target"]);
    }

    #[test]
    fn test_parse_empty_jobs() {
        let yml = r#"
stages: []
"#;
        let config: CiConfig = serde_yaml::from_str(yml).unwrap();
        assert!(config.stages.as_ref().unwrap().is_empty());
        assert!(config.jobs.is_empty());
    }

    #[test]
    fn test_parse_multiple_jobs_same_stage() {
        let yml = r#"
stages:
  - test

unit_tests:
  stage: test
  script:
    - cargo test --lib

integration_tests:
  stage: test
  script:
    - cargo test --test integration
"#;
        let config: CiConfig = serde_yaml::from_str(yml).unwrap();
        assert_eq!(config.jobs.len(), 2);
        for job in config.jobs.values() {
            assert_eq!(job.stage.as_deref(), Some("test"));
        }
    }

    #[test]
    fn test_config_serialization_roundtrip() {
        let yml = r#"
stages:
  - build

build:
  stage: build
  script:
    - make
"#;
        let config: CiConfig = serde_yaml::from_str(yml).unwrap();
        let serialized = serde_yaml::to_string(&config).unwrap();
        let deserialized: CiConfig = serde_yaml::from_str(&serialized).unwrap();
        assert_eq!(config.stages, deserialized.stages);
        assert_eq!(config.jobs.len(), deserialized.jobs.len());
    }

    #[test]
    fn test_parse_with_concurrency() {
        let yml = r#"
stages:
  - deploy

concurrency:
  group: prod-deploy
  cancel_in_progress: true

deploy:
  stage: deploy
  script:
    - make deploy
"#;
        let config: CiConfig = serde_yaml::from_str(yml).unwrap();
        let cc = config.concurrency.as_ref().unwrap();
        assert_eq!(cc.group, "prod-deploy");
        assert!(cc.cancel_in_progress);
        assert_eq!(config.jobs.len(), 1);
    }

    #[test]
    fn test_parse_concurrency_defaults() {
        let yml = r#"
stages:
  - test

concurrency:
  group: ${{ branch }}

test:
  stage: test
  script:
    - make test
"#;
        let config: CiConfig = serde_yaml::from_str(yml).unwrap();
        let cc = config.concurrency.as_ref().unwrap();
        assert_eq!(cc.group, "${{ branch }}");
        assert!(!cc.cancel_in_progress); // default false
    }

    #[test]
    fn test_parse_without_concurrency() {
        let yml = r#"
stages:
  - build

build:
  stage: build
  script:
    - make
"#;
        let config: CiConfig = serde_yaml::from_str(yml).unwrap();
        assert!(config.concurrency.is_none());
    }
}
