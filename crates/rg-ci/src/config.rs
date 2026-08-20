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
    ///
    /// The serde default is spelled through a function rather than left to
    /// `bool::default()`: `docs/ci.md` states this value in its `Concurrency`
    /// table, and a page can only be held to a default that has a name.
    #[serde(default = "default_cancel_in_progress")]
    pub cancel_in_progress: bool,
}

/// What an omitted `cancel_in_progress:` means: refuse the new pipeline while
/// the group is busy rather than cancel what is already running.
///
/// Cancelling is the destructive reading of an unwritten key, so the absence
/// has to mean the other one.
pub(crate) const DEFAULT_CANCEL_IN_PROGRESS: bool = false;

fn default_cancel_in_progress() -> bool {
    DEFAULT_CANCEL_IN_PROGRESS
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

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    /// The reference an author of a `.forgekeep-ci.yml` reads, by the path they
    /// are pointed at.
    ///
    /// `include_str!` rather than a runtime `read_to_string`: the path is
    /// resolved at compile time (so a moved or renamed document breaks the
    /// build instead of silently skipping the checks below), and editing the
    /// document rebuilds — and therefore re-runs — the tests.
    const CI_DOCUMENTATION: (&str, &str) = ("docs/ci.md", include_str!("../../../docs/ci.md"));

    /// The production view of this file. The inventory below is read off the
    /// declaration itself, with complete test items blanked so a key that exists
    /// only in a fixture cannot pass for a key of the model.
    fn production_config_source() -> String {
        rust_source::production_rust_source(include_str!("config.rs"))
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
        /// `#[serde(default)]` in either spelling — bare, or naming a function.
        /// Together with an `Option<…>` type this is what makes a key omissible,
        /// and therefore what decides whether the document owes it a default at
        /// all.
        defaulted: bool,
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

    /// The body of a struct declared in `source`, found through a byte-aligned
    /// code-only view and returned from the original source.
    fn struct_body<'a>(source: &'a str, code: &str, type_name: &str) -> Option<&'a str> {
        let declaration = format!("struct {type_name} {{");
        let body_start = code.find(&declaration)? + declaration.len();
        let mut braces = 1usize;

        for (relative, byte) in code.as_bytes()[body_start..].iter().enumerate() {
            match byte {
                b'{' => braces += 1,
                b'}' => {
                    braces = braces.saturating_sub(1);
                    if braces == 0 {
                        return source.get(body_start..body_start + relative);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// The fields of a struct declared in `source`, in declaration order.
    ///
    /// Reading the declaration rather than keeping a list beside it is the
    /// whole point: a key added to the model joins the contract below by
    /// existing, not by someone remembering to register it.
    fn serde_fields(source: &str, type_name: &str) -> Vec<SerdeField> {
        let code = rust_source::production_rust_code_only(source);
        serde_fields_in_view(source, &code, type_name)
    }

    fn serde_fields_in_view(source: &str, code: &str, type_name: &str) -> Vec<SerdeField> {
        let body = struct_body(source, code, type_name)
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
                defaulted: has_serde_flag(&attributes, "default"),
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
        let code = rust_source::production_rust_code_only(source);
        let mut reachable = vec!["CiConfig".to_owned()];
        let mut visited = 0;

        while visited < reachable.len() {
            let type_name = reachable[visited].clone();
            visited += 1;

            for field in serde_fields_in_view(source, &code, &type_name) {
                if field.skipped {
                    continue;
                }
                for candidate in field
                    .type_text
                    .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                {
                    if candidate.ends_with("Config")
                        && struct_body(source, &code, candidate).is_some()
                        && !reachable.iter().any(|known| known == candidate)
                    {
                        reachable.push(candidate.to_owned());
                    }
                }
            }
        }
        reachable
    }

    #[test]
    fn ci_config_model_inventory_ignores_declaration_shaped_rust_data() {
        let source = r####"
// pub struct CiConfig {
//     pub comment_only: bool,
// }
const NORMAL_DECOY: &str = "pub struct CiConfig {
    pub normal_only: bool,
}
";
const RAW_DECOY: &str = r#"pub struct JobConfig {
    pub raw_only: bool,
}"#;
const BYTE_DECOY: &[u8] = b"pub struct CacheConfig {
    pub byte_only: bool,
}
";

pub struct CiConfig {
    #[serde(rename = "stage_names")]
    pub stages: Vec<String>,
    pub job: JobConfig,
}

pub struct JobConfig {
    pub cache: CacheConfig,
}

pub struct CacheConfig {
    pub key: String,
}
"####;

        assert_eq!(
            serde_fields(source, "CiConfig")
                .into_iter()
                .map(|field| field.key)
                .collect::<Vec<_>>(),
            ["stage_names", "job"]
        );
        assert_eq!(
            ci_config_types(source),
            ["CiConfig", "JobConfig", "CacheConfig"]
        );
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

        let type_names = ci_config_types(&source);
        assert!(
            type_names.len() >= 4,
            "only {} config model types are reachable — the struct scanner has stopped matching",
            type_names.len()
        );

        for type_name in type_names {
            for field in serde_fields(&source, &type_name) {
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

    // ---------------------------------------------------------------------
    // The *values* those same tables promise.
    //
    // The two checks above pin the names: every key the document shows is one
    // this engine accepts, and every key it accepts is shown. Neither of them
    // looks at the column beside the name. `docs/ci.md` carries two tables
    // spelling `| Key | Type | Default | Meaning |`, and four of their cells
    // state a literal value — `stage`, `when`, `allow_failure` and
    // `cancel_in_progress`. Those four are what the author reads when they
    // *omit* a key, which is the case they never test: nobody writes a file to
    // check that the key they did not write does what the page said.
    //
    // Two of the four had no name in the code at all until this module was
    // written (`unwrap_or(false)`, `unwrap_or("on_success")`), so there was
    // nothing a check could have held the page to. That is always the root of
    // this class, and the reason the reader below refuses a literal outright:
    // a default written into the resolve is unreachable from any contract, and
    // the copies stay equal only until the first person edits one of them.
    //
    // The cost of a drifted cell here is not a wrong sentence. `stage` decides
    // which stage a job lands in (a shifted default once bought a green
    // pipeline that ran no command — card_d92cd3260864), `when` decides whether
    // the job runs by itself or waits for a click, and `allow_failure` decides
    // whether its failure is the pipeline's.
    // ---------------------------------------------------------------------

    /// The header of the `docs/ci.md` tables that state defaults. The other two
    /// tables of the document (`Top level`, `Cache`) have no `Default` column
    /// and promise nothing this module can check.
    const DEFAULT_TABLE_HEADER: &str = "| Key | Type | Default | Meaning |";

    /// How the tables spell "this key is required, so there is no default".
    const REQUIRED: &str = "—";

    /// One row of a table that has a `Default` column.
    struct DocumentedRow {
        /// Line number in the document, for failures that can be jumped to.
        line: usize,
        /// The key, as the row backticks it.
        key: String,
        /// The `Default` cell, verbatim.
        cell: String,
    }

    /// Every row of every `Default`-bearing table of `content`, with the number
    /// of such tables found.
    ///
    /// A row is only read while a table that declared the header above is open;
    /// the first line that is not a row closes it. Otherwise a `Default` column
    /// could be answered by a row of the `Cache` table, which has none.
    fn documented_rows(name: &str, content: &str) -> (usize, Vec<DocumentedRow>) {
        let mut rows = Vec::new();
        let mut tables = 0;
        let mut inside = false;

        for (index, line) in content.lines().enumerate() {
            let line = line.trim();
            if line == DEFAULT_TABLE_HEADER {
                tables += 1;
                inside = true;
                continue;
            }
            if !inside {
                continue;
            }
            if !line.starts_with('|') {
                inside = false;
                continue;
            }
            let cells: Vec<&str> = line.split('|').collect();
            // The separator row under the header.
            if cells.iter().all(|cell| {
                cell.trim()
                    .chars()
                    .all(|c| c == '-' || c == ':' || c.is_whitespace())
            }) {
                continue;
            }
            assert!(
                cells.len() >= 5,
                "{name}:{}: a row of a `Default` table has {} cells, not the four the header \
                 declares — the reader would take the wrong one for the default",
                index + 1,
                cells.len().saturating_sub(2)
            );
            let key = cells[1].trim();
            let key = key
                .strip_prefix('`')
                .and_then(|rest| rest.strip_suffix('`'))
                .unwrap_or_else(|| {
                    panic!(
                        "{name}:{}: the first cell of this row is {key:?}, not a backticked \
                         key — every row of a `Default` table names a key an author writes",
                        index + 1
                    )
                });
            rows.push(DocumentedRow {
                line: index + 1,
                key: key.to_owned(),
                cell: cells[3].trim().to_owned(),
            });
        }
        (tables, rows)
    }

    /// What a row states in its `Default` column.
    #[derive(Debug, PartialEq, Eq)]
    enum Stated<'a> {
        /// A literal value, and nothing but the value, in backticks.
        Value(&'a str),
        /// The key is required: there is no default to state.
        Required,
        /// Prose describing a behaviour rather than a value. Kept apart from
        /// the two above so a sentence cannot pass for either.
        Prose(&'a str),
    }

    /// The `Default` cell of a row, classified.
    ///
    /// The backticked span has to be the *whole* cell. A sentence that happens
    /// to carry one — "`1`–`86400`" — describes a behaviour, and reading its
    /// first code span as the default would bind the page to a value it never
    /// promised.
    fn stated_default(cell: &str) -> Stated<'_> {
        if cell == REQUIRED {
            return Stated::Required;
        }
        match cell
            .strip_prefix('`')
            .and_then(|rest| rest.strip_suffix('`'))
        {
            Some(value) if !value.contains('`') => Stated::Value(value),
            _ => Stated::Prose(cell),
        }
    }

    /// A default the document states, bound to the constant that produces it.
    struct DocumentedDefault {
        /// The key whose row states it.
        key: &'static str,
        /// The identifier the fallback hangs off in the production source —
        /// usually the field, but `when` is applied one crate down, where the
        /// value arrives as `when_condition`.
        resolved_as: &'static str,
        /// The `DEFAULT_*` name, for the census and for failure messages.
        constant: &'static str,
        /// Its value, read *from* the constant rather than copied beside it.
        value: String,
    }

    /// The pairing table. The key spellings have to be written out — no rule
    /// derives `DEFAULT_STAGE` from `stage` — but no value is: each row reads
    /// its constant, so renaming one breaks the build and changing one fails
    /// every check below.
    fn documented_defaults() -> Vec<DocumentedDefault> {
        macro_rules! defaults {
            ($(($key:literal, $resolved:literal, $name:literal, $konst:expr)),+ $(,)?) => {
                vec![$(DocumentedDefault {
                    key: $key,
                    resolved_as: $resolved,
                    constant: $name,
                    value: $konst.to_string(),
                }),+]
            };
        }

        defaults![
            ("stage", "stage", "DEFAULT_STAGE", crate::DEFAULT_STAGE),
            (
                "when",
                "when_condition",
                "DEFAULT_JOB_WHEN",
                rg_db::ops::pipeline_ops::DEFAULT_JOB_WHEN
            ),
            (
                "allow_failure",
                "allow_failure",
                "DEFAULT_ALLOW_FAILURE",
                crate::DEFAULT_ALLOW_FAILURE
            ),
            (
                "cancel_in_progress",
                "cancel_in_progress",
                "DEFAULT_CANCEL_IN_PROGRESS",
                super::DEFAULT_CANCEL_IN_PROGRESS
            ),
        ]
    }

    /// The closed vocabulary of `Default` cells that describe a behaviour
    /// instead of stating a value, each with the reason there is no value to
    /// state. A phrase outside this list has to be added deliberately, which is
    /// what stops "none" quietly growing into a value nothing holds.
    const PROSE_DEFAULTS: [(&str, &str); 4] = [
        (
            "none",
            "the key is absent and the engine does nothing in its place — there is no value \
             to name",
        ),
        (
            "run always",
            "a filter that is not written narrows nothing; the absence is the whole meaning",
        ),
        (
            "any runner",
            "an empty tag list is not a tag every runner carries — it is the assignment being \
             unconstrained",
        ),
        (
            "instance default",
            "the operator's `[timeouts].job_secs`, not a constant of this engine: the value \
             belongs to the instance and differs between them",
        ),
    ];

    /// Built-in defaults of this engine that no row of `docs/ci.md` states,
    /// each with the reason. The list exists so the next unpaired default is a
    /// decision someone wrote down rather than one that slipped past the census.
    const DEFAULTS_NOT_IN_THE_CI_DOCUMENT: [(&str, &str); 1] = [(
        "DEFAULT_CI_TOKEN_SCOPES",
        "the scopes the engine mints `CI_JOB_TOKEN` with; `.forgekeep-ci.yml` has no key \
             for them, so there is no row this could pair with",
    )];

    /// Where a fallback comes from.
    #[derive(Debug, PartialEq, Eq)]
    enum Fallback<'a> {
        /// A named constant — the only shape a page can be bound to.
        Constant(&'a str),
        /// A value written into the resolve itself. This is the shape this
        /// whole section exists to stop coming back.
        Literal(&'a str),
        /// Anything else, carrying the text so the failure names the shape.
        Unknown(&'a str),
    }

    /// The argument of an `unwrap_or`-shaped fallback, classified.
    fn classify_fallback(argument: &str) -> Fallback<'_> {
        // `unwrap_or_else(|_| …)` — step over the closure header first, so the
        // lazy spelling of a literal is read as the literal it is.
        let argument = match argument.trim_start().strip_prefix('|') {
            Some(rest) => rest
                .split_once('|')
                .map_or("", |(_, body)| body)
                .trim_start(),
            None => argument.trim_start(),
        };

        if let Some((literal, _)) = argument
            .strip_prefix('"')
            .and_then(|rest| rest.split_once('"'))
        {
            return Fallback::Literal(literal);
        }
        let end = argument
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
            .unwrap_or(argument.len());
        let token = &argument[..end];
        if token.is_empty() {
            // Everything after the paren is still in hand here, so bound the
            // shape to the line it starts on: a diagnostic that pastes the
            // remainder of the file names the defect no better and buries it.
            let shape = argument.split('\n').next().unwrap_or(argument);
            return Fallback::Unknown(shape.trim_end_matches([')', ',', ';']).trim());
        }
        if token == "true" || token == "false" || token.chars().all(|c| c.is_ascii_digit()) {
            return Fallback::Literal(token);
        }
        let name = token.rsplit("::").next().unwrap_or(token);
        if name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            return Fallback::Constant(name);
        }
        Fallback::Unknown(token)
    }

    /// Every fallback `source` applies to `receiver`.
    ///
    /// The method call has to follow the identifier directly, through nothing
    /// but the adapters that carry a value unchanged. A looser reader would let
    /// an unrelated `unwrap_or` further down the same statement stand in for
    /// the fallback being looked for — and answer "named constant" about a call
    /// that has nothing to do with the key.
    ///
    /// Two views, and it takes both for the same reason `struct_body` above
    /// does. The call is *located* in `code`, where literals are spaces, so an
    /// `unwrap_or` written inside a comment or a raw fixture is not a resolve
    /// this engine performs. The argument is then *read* out of `source` at the
    /// same byte offset, because the one shape this section exists to catch —
    /// a value written into the resolve itself — is a literal, and the view
    /// that makes the location trustworthy is the view that has already erased
    /// it. Reading both out of `code` left `Fallback::Literal(<string>)`
    /// unreachable on the production path: `unwrap_or("on_success")` arrived as
    /// `unwrap_or(            )`, fell through to the empty-token branch and
    /// answered `Unknown`, so a literal fallback was still refused — by a
    /// diagnostic written for a different defect, which sends the author
    /// looking for a shape that is not there (card_d720bb328362).
    fn field_fallbacks<'a>(source: &'a str, code: &str, receiver: &str) -> Vec<Fallback<'a>> {
        const CARRIED: [&str; 5] = [
            ".as_deref()",
            ".as_ref()",
            ".copied()",
            ".cloned()",
            ".to_owned()",
        ];
        const UNWRAP_OR: &str = ".unwrap_or";
        let mut found = Vec::new();

        for (index, _) in code.match_indices(receiver) {
            if code[..index]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                continue;
            }
            let mut at = skip_space(code, index + receiver.len());
            while let Some(carried) = CARRIED.iter().find(|call| code[at..].starts_with(**call)) {
                at = skip_space(code, at + carried.len());
            }
            if !code[at..].starts_with(UNWRAP_OR) {
                continue;
            }
            let after = at + UNWRAP_OR.len();
            let Some(paren) = code[after..].find('(') else {
                continue;
            };
            match &code[after..after + paren] {
                "" | "_else" => found.push(classify_fallback(&source[after + paren + 1..])),
                // `unwrap_or_default()` and anything else: named by its shape,
                // because `bool::default()` is a value with no name either.
                other => found.push(Fallback::Unknown(&source[after..after + other.len()])),
            }
        }
        found
    }

    /// The offset of the first non-whitespace byte at or after `at`.
    ///
    /// The offset-preserving spelling of `trim_start`, which is what lets the
    /// walk above stay addressable in the second view.
    fn skip_space(text: &str, at: usize) -> usize {
        at + (text[at..].len() - text[at..].trim_start().len())
    }

    /// The `const DEFAULT_*` names `source` declares, whatever their
    /// visibility: a default that is private today is still a default an author
    /// meets. Read off the declarations rather than listed beside them — a
    /// constant added to the engine joins the census by existing.
    fn declared_default_constants(source: &str) -> BTreeSet<&str> {
        source
            .lines()
            .map(str::trim_start)
            .map(|line| line.strip_prefix("pub(crate) ").unwrap_or(line))
            .map(|line| line.strip_prefix("pub ").unwrap_or(line))
            .filter_map(|line| line.strip_prefix("const "))
            .filter_map(|rest| rest.split_once(':'))
            .map(|(name, _)| name.trim())
            .filter(|name| name.starts_with("DEFAULT_"))
            .collect()
    }

    /// The production `.rs` of this crate, with each file's complete
    /// `#[cfg(test)]` items blanked, plus the one file of `rg-db` where a
    /// documented default of this engine is actually applied.
    ///
    /// A directory walk rather than a list of `include_str!`s: the question is
    /// whether a default exists *anywhere* the engine resolves one, and a fixed
    /// list would have to be edited whenever one moves — which is the
    /// remembering these checks exist to remove. `pipeline_ops.rs` joins it
    /// through `include_str!` so that moving the file breaks the build rather
    /// than quietly emptying the scan: `when` is the one documented default
    /// whose fallback is applied at the row-writing layer, and a census that
    /// could not see it would report the engine as fully paired while the value
    /// sat in another crate.
    fn resolving_sources() -> Vec<ResolvingSource> {
        let src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = vec![ResolvingSource::new(
            "rg-db/src/ops/pipeline_ops.rs".to_owned(),
            include_str!("../../rg-db/src/ops/pipeline_ops.rs"),
        )];
        let mut pending = vec![src];

        while let Some(dir) = pending.pop() {
            let entries = std::fs::read_dir(&dir)
                .unwrap_or_else(|error| panic!("{}: {error}", dir.display()));

            for entry in entries {
                let path = entry.expect("a readable directory entry").path();
                if path.is_dir() {
                    pending.push(path);
                    continue;
                }
                if path.extension().is_none_or(|extension| extension != "rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                sources.push(ResolvingSource::new(path.display().to_string(), &text));
            }
        }
        sources
    }

    /// One file of the resolving corpus, in the two byte-aligned production
    /// views the fallback reader needs together.
    struct ResolvingSource {
        file: String,
        /// Comments and complete `#[cfg(test)]` items blanked; literals kept.
        /// The view a fallback's *value* is read out of.
        source: String,
        /// The same span with literals blanked too. The view a call is
        /// *located* in, so a resolve spelled inside a literal is not one.
        code: String,
    }

    impl ResolvingSource {
        fn new(file: String, text: &str) -> Self {
            Self {
                file,
                source: production_source(text),
                code: production_code(text),
            }
        }
    }

    /// Production Rust code with complete test items blanked. A constant
    /// declared in a fixture is not a default anyone meets, and a fallback
    /// written in one is not a fallback the engine applies.
    fn production_code(text: &str) -> String {
        rust_source::production_rust_code_only(text)
    }

    /// The string-bearing twin of [`production_code`], byte-aligned with it.
    fn production_source(text: &str) -> String {
        rust_source::production_rust_source(text)
    }

    /// The four values `docs/ci.md` promises for keys an author leaves out.
    ///
    /// Every other check on this document reads names. This one reads the
    /// column an author acts on without ever testing it: the behaviour of the
    /// key they did *not* write.
    #[test]
    fn every_default_the_ci_documentation_states_is_the_constant_that_produces_it() {
        // The readers have to be able to answer "no" before their "yes" is
        // worth anything.
        assert_eq!(
            stated_default("`on_success`"),
            Stated::Value("on_success"),
            "the cell reader does not recognise a value the way the tables state one"
        );
        assert_eq!(
            stated_default(REQUIRED),
            Stated::Required,
            "the cell reader does not recognise how the tables spell a required key"
        );
        assert_eq!(
            stated_default("run always"),
            Stated::Prose("run always"),
            "the cell reader invents a literal default out of prose describing a behaviour"
        );
        assert_eq!(
            stated_default("`1`–`86400`"),
            Stated::Prose("`1`–`86400`"),
            "the cell reader takes the first code span of a sentence for the whole default, so \
             a range would pass for a value"
        );

        let (name, content) = CI_DOCUMENTATION;
        let (tables, rows) = documented_rows(name, content);
        assert!(
            tables >= 2,
            "only {tables} `Default` tables found in {name} — the table scanner has stopped \
             matching, and every check below would then agree with anything"
        );
        assert!(
            rows.len() >= 15,
            "only {} rows read out of {name}'s `Default` tables — the row scanner has drifted \
             away from how they are written",
            rows.len()
        );

        let source = production_config_source();
        let mut fields = BTreeMap::new();
        for type_name in ci_config_types(&source) {
            for field in serde_fields(&source, &type_name) {
                if !field.skipped && !field.flattened {
                    fields.insert(field.key.clone(), (type_name.clone(), field));
                }
            }
        }

        let documented = documented_defaults();
        let mut paired = BTreeSet::new();
        let mut prose_used = BTreeSet::new();

        for row in &rows {
            let (type_name, field) = fields.get(&row.key).unwrap_or_else(|| {
                panic!(
                    "{name}:{}: this row describes `{}`, which no config struct accepts — the \
                     author writing it gets a refused pipeline, and the table is where they \
                     looked the key up",
                    row.line, row.key
                )
            });
            let omissible = field.defaulted || field.type_text.starts_with("Option<");

            match stated_default(&row.cell) {
                Stated::Value(shown) => {
                    let entry = documented
                        .iter()
                        .find(|entry| entry.key == row.key)
                        .unwrap_or_else(|| {
                            panic!(
                                "{name}:{}: the `{}` row states `{shown}` as the value an \
                                 omitted key takes, and nothing binds that cell to the code \
                                 that produces it. Give the fallback a `const DEFAULT_*` and \
                                 pair it in documented_defaults(), or say in prose what the \
                                 absence does",
                                row.line, row.key
                            )
                        });
                    assert_eq!(
                        shown, entry.value,
                        "{name}:{}: the `{}` row states the default `{shown}`, but `{}` — what \
                         the engine actually falls back to — is `{}`. An author who omits the \
                         key gets the second value and reads the first",
                        row.line, row.key, entry.constant, entry.value
                    );
                    assert!(
                        omissible,
                        "{name}:{}: the `{}` row states a default, but {type_name} takes the \
                         key as `{}` with no serde default — omitting it is a parse error, not \
                         the value this row promises",
                        row.line, row.key, field.type_text
                    );
                    paired.insert(row.key.clone());
                }
                Stated::Required => assert!(
                    !omissible,
                    "{name}:{}: the `{}` row is marked required, but {type_name} takes the key \
                     as `{}` — a file that omits it loads, and the row says it cannot",
                    row.line, row.key, field.type_text
                ),
                Stated::Prose(prose) => {
                    let (_, reason) = PROSE_DEFAULTS
                        .iter()
                        .find(|(phrase, _)| *phrase == prose)
                        .unwrap_or_else(|| {
                            panic!(
                                "{name}:{}: the `Default` cell of the `{}` row reads {prose:?}, \
                                 which is neither a backticked value nor one of the phrases \
                                 this document uses for \"there is no value\". State the value \
                                 in backticks and pair it with the constant, or add the phrase \
                                 to PROSE_DEFAULTS with the reason nothing produces one",
                                row.line, row.key
                            )
                        });
                    assert!(
                        !reason.is_empty(),
                        "a prose default excused without a reason"
                    );
                    assert!(
                        omissible,
                        "{name}:{}: the `{}` row describes what happens when the key is left \
                         out, but {type_name} takes it as `{}` with no serde default — leaving \
                         it out is a parse error",
                        row.line, row.key, field.type_text
                    );
                    prose_used.insert(prose.to_owned());
                }
            }
        }

        for entry in &documented {
            assert!(
                paired.contains(entry.key),
                "documented_defaults() binds `{}` to `{}`, and no row of {name} states a value \
                 for it any more — the constant is then held to a page that stopped promising \
                 anything",
                entry.key,
                entry.constant
            );
        }
        for (phrase, _) in PROSE_DEFAULTS {
            assert!(
                prose_used.contains(phrase),
                "PROSE_DEFAULTS still excuses {phrase:?}, which no `Default` cell of {name} \
                 reads any more — drop the entry so the list keeps meaning something"
            );
        }

        // Two of the four are cheap to ask the engine directly rather than to
        // read off its source, and a behaviour is what the row actually
        // promises. The other two need a pipeline in a database to observe.
        let omitted: CiConfig = serde_yaml::from_str(
            "concurrency:\n  group: probe\nprobe:\n  script:\n    - echo probe\n",
        )
        .expect("a job with no stage and a concurrency block with no cancel flag is a valid file");
        assert_eq!(
            omitted
                .concurrency
                .as_ref()
                .expect("the probe declares a concurrency block")
                .cancel_in_progress,
            super::DEFAULT_CANCEL_IN_PROGRESS,
            "a `concurrency:` block that omits `cancel_in_progress` does not load as \
             `DEFAULT_CANCEL_IN_PROGRESS` — the serde default and the constant the page is \
             held to have come apart"
        );
        assert!(
            crate::resolved_stage_order(&omitted)
                .iter()
                .any(|stage| stage == crate::DEFAULT_STAGE),
            "a job that names no stage is not placed in `DEFAULT_STAGE` — the table's `stage` \
             row is checked against a constant the engine no longer places jobs in"
        );
    }

    /// The other half of the same contract, and the one the document cannot
    /// state: that each value it names is reached through a *name*.
    ///
    /// A fallback written into `unwrap_or` as a literal is unreachable from any
    /// check — which is how `on_success` and `false` stood beside their pages
    /// with nothing holding them equal.
    #[test]
    fn every_ci_default_the_engine_resolves_comes_from_a_named_constant() {
        const PROBE: &str = "let a = job.stage.as_deref().unwrap_or(DEFAULT_STAGE);\n\
             let b = job.allow_failure.unwrap_or(false);\n\
             // let x = commented_when.unwrap_or(\"on_success\");\n\
             const SAMPLE: &str = r#\"let y = quoted_when.unwrap_or(\"on_success\");\"#;\n\
             let c = when_condition.unwrap_or(\"on_success\");\n\
             let d = flag.cancel_in_progress.unwrap_or_default();\n\
             let e = other.stage_name.to_string();\n\
             let f = job.tags.map(|t| t.len()).unwrap_or(0);\n";

        // The probe goes through the SAME pair of views the corpus below does.
        // Feeding it raw proved the reader over an input the production path
        // never produces: `unwrap_or("on_success")` reaches the walk as
        // `unwrap_or(            )`, so the `Fallback::Literal(<string>)` arm
        // this file's whole argument rests on was unreachable in production and
        // the self-check said otherwise (card_d720bb328362).
        let probe = ResolvingSource::new("probe.rs".to_owned(), PROBE);
        let fallbacks = |receiver| field_fallbacks(&probe.source, &probe.code, receiver);

        assert_eq!(
            fallbacks("stage"),
            vec![Fallback::Constant("DEFAULT_STAGE")],
            "the fallback reader does not recognise a constant reached through `as_deref`, or \
             it answers for `stage_name` as well as for `stage`"
        );
        assert_eq!(
            fallbacks("allow_failure"),
            vec![Fallback::Literal("false")],
            "the fallback reader takes a bare `false` for a named default, so the one shape \
             these checks exist to catch would pass"
        );
        assert_eq!(
            fallbacks("when_condition"),
            vec![Fallback::Literal("on_success")],
            "the fallback reader does not read a string literal written into the resolve"
        );
        assert_eq!(
            fallbacks("cancel_in_progress"),
            vec![Fallback::Unknown("_default")],
            "the fallback reader lets `unwrap_or_default()` pass as a named default — the \
             value it produces has no name either"
        );
        assert_eq!(
            fallbacks("tags"),
            Vec::new(),
            "the fallback reader walks past a method call between the field and `unwrap_or`, \
             so an unrelated fallback would answer for the key"
        );
        // The other side of reading the value out of the string-bearing view:
        // the call still has to be *located* in the code-only one, or the two
        // decoys above would each answer for a resolve the engine never runs.
        assert_eq!(
            fallbacks("commented_when"),
            Vec::new(),
            "a fallback written in a comment is read as one the engine applies"
        );
        assert_eq!(
            fallbacks("quoted_when"),
            Vec::new(),
            "a fallback quoted inside a raw string is read as one the engine applies"
        );
        assert_eq!(
            classify_fallback("|_| DEFAULT_STAGE.to_string())"),
            Fallback::Constant("DEFAULT_STAGE"),
            "the fallback reader does not see through the lazy spelling of a fallback"
        );

        let documented = documented_defaults();
        let sources = resolving_sources();
        assert!(
            sources.len() >= 5,
            "the source walk found only {} files — it is looking in the wrong place, and an \
             empty scan agrees with anything",
            sources.len()
        );

        for entry in &documented {
            let mut sites = 0;
            for ResolvingSource { file, source, code } in &sources {
                for fallback in field_fallbacks(source, code, entry.resolved_as) {
                    sites += 1;
                    match fallback {
                        Fallback::Constant(used) => assert_eq!(
                            used, entry.constant,
                            "{file}: `{}` falls back to `{used}`, but docs/ci.md is held to \
                             `{}` — the page is then checked against a constant the engine no \
                             longer uses",
                            entry.resolved_as, entry.constant
                        ),
                        Fallback::Literal(value) => panic!(
                            "{file}: the fallback `{value}` for `{}` is written into the \
                             resolve itself. docs/ci.md restates that value in its `{}` row, \
                             and a literal has no name for the page to be bound to — give it a \
                             `const DEFAULT_*` and pair it in documented_defaults()",
                            entry.resolved_as, entry.key
                        ),
                        Fallback::Unknown(shape) => panic!(
                            "{file}: `{}` falls back through `unwrap_or{shape}`, a shape that \
                             names no constant — docs/ci.md states `{}` for it, so the value \
                             has to come from somewhere a check can read",
                            entry.resolved_as, entry.value
                        ),
                    }
                }
            }
            assert!(
                sites >= 1,
                "nothing in the engine falls back for `{}`, yet docs/ci.md states `{}` as what \
                 an omitted `{}:` gives you — the page promises a value no resolve produces",
                entry.resolved_as,
                entry.value,
                entry.key
            );
        }
    }

    /// The census, in both directions: a default this engine declares that no
    /// row of the document states, and a row pairing a constant that is gone.
    #[test]
    fn every_default_constant_the_ci_engine_declares_is_stated_in_the_documentation() {
        assert_eq!(
            declared_default_constants(
                "pub const DEFAULT_X: &str = \"1\";\n    const DEFAULT_Y: u8 = 2;\n\
                 pub(crate) const DEFAULT_Z: bool = false;\nconst OTHER: u8 = 4;\n"
            ),
            BTreeSet::from(["DEFAULT_X", "DEFAULT_Y", "DEFAULT_Z"]),
            "the declaration scan does not read `const DEFAULT_*` the way this engine writes \
             them — a private one would escape the census entirely"
        );
        assert!(
            declared_default_constants("#[cfg(test)]\nconst DEFAULT_FIXTURE: u8 = 1;\n")
                .contains("DEFAULT_FIXTURE"),
            "the scan is expected to read any declaration it is given — cutting the test half \
             away is production_code's job, and this pins which of the two does it"
        );
        let production = production_code(
            "const DEFAULT_REAL: u8 = 1;\n#[cfg(test)]\nmod tests {\n    const \
             DEFAULT_FIXTURE: u8 = 2;\n}\nconst DEFAULT_AFTER: u8 = 3;\n",
        );
        assert!(
            !production.contains("DEFAULT_FIXTURE") && production.contains("DEFAULT_AFTER"),
            "production_code must blank the complete test item without hiding later production \
             constants"
        );

        let sources = resolving_sources();
        let mut declared: BTreeSet<String> = BTreeSet::new();
        for entry in &sources {
            declared.extend(
                declared_default_constants(&entry.code)
                    .into_iter()
                    .map(str::to_owned),
            );
        }
        assert!(
            !declared.is_empty(),
            "no `DEFAULT_*` constant found at all — the declaration scan has stopped matching"
        );

        let documented = documented_defaults();
        let paired: BTreeSet<&str> = documented.iter().map(|entry| entry.constant).collect();

        for name in &declared {
            assert!(
                paired.contains(name.as_str())
                    || DEFAULTS_NOT_IN_THE_CI_DOCUMENT
                        .iter()
                        .any(|(excused, _)| excused == name),
                "`{name}` is a built-in default of the CI engine that no row of \
                 documented_defaults() pairs with a key of `.forgekeep-ci.yml` — state it in \
                 the matching `Default` cell of docs/ci.md and pair it, or name it in \
                 DEFAULTS_NOT_IN_THE_CI_DOCUMENT with the reason no author ever meets it"
            );
        }

        // Renaming a paired constant breaks the build — every row reads its
        // value through the path. *Moving* one out of the scanned sources would
        // not: it would simply leave the census, taking its row with it.
        for name in &paired {
            assert!(
                declared.contains(*name),
                "documented_defaults() pairs `{name}`, which none of the scanned sources \
                 declares any more — docs/ci.md would then be held to a constant living \
                 somewhere the census cannot see"
            );
        }

        for (excused, reason) in DEFAULTS_NOT_IN_THE_CI_DOCUMENT {
            assert!(
                !reason.is_empty(),
                "`{excused}` is excused from the census without a reason"
            );
            assert!(
                declared.contains(excused),
                "DEFAULTS_NOT_IN_THE_CI_DOCUMENT still excuses `{excused}`, which this engine \
                 no longer declares — drop the entry so the list keeps meaning something"
            );
        }
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
