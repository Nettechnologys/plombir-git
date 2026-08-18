//! Gitea-compatible Markdown issue and pull-request template discovery.

use anyhow::{Context, Result};
use rg_git::cli_gateway::GitCommandGateway;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MAX_TEMPLATE_SIZE: usize = 1024 * 1024;

const ISSUE_TEMPLATE_DIRS: &[&str] = &[
    "ISSUE_TEMPLATE",
    "issue_template",
    ".gitea/ISSUE_TEMPLATE",
    ".gitea/issue_template",
    ".github/ISSUE_TEMPLATE",
    ".github/issue_template",
    ".gitlab/ISSUE_TEMPLATE",
    ".gitlab/issue_template",
];

const ISSUE_CONFIGS: &[&str] = &[
    ".gitea/ISSUE_TEMPLATE/config.yaml",
    ".gitea/ISSUE_TEMPLATE/config.yml",
    ".gitea/issue_template/config.yaml",
    ".gitea/issue_template/config.yml",
    ".github/ISSUE_TEMPLATE/config.yaml",
    ".github/ISSUE_TEMPLATE/config.yml",
    ".github/issue_template/config.yaml",
    ".github/issue_template/config.yml",
];

const PULL_REQUEST_TEMPLATES: &[&str] = &[
    "PULL_REQUEST_TEMPLATE.md",
    "pull_request_template.md",
    ".gitea/PULL_REQUEST_TEMPLATE.md",
    ".gitea/pull_request_template.md",
    ".github/PULL_REQUEST_TEMPLATE.md",
    ".github/pull_request_template.md",
];

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IssueTemplate {
    pub name: String,
    pub title: String,
    pub about: String,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub content: String,
    pub file_name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IssueConfig {
    #[serde(default = "default_blank_issues_enabled")]
    pub blank_issues_enabled: bool,
    #[serde(default)]
    pub contact_links: Vec<IssueContactLink>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IssueContactLink {
    pub name: String,
    pub url: String,
    pub about: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PullRequestTemplate {
    pub content: String,
    pub file_name: String,
}

#[derive(Debug, Default)]
pub struct IssueTemplateDiscovery {
    pub templates: Vec<IssueTemplate>,
    pub errors: Vec<(String, String)>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrontMatter {
    #[serde(default)]
    name: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    about: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    labels: serde_yaml::Value,
    #[serde(default)]
    assignees: serde_yaml::Value,
    #[serde(default, rename = "ref")]
    git_ref: String,
}

pub fn discover_issue_templates(
    repository_path: &Path,
    default_branch: &str,
) -> Result<IssueTemplateDiscovery> {
    let git = GitCommandGateway::new()?;
    let Some(commit_ref) = verified_branch_ref(&git, repository_path, default_branch)? else {
        return Ok(IssueTemplateDiscovery::default());
    };
    let mut discovery = IssueTemplateDiscovery::default();

    for directory in ISSUE_TEMPLATE_DIRS {
        let listing = list_directory(&git, repository_path, &commit_ref, directory)?;
        // An entry we could not even name is a template we failed to read, not
        // one that is absent — it joins the same diagnostics as a template that
        // failed to parse instead of vanishing from a confident `200`.
        discovery.errors.extend(listing.errors);
        for filename in listing.names {
            let path = format!("{directory}/{filename}");
            match read_text_blob(&git, repository_path, &commit_ref, &path)
                .and_then(|content| parse_markdown_template(&path, &content))
            {
                Ok(template) => discovery.templates.push(template),
                // Travels on into both the log and the API response, so the
                // chain has to survive the trip to `String` (card_a997f30c142c).
                Err(error) => discovery.errors.push((path, format!("{error:#}"))),
            }
        }
    }

    Ok(discovery)
}

pub fn read_issue_config(repository_path: &Path, default_branch: &str) -> Result<IssueConfig> {
    let git = GitCommandGateway::new()?;
    let Some(commit_ref) = verified_branch_ref(&git, repository_path, default_branch)? else {
        return Ok(IssueConfig::default());
    };
    for candidate in ISSUE_CONFIGS {
        let Some(content) = try_read_text_blob(&git, repository_path, &commit_ref, candidate)?
        else {
            continue;
        };
        let config: IssueConfig = serde_yaml::from_str(&content)
            .context(crate::error::InvalidRequest::new(
                "invalid issue template config",
            ))
            .context("invalid issue template config YAML")?;
        validate_config(&config)
            .context(crate::error::InvalidRequest::new(
                "invalid issue template config",
            ))
            .context("invalid issue template config contents")?;
        return Ok(config);
    }
    Ok(IssueConfig::default())
}

pub fn read_pull_request_template(
    repository_path: &Path,
    default_branch: &str,
) -> Result<Option<PullRequestTemplate>> {
    let git = GitCommandGateway::new()?;
    let Some(commit_ref) = verified_branch_ref(&git, repository_path, default_branch)? else {
        return Ok(None);
    };
    for candidate in PULL_REQUEST_TEMPLATES {
        if let Some(content) = try_read_text_blob(&git, repository_path, &commit_ref, candidate)? {
            return Ok(Some(PullRequestTemplate {
                content,
                file_name: (*candidate).to_string(),
            }));
        }
    }
    Ok(None)
}

fn verified_branch_ref(
    git: &GitCommandGateway,
    repository_path: &Path,
    default_branch: &str,
) -> Result<Option<String>> {
    let branch_ref = format!("refs/heads/{default_branch}");
    let output = git.run(
        &["show-ref", "--verify", "--quiet", &branch_ref],
        Some(repository_path),
    )?;
    if !output.success() {
        if output.status.code() == Some(1) {
            return Ok(None);
        }
        output.ensure_success()?;
    }

    let commit_spec = format!("{branch_ref}^{{commit}}");
    let output = git.run(
        &["rev-parse", "--verify", "--quiet", &commit_spec],
        Some(repository_path),
    )?;
    output.ensure_success()?;
    Ok(Some(branch_ref))
}

/// The Markdown templates directly inside one directory, plus the entries that
/// could not be named.
#[derive(Debug, Default)]
struct DirectoryListing {
    names: Vec<String>,
    /// `(path, reason)`, ready to join `IssueTemplateDiscovery::errors`.
    errors: Vec<(String, String)>,
}

/// List the Markdown issue templates directly inside `directory`.
///
/// Git path bytes are not required to be UTF-8, and an entry we cannot decode
/// is not an entry that is not there: the previous `from_utf8(..).ok()` dropped
/// it, and the caller then reported the shortened list as the complete one. Such
/// an entry is reported instead, named with a lossy rendering — recognizable to
/// an operator without pretending the bytes were ever valid UTF-8.
fn list_directory(
    git: &GitCommandGateway,
    repository_path: &Path,
    git_ref: &str,
    directory: &str,
) -> Result<DirectoryListing> {
    let pathspec = format!("{directory}/");
    let output = git.run(
        &["ls-tree", "-rz", "--name-only", git_ref, "--", &pathspec],
        Some(repository_path),
    )?;
    output.ensure_success()?;
    let prefix = format!("{directory}/");
    let mut listing = DirectoryListing::default();
    for entry in output.stdout.split(|byte| *byte == 0) {
        let Some(relative) = entry.strip_prefix(prefix.as_bytes()) else {
            continue;
        };
        // `-r` also walks nested directories; only this directory's own files
        // are templates. The `.md` gate runs on the raw bytes so an undecodable
        // name is still classified before it is reported.
        if relative.is_empty()
            || relative.contains(&b'/')
            || !relative.to_ascii_lowercase().ends_with(b".md")
        {
            continue;
        }
        match std::str::from_utf8(relative) {
            Ok(name) => listing.names.push(name.to_string()),
            Err(error) => listing.errors.push((
                format!("{prefix}{}", String::from_utf8_lossy(relative)),
                format!("template file name is not valid UTF-8: {error}"),
            )),
        }
    }
    listing.names.sort();
    Ok(listing)
}

fn try_read_text_blob(
    git: &GitCommandGateway,
    repository_path: &Path,
    git_ref: &str,
    path: &str,
) -> Result<Option<String>> {
    let listing = git.run(
        &["ls-tree", "-z", "--name-only", git_ref, "--", path],
        Some(repository_path),
    )?;
    listing.ensure_success()?;
    if !listing
        .stdout
        .split(|byte| *byte == 0)
        .any(|name| name == path.as_bytes())
    {
        return Ok(None);
    }

    let object = format!("{git_ref}:{path}");
    let output = git.run(&["cat-file", "blob", &object], Some(repository_path))?;
    output.ensure_success()?;
    decode_template_content(path, output.stdout).map(Some)
}

fn read_text_blob(
    git: &GitCommandGateway,
    repository_path: &Path,
    git_ref: &str,
    path: &str,
) -> Result<String> {
    try_read_text_blob(git, repository_path, git_ref, path)?
        .ok_or_else(|| anyhow::anyhow!("template disappeared while reading"))
}

fn decode_template_content(path: &str, data: Vec<u8>) -> Result<String> {
    if data.len() > MAX_TEMPLATE_SIZE {
        anyhow::bail!("template is larger than {MAX_TEMPLATE_SIZE} bytes");
    }
    String::from_utf8(data).with_context(|| format!("template is not UTF-8: {path}"))
}

fn parse_markdown_template(path: &str, source: &str) -> Result<IssueTemplate> {
    let (metadata, body) = split_front_matter(source);
    let filename = PathBuf::from(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_string();
    let fallback_about = ellipsis(body.trim(), 80);

    let front_matter = match metadata {
        Some(yaml) => serde_yaml::from_str::<FrontMatter>(yaml)
            .context("invalid issue template front matter")?,
        None => FrontMatter::default(),
    };
    let name = if front_matter.name.trim().is_empty() {
        filename.clone()
    } else {
        front_matter.name.trim().to_string()
    };
    let about = if !front_matter.about.trim().is_empty() {
        front_matter.about.trim().to_string()
    } else if !front_matter.description.trim().is_empty() {
        front_matter.description.trim().to_string()
    } else {
        fallback_about
    };

    Ok(IssueTemplate {
        name,
        title: front_matter.title,
        about,
        labels: yaml_string_list(&front_matter.labels)?,
        assignees: yaml_string_list(&front_matter.assignees)?,
        git_ref: front_matter.git_ref,
        content: body.to_string(),
        file_name: path.to_string(),
    })
}

fn split_front_matter(source: &str) -> (Option<&str>, &str) {
    let Some(first_newline) = source.find('\n') else {
        return (None, source);
    };
    let first = source[..first_newline].trim_end_matches('\r').trim();
    if first.len() < 3 || !first.bytes().all(|byte| byte == b'-') {
        return (None, source);
    }
    let remainder = &source[first_newline + 1..];
    let mut offset = 0;
    for line in remainder.split_inclusive('\n') {
        let candidate = line.trim_end_matches(['\r', '\n']).trim();
        if candidate.len() >= 3 && candidate.bytes().all(|byte| byte == b'-') {
            let metadata = &remainder[..offset];
            let body = &remainder[offset + line.len()..];
            return (Some(metadata), body);
        }
        offset += line.len();
    }
    (None, source)
}

fn yaml_string_list(value: &serde_yaml::Value) -> Result<Vec<String>> {
    match value {
        serde_yaml::Value::Null => Ok(Vec::new()),
        serde_yaml::Value::String(value) => Ok(value
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .collect()),
        serde_yaml::Value::Sequence(values) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| anyhow::anyhow!("labels/assignees entries must be strings"))
            })
            .collect(),
        _ => anyhow::bail!("labels/assignees must be a string or string list"),
    }
}

fn validate_config(config: &IssueConfig) -> Result<()> {
    for (index, link) in config.contact_links.iter().enumerate() {
        if link.name.trim().is_empty() || link.about.trim().is_empty() {
            anyhow::bail!("contact link {} requires name and about", index + 1);
        }
        let url = reqwest::Url::parse(&link.url)
            .with_context(|| format!("invalid contact link URL at position {}", index + 1))?;
        if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
            anyhow::bail!(
                "contact link {} must use an absolute HTTP(S) URL",
                index + 1
            );
        }
    }
    Ok(())
}

fn ellipsis(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

/// What a `config.yml` that says nothing about blank issues means: the chooser
/// still offers "open a blank issue".
///
/// `docs/issue-templates.md` states this value in the `Default` column of its
/// config table, and the value stood in the code twice — once in the serde
/// default and once in `Default::default` — with nothing but memory holding the
/// three copies equal.
const DEFAULT_BLANK_ISSUES_ENABLED: bool = true;

fn default_blank_issues_enabled() -> bool {
    DEFAULT_BLANK_ISSUES_ENABLED
}

impl Default for IssueConfig {
    fn default() -> Self {
        Self {
            blank_issues_enabled: DEFAULT_BLANK_ISSUES_ENABLED,
            contact_links: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        discover_issue_templates, parse_markdown_template, read_issue_config, split_front_matter,
        validate_config, IssueConfig, ISSUE_CONFIGS, ISSUE_TEMPLATE_DIRS, MAX_TEMPLATE_SIZE,
        PULL_REQUEST_TEMPLATES,
    };
    use std::collections::BTreeSet;

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    /// The document an author of a *repository* — not an operator of this
    /// server — reads to learn this format. Resolved at compile time, so a
    /// moved or renamed document breaks the build instead of silently skipping
    /// the checks below, and editing it re-runs them.
    const TEMPLATE_DOCUMENTATION: (&str, &str) = (
        "docs/issue-templates.md",
        include_str!("../../../docs/issue-templates.md"),
    );

    /// The production view of this file. The inventory below is read off the
    /// declaration itself, with complete test items blanked so a key that exists
    /// only in a fixture cannot pass for a key of the model.
    fn production_source() -> String {
        rust_source::production_rust_source(include_str!("issue_template.rs"))
    }

    /// One field of a serde struct, as the reader of the YAML sees it.
    struct SerdeField {
        /// The Rust field name, for error messages.
        field: String,
        /// The key an author actually writes — `#[serde(rename = "…")]` wins.
        key: String,
        /// The type text, so the walk below can find the next model struct.
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
        let body = struct_body(source, code, type_name).unwrap_or_else(|| {
            panic!("{type_name} declaration must be present in issue_template.rs")
        });

        let mut fields = Vec::new();
        let mut attributes = String::new();

        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() {
                attributes.clear();
                continue;
            }
            if line.starts_with("//") {
                continue;
            }
            if line.starts_with("#[") {
                attributes.push_str(line);
                continue;
            }
            let declaration = line
                .strip_prefix("pub(crate) ")
                .or_else(|| line.strip_prefix("pub "))
                .unwrap_or(line);
            let Some((field, type_text)) = declaration.split_once(':') else {
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

    /// Every struct a repository's own files are parsed into: the front matter
    /// of a Markdown template and the chooser configuration, plus whatever they
    /// reach.
    ///
    /// Derived rather than hand-listed for the same reason as the fields: a new
    /// block hung off the configuration joins the documentation contract by
    /// being reachable, not by being remembered.
    fn template_model_types(source: &str) -> Vec<String> {
        let code = rust_source::production_rust_code_only(source);
        let mut reachable = vec!["FrontMatter".to_owned(), "IssueConfig".to_owned()];
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
                    if struct_body(source, &code, candidate).is_some()
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
    fn template_model_inventory_ignores_declaration_shaped_rust_data() {
        let source = r####"
// struct FrontMatter {
//     comment_only: bool,
// }
const NORMAL_DECOY: &str = "struct FrontMatter {
    normal_only: bool,
}
";
const RAW_DECOY: &str = r#"struct IssueConfig {
    raw_only: bool,
}"#;
const BYTE_DECOY: &[u8] = b"struct IssueContactLink {
    byte_only: bool,
}
";

struct FrontMatter {
    #[serde(rename = "display_name")]
    name: String,
    config: IssueConfig,
}

struct IssueConfig {
    contact_links: Vec<IssueContactLink>,
}

struct IssueContactLink {
    url: String,
}
"####;

        assert_eq!(
            serde_fields(source, "FrontMatter")
                .into_iter()
                .map(|field| field.key)
                .collect::<Vec<_>>(),
            ["display_name", "config"]
        );
        assert_eq!(
            template_model_types(source),
            ["FrontMatter", "IssueConfig", "IssueContactLink"]
        );
    }

    /// The fenced code blocks of a Markdown document, as `(info string, line
    /// number of the block's first content line, block body)`.
    fn code_blocks(name: &str, content: &str) -> Vec<(String, usize, String)> {
        let mut blocks = Vec::new();
        let mut body: Vec<&str> = Vec::new();
        let mut info = String::new();
        let mut start = 0usize;
        let mut inside = false;

        for (index, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if inside {
                if trimmed == "```" {
                    blocks.push((info.clone(), start, body.join("\n")));
                    body.clear();
                    inside = false;
                } else {
                    body.push(line);
                }
            } else if let Some(language) = trimmed.strip_prefix("```") {
                inside = true;
                info = language.to_owned();
                start = index + 2;
            }
        }

        assert!(
            !inside,
            "{name}:{start}: a ``` block is never closed — the extractor reads \
             the rest of the document as one example"
        );
        blocks
    }

    /// The lines of the fenced block that follows a marker comment.
    ///
    /// The marker, rather than the block's position, is what ties an inventory
    /// in the document to a list in this file: inserting a paragraph must not
    /// silently re-point a check at somebody else's example.
    fn inventory_after(name: &str, content: &str, marker: &str) -> Vec<String> {
        let marker_line = content
            .lines()
            .position(|line| line.trim() == marker)
            .unwrap_or_else(|| {
                panic!(
                    "{name}: the marker `{marker}` is gone, so the inventory it introduced can no \
                     longer be checked against this file — restore it above the list"
                )
            });
        let (_, _, body) = code_blocks(name, content)
            .into_iter()
            .find(|(_, start, _)| *start > marker_line + 1)
            .unwrap_or_else(|| panic!("{name}: no fenced block follows the marker `{marker}`"));
        body.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// Every mapping key a YAML document shows, at any depth.
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

    /// The keys the document actually shows an author writing.
    ///
    /// Only the metadata regions count: the front matter of a ```markdown
    /// example (split by the very function that splits the reader's own file)
    /// and the whole of a ```yaml one. Prose inside a template body is not
    /// documentation of a key — otherwise a line like `Version: 1.0` in an
    /// example would excuse a `version` key nobody ever described.
    fn documented_keys(name: &str, content: &str) -> BTreeSet<String> {
        let mut keys = BTreeSet::new();
        for (language, _, body) in code_blocks(name, content) {
            let metadata = match language.as_str() {
                "yaml" => body.clone(),
                "markdown" => split_front_matter(&body).0.unwrap_or("").to_owned(),
                _ => continue,
            };
            keys.extend(yaml_keys(&metadata).into_iter().map(str::to_owned));
        }
        keys
    }

    /// The examples an author copies have to be files this server accepts.
    ///
    /// `FrontMatter`, `IssueConfig` and `IssueContactLink` are all
    /// `deny_unknown_fields`, so a key that drifted in the documentation is not
    /// cosmetic: pasting it produces a template that never appears in the
    /// chooser, and the author has nowhere else to look the right name up.
    #[test]
    fn every_example_in_the_template_documentation_is_a_file_the_server_accepts() {
        let (name, content) = TEMPLATE_DOCUMENTATION;
        let mut templates = 0;
        let mut configs = 0;

        for (language, line, body) in code_blocks(name, content) {
            match language.as_str() {
                "markdown" => {
                    // A block that opens front matter and fails to close it
                    // parses perfectly well — as a template with no metadata at
                    // all. Left unchecked, an example could lose every key it
                    // claims to teach and still pass the parse below.
                    if body.starts_with("---") {
                        assert!(
                            split_front_matter(&body).0.is_some(),
                            "{name}:{line}: this ```markdown block opens a front-matter block and \
                             never closes it, so every key it shows is read as body text"
                        );
                    }
                    parse_markdown_template(".gitea/ISSUE_TEMPLATE/example.md", &body)
                        .unwrap_or_else(|error| {
                            panic!(
                                "{name}:{line}: this ```markdown block is what a reader commits to \
                                 their own repository, and this server refuses it: {error:#}"
                            )
                        });
                    templates += 1;
                }
                "yaml" => {
                    let config: IssueConfig = serde_yaml::from_str(&body).unwrap_or_else(|error| {
                        panic!(
                            "{name}:{line}: this ```yaml block is not a valid issue chooser \
                                 `config.yml`: {error}"
                        )
                    });
                    // Parsing is only half the gate: a contact link with a
                    // relative URL or a blank name is refused after parsing, by
                    // the same function that judges the reader's own file.
                    validate_config(&config).unwrap_or_else(|error| {
                        panic!(
                            "{name}:{line}: this ```yaml block parses, but the server rejects its \
                             contents: {error:#}"
                        )
                    });
                    configs += 1;
                }
                _ => {}
            }
        }

        // Floors, not counts: they fail loudly if the fence scanner ever stops
        // matching and the test quietly checks nothing.
        assert!(
            templates >= 2,
            "only {templates} ```markdown examples found in {name} — the scanner has stopped \
             matching them"
        );
        assert!(
            configs >= 1,
            "no ```yaml example found in {name} — the scanner has stopped matching them"
        );
    }

    /// The mirror of the check above: that one asks that everything the
    /// document shows is real, this asks that everything real is shown.
    ///
    /// A key nobody can discover is worse here than in the server's own config.
    /// These files live in the *author's* repository, every block that takes
    /// them is `deny_unknown_fields`, and there is no half-working middle: the
    /// name is either found in this document or guessed, and a guess is a
    /// template that silently never appears.
    #[test]
    fn every_key_the_template_model_accepts_is_shown_in_the_documentation() {
        let (name, content) = TEMPLATE_DOCUMENTATION;
        let documented = documented_keys(name, content);
        let source = production_source();
        let model_types = template_model_types(&source);
        let mut checked = 0;

        assert!(
            model_types.len() >= 3,
            "only {} serde model types read out of issue_template.rs — the declaration scanner \
             has stopped following nested models",
            model_types.len()
        );

        for type_name in model_types {
            for field in serde_fields(&source, &type_name) {
                if field.skipped {
                    continue;
                }
                assert!(
                    !field.flattened,
                    "`{}` of {type_name} is flattened, so it has no key of its own — say in {name} \
                     what an author writes in its place before this check can pass",
                    field.field
                );
                assert!(
                    documented.contains(&field.key),
                    "no example in {name} shows `{}` of {type_name}, so the only way to learn the \
                     key exists is to read issue_template.rs — and `deny_unknown_fields` means an \
                     author who guesses the name gets a template that is dropped without a word",
                    field.key
                );
                checked += 1;
            }
        }

        // The floor guards the scanner, not the model: a declaration format it
        // stopped recognising would leave this test asserting nothing.
        assert!(
            checked >= 9,
            "only {checked} keys read off the model — the declaration scanner has stopped matching"
        );
    }

    /// The one value that document promises for a key an author leaves out.
    ///
    /// The two checks above read *names*: every key shown is real, every real
    /// key is shown. The `Default` column beside them was read by neither, and
    /// the value in it — whether the chooser still offers a blank issue — lived
    /// in the code twice over, as a bare `true` in the serde default and
    /// another in `Default::default`. Three copies, no name, and the only thing
    /// keeping them equal was that nobody had edited one.
    ///
    /// The same shape as the four defaults of `docs/ci.md`
    /// (`rg-ci/src/config.rs`), on a table one twentieth the size: a value in
    /// backticks has to be a named constant, prose has to be registered as
    /// prose, and the behaviour is asked of the model rather than read off its
    /// source.
    #[test]
    fn every_default_the_template_documentation_states_is_the_constant_that_produces_it() {
        /// The header of the one table of this document that has a `Default`
        /// column. The `contact_links` table below it has none.
        const DEFAULT_TABLE_HEADER: &str = "| Key | Type | Default | Meaning |";

        /// `Default` cells that describe a behaviour instead of naming a value,
        /// with the reason there is nothing to name. A phrase outside this list
        /// has to be added deliberately, which is what stops "none" quietly
        /// growing into a value nothing holds.
        const PROSE_DEFAULTS: [(&str, &str); 5] = [
            (
                "empty",
                "the absence of a string or a list is not a value standing in for one — there \
                 is no constant behind `String::new()` or `Vec::new()`",
            ),
            (
                "none",
                "the key is absent and nothing is applied in its place",
            ),
            (
                "the file name",
                "taken from the template's own path at parse time, so it differs per file and \
                 no constant could state it",
            ),
            (
                "see below",
                "`about` falls back to the body's excerpt, which is the template's own text — \
                 the paragraph under this table is the description, and there is no value to \
                 put here",
            ),
            (
                "repository default",
                "the repository's default branch, a per-repository value this crate never \
                 chooses",
            ),
        ];

        /// The value a `Default` cell states, when it states one and nothing
        /// else. A sentence carrying an incidental code span is not one of
        /// these.
        fn stated_value(cell: &str) -> Option<&str> {
            cell.strip_prefix('`')
                .and_then(|rest| rest.strip_suffix('`'))
                .filter(|value| !value.contains('`'))
        }

        assert_eq!(
            stated_value("`true`"),
            Some("true"),
            "the cell reader does not recognise a value the way the table states one"
        );
        assert_eq!(
            stated_value("either `true` or `false`"),
            None,
            "the cell reader takes a code span out of a sentence for the whole default, so \
             prose would pass for a value"
        );

        let (name, content) = TEMPLATE_DOCUMENTATION;
        // The key spelling is written out — no rule derives
        // `DEFAULT_BLANK_ISSUES_ENABLED` from `blank_issues_enabled` — but the
        // value is read *from* the constant, so renaming it breaks the build
        // and changing it fails this check.
        let paired = [(
            "blank_issues_enabled",
            "DEFAULT_BLANK_ISSUES_ENABLED",
            super::DEFAULT_BLANK_ISSUES_ENABLED.to_string(),
        )];
        let mut seen = BTreeSet::new();
        let mut prose_used = BTreeSet::new();
        let mut inside = false;

        for (index, line) in content.lines().enumerate() {
            let line = line.trim();
            if line == DEFAULT_TABLE_HEADER {
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
            if cells.iter().all(|cell| {
                cell.trim()
                    .chars()
                    .all(|c| c == '-' || c == ':' || c.is_whitespace())
            }) {
                continue;
            }
            let key = cells[1].trim().trim_matches('`');
            let cell = cells[3].trim();

            match stated_value(cell) {
                Some(shown) => {
                    let (_, konst, value) = paired
                        .iter()
                        .find(|(paired_key, _, _)| *paired_key == key)
                        .unwrap_or_else(|| {
                            panic!(
                                "{name}:{}: the `{key}` row states `{shown}` as the value an \
                                 omitted key takes, and nothing binds that cell to the code \
                                 that produces it — give the fallback a `const DEFAULT_*` and \
                                 pair it here, or say in prose what the absence does",
                                index + 1
                            )
                        });
                    assert_eq!(
                        shown,
                        value,
                        "{name}:{}: the `{key}` row states `{shown}`, but `{konst}` — what the \
                         parser actually falls back to — is `{value}`. A maintainer who omits \
                         the key gets the second and reads the first",
                        index + 1
                    );
                    seen.insert(key.to_owned());
                }
                None => {
                    let (_, reason) = PROSE_DEFAULTS
                        .iter()
                        .find(|(phrase, _)| *phrase == cell)
                        .unwrap_or_else(|| {
                            panic!(
                                "{name}:{}: the `Default` cell of the `{key}` row reads \
                                 {cell:?}, which is neither a backticked value nor one of the \
                                 phrases this document uses for \"there is no value\" — state \
                                 the value and pair it with its constant, or add the phrase to \
                                 PROSE_DEFAULTS with the reason nothing produces one",
                                index + 1
                            )
                        });
                    assert!(
                        !reason.is_empty(),
                        "a prose default excused without a reason"
                    );
                    prose_used.insert(cell.to_owned());
                }
            }
        }

        for (key, konst, _) in &paired {
            assert!(
                seen.contains(*key),
                "`{konst}` is bound to the `{key}` row of {name}, and no row states a value for \
                 it any more — the constant is held to a page that stopped promising anything"
            );
        }
        for (phrase, _) in PROSE_DEFAULTS {
            assert!(
                prose_used.contains(phrase),
                "PROSE_DEFAULTS still excuses {phrase:?}, which no `Default` cell of {name} \
                 reads any more — drop the entry so the list keeps meaning something"
            );
        }

        // The census, so the next default cannot arrive unstated: every
        // `const DEFAULT_*` this format declares is one an author meets, and
        // has to be paired with the row that states it.
        let source = production_source();
        let declared: BTreeSet<&str> = source
            .lines()
            .map(str::trim_start)
            .map(|line| line.strip_prefix("pub(crate) ").unwrap_or(line))
            .map(|line| line.strip_prefix("pub ").unwrap_or(line))
            .filter_map(|line| line.strip_prefix("const "))
            .filter_map(|rest| rest.split_once(':'))
            .map(|(constant, _)| constant.trim())
            .filter(|constant| constant.starts_with("DEFAULT_"))
            .collect();
        for constant in &declared {
            assert!(
                paired.iter().any(|(_, name, _)| name == constant),
                "`{constant}` is a built-in default of this format that no row of {name} is \
                 paired with — state it in the `Default` cell of the key it produces, or say \
                 here why no author ever meets it"
            );
        }
        for (_, constant, _) in &paired {
            assert!(
                declared.contains(constant),
                "the pairing above binds `{constant}`, which issue_template.rs no longer \
                 declares — the page would then be held to a constant living somewhere this \
                 check cannot see"
            );
        }

        // The row promises a behaviour, so the behaviour is what is asked —
        // both ways in, since the two used to carry their own copy of the value.
        let omitted: IssueConfig =
            serde_yaml::from_str("contact_links: []").expect("a config with no blank-issue key");
        assert_eq!(
            omitted.blank_issues_enabled,
            super::DEFAULT_BLANK_ISSUES_ENABLED,
            "a `config.yml` that omits `blank_issues_enabled` does not load as \
             `DEFAULT_BLANK_ISSUES_ENABLED` — the serde default and the constant the page is \
             held to have come apart"
        );
        assert_eq!(
            IssueConfig::default().blank_issues_enabled,
            super::DEFAULT_BLANK_ISSUES_ENABLED,
            "`IssueConfig::default()` and the serde default disagree about blank issues — a \
             repository with no `config.yml` at all then gets the other answer, and the page \
             states only one"
        );
    }

    /// The paths are the other half of the contract, and the larger half: three
    /// lists, twenty-two entries, none of them guessable and none of them
    /// visible anywhere in the product. Compared as sets in both directions —
    /// a path this server stopped opening must not stay in the document any
    /// more than a path it opens may be missing from it.
    #[test]
    fn every_path_a_template_may_live_at_is_listed_in_the_documentation() {
        let (name, content) = TEMPLATE_DOCUMENTATION;

        for (marker, paths, what) in [
            (
                "<!-- inventory: issue-template-directories -->",
                ISSUE_TEMPLATE_DIRS,
                "directory issue templates are read from",
            ),
            (
                "<!-- inventory: issue-config-paths -->",
                ISSUE_CONFIGS,
                "path the issue chooser configuration is read from",
            ),
            (
                "<!-- inventory: pull-request-templates -->",
                PULL_REQUEST_TEMPLATES,
                "path the pull-request template is read from",
            ),
        ] {
            let documented: BTreeSet<String> =
                inventory_after(name, content, marker).into_iter().collect();
            let opened: BTreeSet<String> = paths.iter().map(|path| (*path).to_owned()).collect();

            let undocumented: Vec<&String> = opened.difference(&documented).collect();
            assert!(
                undocumented.is_empty(),
                "{name}: each of {undocumented:?} is a {what}, and the inventory under `{marker}` \
                 does not list it — an author has no way to learn the path exists"
            );

            let abandoned: Vec<&String> = documented.difference(&opened).collect();
            assert!(
                abandoned.is_empty(),
                "{name}: the inventory under `{marker}` offers each of {abandoned:?} as a {what}, \
                 but nothing opens it any more — a file committed there is read by nobody"
            );
        }

        // The ceiling on a template's size is a rule an author only meets by
        // hitting it, so the document states the number and this ties it to the
        // constant that enforces it.
        assert!(
            content.contains(&format!("{MAX_TEMPLATE_SIZE} bytes")),
            "{name} does not state the {MAX_TEMPLATE_SIZE}-byte ceiling that decides whether a \
             template is served or reported as broken"
        );
    }

    fn committed_repository(files: &[(&str, &str)]) -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let repository = directory.path().join("templates");
        let git = rg_git::cli_gateway::GitCommandGateway::new().unwrap();
        git.run_or_bail(
            &["init", "-q", "-b", "main", repository.to_str().unwrap()],
            None,
        )
        .unwrap();
        for arguments in [
            vec!["config", "user.email", "templates@example.com"],
            vec!["config", "user.name", "Templates"],
        ] {
            git.run_or_bail(&arguments, Some(&repository)).unwrap();
        }
        for (path, content) in files {
            let path = repository.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        if !files.is_empty() {
            for arguments in [vec!["add", "-A"], vec!["commit", "-qm", "templates"]] {
                git.run_or_bail(&arguments, Some(&repository)).unwrap();
            }
        }
        (directory, repository)
    }

    #[test]
    fn parses_gitea_markdown_front_matter() {
        let template = parse_markdown_template(
            ".gitea/ISSUE_TEMPLATE/bug.md",
            "----\nname: Bug report\ndescription: Something broke\ntitle: '[Bug] '\nlabels: bug, triage\nassignees: [alice, bob]\nref: release\n----\n## Steps\n",
        )
        .unwrap();
        assert_eq!(template.name, "Bug report");
        assert_eq!(template.about, "Something broke");
        assert_eq!(template.labels, ["bug", "triage"]);
        assert_eq!(template.assignees, ["alice", "bob"]);
        assert_eq!(template.git_ref, "release");
        assert_eq!(template.content, "## Steps\n");
    }

    #[test]
    fn markdown_without_metadata_uses_filename_and_excerpt() {
        let template =
            parse_markdown_template("ISSUE_TEMPLATE/question.md", "Tell us more").unwrap();
        assert_eq!(template.name, "question.md");
        assert_eq!(template.about, "Tell us more");
        assert_eq!(template.content, "Tell us more");
    }

    #[test]
    fn unterminated_front_matter_is_plain_markdown() {
        let source = "---\n# Heading\n";
        assert_eq!(split_front_matter(source), (None, source));
    }

    #[test]
    fn issue_config_defaults_to_blank_enabled() {
        let config: IssueConfig = serde_yaml::from_str("contact_links: []").unwrap();
        assert!(config.blank_issues_enabled);
    }

    #[test]
    fn issue_config_rejects_unknown_keys_instead_of_defaulting_them() {
        let (_directory, repository) = committed_repository(&[(
            ".gitea/ISSUE_TEMPLATE/config.yml",
            "blank_issue_enabled: false\n",
        )]);

        let error = read_issue_config(&repository, "main").unwrap_err();
        let reason = format!("{error:#}");
        assert!(reason.contains("blank_issue_enabled"), "{reason}");
        assert!(reason.contains("unknown field"), "{reason}");

        let config: IssueConfig = serde_yaml::from_str("blank_issues_enabled: false\n").unwrap();
        assert!(!config.blank_issues_enabled);

        let nested_error = serde_yaml::from_str::<IssueConfig>(
            "contact_links:\n  - name: Support\n    url: https://example.com/support\n    about: Ask here\n    icon: help\n",
        )
        .unwrap_err()
        .to_string();
        assert!(nested_error.contains("icon"), "{nested_error}");
    }

    #[test]
    fn malformed_and_unknown_front_matter_are_discovery_errors() {
        let (_directory, repository) = committed_repository(&[
            (
                ".gitea/ISSUE_TEMPLATE/malformed.md",
                "---\nname: [\n---\nBroken body\n",
            ),
            (
                ".gitea/ISSUE_TEMPLATE/typo.md",
                "---\nname: Typo\nabout: Misspelled labels\nlables: [bug]\n---\nTypo body\n",
            ),
            (
                ".gitea/ISSUE_TEMPLATE/valid.md",
                "---\nname: Valid\nabout: Supported metadata\nlabels: [bug]\n---\nValid body\n",
            ),
        ]);

        let discovery = discover_issue_templates(&repository, "main").unwrap();

        assert_eq!(discovery.templates.len(), 1);
        assert_eq!(discovery.templates[0].name, "Valid");
        assert_eq!(discovery.templates[0].labels, ["bug"]);
        assert_eq!(discovery.templates[0].content, "Valid body\n");
        for (file, key) in [("malformed.md", "invalid"), ("typo.md", "lables")] {
            let (path, reason) = discovery
                .errors
                .iter()
                .find(|(path, _)| path.ends_with(file))
                .unwrap_or_else(|| panic!("missing diagnostic for {file}: {:?}", discovery.errors));
            assert_eq!(path, &format!(".gitea/ISSUE_TEMPLATE/{file}"));
            assert!(
                reason.contains("invalid issue template front matter"),
                "{reason}"
            );
            assert!(reason.contains(key), "{reason}");
        }
    }

    /// Git accepts path bytes that are not UTF-8. Such a template cannot be
    /// served — but the listing must not answer "these are all of them" after
    /// quietly leaving one out. It joins `errors`, the same channel a template
    /// that fails to parse uses, while the healthy ones are unaffected.
    #[cfg(unix)]
    #[test]
    fn an_undecodable_template_name_is_reported_instead_of_dropped() {
        use std::os::unix::ffi::OsStrExt;

        let (_directory, repository) = committed_repository(&[]);
        let git = rg_git::cli_gateway::GitCommandGateway::new().unwrap();

        let template_directory = repository.join(".gitea/ISSUE_TEMPLATE");
        std::fs::create_dir_all(&template_directory).unwrap();
        std::fs::write(
            template_directory.join("bug.md"),
            "---\nname: Bug report\n---\n\n## Steps\n",
        )
        .unwrap();
        // 0xFF can never start a UTF-8 sequence, so this name is undecodable
        // while remaining a perfectly ordinary path to Git.
        let undecodable = std::ffi::OsStr::from_bytes(b"br\xffken.md");
        std::fs::write(template_directory.join(undecodable), "# Broken name\n").unwrap();

        for arguments in [vec!["add", "-A"], vec!["commit", "-qm", "templates"]] {
            git.run_or_bail(&arguments, Some(&repository)).unwrap();
        }

        let discovery = discover_issue_templates(&repository, "main").unwrap();

        assert_eq!(
            discovery
                .templates
                .iter()
                .map(|template| template.file_name.as_str())
                .collect::<Vec<_>>(),
            [".gitea/ISSUE_TEMPLATE/bug.md"],
            "a healthy template keeps its payload"
        );
        let (path, reason) = discovery
            .errors
            .iter()
            .find(|(path, _)| path.contains("ken.md"))
            .expect("the undecodable name must be reported, not dropped");
        assert!(path.starts_with(".gitea/ISSUE_TEMPLATE/"), "{path}");
        assert!(reason.contains("not valid UTF-8"), "{reason}");
    }
}
