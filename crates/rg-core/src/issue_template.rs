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
pub struct IssueConfig {
    #[serde(default = "default_blank_issues_enabled")]
    pub blank_issues_enabled: bool,
    #[serde(default)]
    pub contact_links: Vec<IssueContactLink>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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
        for filename in list_directory(&git, repository_path, &commit_ref, directory)? {
            if !filename.to_ascii_lowercase().ends_with(".md") {
                continue;
            }
            let path = format!("{directory}/{filename}");
            match read_text_blob(&git, repository_path, &commit_ref, &path)
                .and_then(|content| parse_markdown_template(&path, &content))
            {
                Ok(template) => discovery.templates.push(template),
                Err(error) => discovery.errors.push((path, error.to_string())),
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
        let config: IssueConfig =
            serde_yaml::from_str(&content).context("invalid issue template config YAML")?;
        validate_config(&config)?;
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
    let commit_spec = format!("{branch_ref}^{{commit}}");
    let output = git.run(
        &["rev-parse", "--verify", &commit_spec],
        Some(repository_path),
    )?;
    if output.success() {
        Ok(Some(branch_ref))
    } else {
        Ok(None)
    }
}

fn list_directory(
    git: &GitCommandGateway,
    repository_path: &Path,
    git_ref: &str,
    directory: &str,
) -> Result<Vec<String>> {
    let treeish = format!("{git_ref}:{directory}");
    let output = git.run(
        &["ls-tree", "-z", "--name-only", &treeish],
        Some(repository_path),
    )?;
    if !output.success() {
        return Ok(Vec::new());
    }
    let mut names: Vec<String> = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .filter_map(|name| std::str::from_utf8(name).ok().map(str::to_string))
        .filter(|name| !name.contains('/'))
        .collect();
    names.sort();
    Ok(names)
}

fn try_read_text_blob(
    git: &GitCommandGateway,
    repository_path: &Path,
    git_ref: &str,
    path: &str,
) -> Result<Option<String>> {
    let object = format!("{git_ref}:{path}");
    let output = git.run(&["cat-file", "blob", &object], Some(repository_path))?;
    if !output.success() {
        return Ok(None);
    }
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

    let front_matter = metadata
        .and_then(|yaml| serde_yaml::from_str::<FrontMatter>(yaml).ok())
        .unwrap_or_default();
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

fn default_blank_issues_enabled() -> bool {
    true
}

impl Default for IssueConfig {
    fn default() -> Self {
        Self {
            blank_issues_enabled: true,
            contact_links: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_markdown_template, split_front_matter, IssueConfig};

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
}
