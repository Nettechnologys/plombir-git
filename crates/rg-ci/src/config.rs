//! CI configuration types for `.ironforge-ci.yml`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::HashMap;

/// Concurrency control for CI/CD workflows.
///
/// Prevents multiple pipelines from running simultaneously for the same group.
/// If `cancel_in_progress` is true, any currently running pipeline in the same
/// group will be cancelled before the new one starts.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

    /// Per-job timeout in seconds.
    #[serde(default)]
    pub timeout_seconds: Option<u64>,

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheConfig {
    pub key: String,
    pub paths: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

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
