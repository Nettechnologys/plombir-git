//! Repository creation templates — .gitignore, LICENSE, README, default labels.
//!
//! These templates are embedded at compile time for zero-disk dependency.
//! Inspired by Gitea's template system with a curated subset of common choices.

use std::collections::BTreeMap;

// ── Template metadata types ───────────────────────────────────────────

/// A named template item (e.g., "Go", "MIT License").
#[derive(Debug, Clone, serde::Serialize)]
pub struct TemplateOption {
    /// Unique key (e.g., "go", "mit")
    pub key: String,
    /// Display name (e.g., "Go", "MIT License")
    pub name: String,
    /// Short description shown in tooltip
    pub description: String,
}

/// Full template with content for a specific key.
#[derive(Debug, Clone)]
pub struct TemplateContent {
    pub key: String,
    pub name: String,
    pub description: String,
    /// File content (text)
    pub content: &'static str,
}

// ── .gitignore templates ──────────────────────────────────────────────

// Format: key => (display_name, description, content)
fn gitignore_map() -> BTreeMap<&'static str, (&'static str, &'static str, &'static str)> {
    let mut m = BTreeMap::new();

    m.insert(
        "go",
        (
            "Go",
            "Go language",
            include_str!("templates/gitignore/go.txt"),
        ),
    );
    m.insert(
        "rust",
        (
            "Rust",
            "Rust language",
            include_str!("templates/gitignore/rust.txt"),
        ),
    );
    m.insert(
        "python",
        (
            "Python",
            "Python language",
            include_str!("templates/gitignore/python.txt"),
        ),
    );
    m.insert(
        "node",
        (
            "Node.js",
            "Node.js / JavaScript",
            include_str!("templates/gitignore/node.txt"),
        ),
    );
    m.insert(
        "java",
        (
            "Java",
            "Java (Maven/Gradle)",
            include_str!("templates/gitignore/java.txt"),
        ),
    );
    m.insert(
        "cpp",
        (
            "C/C++",
            "C and C++",
            include_str!("templates/gitignore/cpp.txt"),
        ),
    );
    m.insert(
        "dotnet",
        (
            ".NET/C#",
            ".NET and C#",
            include_str!("templates/gitignore/dotnet.txt"),
        ),
    );
    m.insert(
        "flutter",
        (
            "Flutter/Dart",
            "Flutter and Dart",
            include_str!("templates/gitignore/flutter.txt"),
        ),
    );
    m.insert(
        "ruby",
        (
            "Ruby",
            "Ruby (Rails)",
            include_str!("templates/gitignore/ruby.txt"),
        ),
    );
    m.insert(
        "swift",
        (
            "Swift",
            "Swift / iOS",
            include_str!("templates/gitignore/swift.txt"),
        ),
    );
    m.insert(
        "kotlin",
        (
            "Kotlin",
            "Kotlin / Android",
            include_str!("templates/gitignore/kotlin.txt"),
        ),
    );
    m.insert(
        "php",
        (
            "PHP",
            "PHP (Laravel/Composer)",
            include_str!("templates/gitignore/php.txt"),
        ),
    );
    m.insert(
        "vue",
        (
            "Vue.js",
            "Vue.js / Svelte",
            include_str!("templates/gitignore/vue.txt"),
        ),
    );
    m.insert(
        "terraform",
        (
            "Terraform",
            "Terraform / IaC",
            include_str!("templates/gitignore/terraform.txt"),
        ),
    );
    m.insert(
        "docker",
        (
            "Docker",
            "Docker",
            include_str!("templates/gitignore/docker.txt"),
        ),
    );

    m
}

pub fn gitignore_options() -> Vec<TemplateOption> {
    gitignore_map()
        .iter()
        .map(|(k, (name, desc, _))| TemplateOption {
            key: k.to_string(),
            name: name.to_string(),
            description: desc.to_string(),
        })
        .collect()
}

pub fn gitignore_content(key: &str) -> Option<TemplateContent> {
    gitignore_map()
        .get(key)
        .map(|(name, desc, content)| TemplateContent {
            key: key.to_string(),
            name: name.to_string(),
            description: desc.to_string(),
            content,
        })
}

// ── LICENSE templates ─────────────────────────────────────────────────

fn license_map() -> BTreeMap<&'static str, (&'static str, &'static str, &'static str)> {
    let mut m = BTreeMap::new();

    m.insert(
        "mit",
        (
            "MIT License",
            "Permissive — do anything, keep copyright notice",
            include_str!("templates/licenses/mit.txt"),
        ),
    );
    m.insert(
        "apache-2.0",
        (
            "Apache License 2.0",
            "Permissive with patent grant",
            include_str!("templates/licenses/apache-2.0.txt"),
        ),
    );
    m.insert(
        "gpl-3.0",
        (
            "GNU GPL v3",
            "Copyleft — derivative works must also be GPL",
            include_str!("templates/licenses/gpl-3.0.txt"),
        ),
    );
    m.insert(
        "bsd-2-clause",
        (
            "BSD 2-Clause",
            "Short permissive license",
            include_str!("templates/licenses/bsd-2-clause.txt"),
        ),
    );
    m.insert(
        "bsd-3-clause",
        (
            "BSD 3-Clause",
            "Permissive with no-endorsement clause",
            include_str!("templates/licenses/bsd-3-clause.txt"),
        ),
    );
    m.insert(
        "unlicense",
        (
            "The Unlicense",
            "Public domain dedication",
            include_str!("templates/licenses/unlicense.txt"),
        ),
    );
    m.insert(
        "mpl-2.0",
        (
            "Mozilla Public License 2.0",
            "File-level copyleft",
            include_str!("templates/licenses/mpl-2.0.txt"),
        ),
    );

    m
}

pub fn license_options() -> Vec<TemplateOption> {
    license_map()
        .iter()
        .map(|(k, (name, desc, _))| TemplateOption {
            key: k.to_string(),
            name: name.to_string(),
            description: desc.to_string(),
        })
        .collect()
}

pub fn license_content(key: &str) -> Option<TemplateContent> {
    license_map()
        .get(key)
        .map(|(name, desc, content)| TemplateContent {
            key: key.to_string(),
            name: name.to_string(),
            description: desc.to_string(),
            content,
        })
}

// ── README templates ──────────────────────────────────────────────────

fn readme_map() -> BTreeMap<&'static str, (&'static str, &'static str, &'static str)> {
    let mut m = BTreeMap::new();

    m.insert(
        "default",
        (
            "Default",
            "Basic README with project name and description",
            include_str!("templates/readmes/default.md"),
        ),
    );

    m
}

pub fn readme_options() -> Vec<TemplateOption> {
    readme_map()
        .iter()
        .map(|(k, (name, desc, _))| TemplateOption {
            key: k.to_string(),
            name: name.to_string(),
            description: desc.to_string(),
        })
        .collect()
}

pub fn readme_content(key: &str, repo_name: &str, description: &str) -> Option<String> {
    readme_map().get(key).map(|(_, _, template)| {
        template
            .replace("{REPO_NAME}", repo_name)
            .replace("{DESCRIPTION}", description)
    })
}

// ── Default Issue Labels ──────────────────────────────────────────────

/// A default label definition.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DefaultLabel {
    pub name: String,
    pub color: String,
    pub description: String,
}

/// Default issue labels (inspired by GitHub/Gitea defaults).
pub fn default_labels(label_set: &str) -> Vec<DefaultLabel> {
    match label_set {
        "default" => vec![
            DefaultLabel {
                name: "bug".into(),
                color: "#d73a4a".into(),
                description: "Something isn't working".into(),
            },
            DefaultLabel {
                name: "enhancement".into(),
                color: "#a2eeef".into(),
                description: "New feature or request".into(),
            },
            DefaultLabel {
                name: "documentation".into(),
                color: "#0075ca".into(),
                description: "Improvements or additions to documentation".into(),
            },
            DefaultLabel {
                name: "duplicate".into(),
                color: "#cfd3d7".into(),
                description: "This issue or pull request already exists".into(),
            },
            DefaultLabel {
                name: "good first issue".into(),
                color: "#7057ff".into(),
                description: "Good for newcomers".into(),
            },
            DefaultLabel {
                name: "help wanted".into(),
                color: "#008672".into(),
                description: "Extra attention is needed".into(),
            },
            DefaultLabel {
                name: "invalid".into(),
                color: "#e4e669".into(),
                description: "This doesn't seem right".into(),
            },
            DefaultLabel {
                name: "question".into(),
                color: "#d876e3".into(),
                description: "Further information is requested".into(),
            },
            DefaultLabel {
                name: "wontfix".into(),
                color: "#ffffff".into(),
                description: "This will not be worked on".into(),
            },
        ],
        "scrum" => vec![
            DefaultLabel {
                name: "bug".into(),
                color: "#d73a4a".into(),
                description: "Something isn't working".into(),
            },
            DefaultLabel {
                name: "story".into(),
                color: "#a2eeef".into(),
                description: "User story".into(),
            },
            DefaultLabel {
                name: "task".into(),
                color: "#d4c5f9".into(),
                description: "Smaller task".into(),
            },
            DefaultLabel {
                name: "epic".into(),
                color: "#3E4BF0".into(),
                description: "Large epic".into(),
            },
            DefaultLabel {
                name: "spike".into(),
                color: "#fbca04".into(),
                description: "Research spike".into(),
            },
            DefaultLabel {
                name: "priority: high".into(),
                color: "#d93f0b".into(),
                description: "High priority".into(),
            },
            DefaultLabel {
                name: "priority: medium".into(),
                color: "#fbca04".into(),
                description: "Medium priority".into(),
            },
            DefaultLabel {
                name: "priority: low".into(),
                color: "#0e8a16".into(),
                description: "Low priority".into(),
            },
        ],
        _ => default_labels("default"),
    }
}

/// List available label sets.
pub fn label_set_options() -> Vec<TemplateOption> {
    vec![
        TemplateOption {
            key: "none".into(),
            name: "None".into(),
            description: "No default labels".into(),
        },
        TemplateOption {
            key: "default".into(),
            name: "Default".into(),
            description: "Standard labels (bug, enhancement, docs, etc.)".into(),
        },
        TemplateOption {
            key: "scrum".into(),
            name: "Scrum".into(),
            description: "Scrum labels (story, task, epic, spike)".into(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gitignore_options_non_empty() {
        let opts = gitignore_options();
        assert!(!opts.is_empty());
        assert!(opts.iter().any(|o| o.key == "go"));
        assert!(opts.iter().any(|o| o.key == "rust"));
    }

    #[test]
    fn test_gitignore_content_non_empty() {
        let content = gitignore_content("go").expect("go template should exist");
        assert!(!content.content.is_empty());
        assert!(content.content.contains("*.exe") || content.content.contains("*.out"));
    }

    #[test]
    fn test_license_options_non_empty() {
        let opts = license_options();
        assert!(!opts.is_empty());
        assert!(opts.iter().any(|o| o.key == "mit"));
        assert!(opts.iter().any(|o| o.key == "gpl-3.0"));
    }

    #[test]
    fn test_default_labels_count() {
        let labels = default_labels("default");
        assert_eq!(labels.len(), 9);
        let scrum = default_labels("scrum");
        assert_eq!(scrum.len(), 8);
    }

    #[test]
    fn test_readme_template_substitution() {
        let content = readme_content("default", "my-project", "A cool project");
        assert!(content.is_some());
        let c = content.unwrap();
        assert!(c.contains("my-project"));
        assert!(c.contains("A cool project"));
    }
}
