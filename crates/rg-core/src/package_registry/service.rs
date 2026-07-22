//! Package Registry 核心服务
//!
//! 提供通用的包发布/下载/列表/删除操作，协调 DB ops 和存储层。

use sea_orm::DatabaseConnection;

use crate::package_registry::storage::{PackageStorage, StoredFile};

/// Package type constants for known package managers.
pub mod package_types {
    pub const CARGO: &str = "cargo";
    pub const NPM: &str = "npm";
    pub const MAVEN: &str = "maven";
    pub const PYPI: &str = "pypi";
    pub const DOCKER: &str = "docker";
    pub const NUGET: &str = "nuget";
    pub const RUBYGEMS: &str = "rubygems";
    pub const GO: &str = "go";
    pub const HELM: &str = "helm";
    pub const COMPOSER: &str = "composer";
    pub const CONAN: &str = "conan";
    pub const CONDA: &str = "conda";
    pub const ALPINE: &str = "alpine";
    pub const DEBIAN: &str = "debian";
    pub const RPM: &str = "rpm";
    pub const SWIFT: &str = "swift";
    pub const GENERIC: &str = "generic";

    /// All supported package types.
    pub const ALL: &[&str] = &[
        CARGO, NPM, MAVEN, PYPI, DOCKER, NUGET, RUBYGEMS, GO, HELM, COMPOSER, CONAN, CONDA, ALPINE,
        DEBIAN, RPM, SWIFT, GENERIC,
    ];

    /// Validate a package type string.
    pub fn is_valid(t: &str) -> bool {
        ALL.contains(&t)
    }
}

/// Info needed to publish a package.
pub struct PublishInfo {
    pub owner: String,
    pub repo: String,
    pub package_type: String,
    pub name: String,
    pub version: String,
    pub semver: Option<String>,
    pub metadata: Option<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub repository_url: Option<String>,
    pub author_id: i64,
    /// File name → file data
    pub files: Vec<(String, Vec<u8>)>,
}

/// Result of publishing a package.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PublishResult {
    pub package_id: i64,
    pub version_id: i64,
    pub existing: bool,
}

/// Summary of a package for listing.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PackageSummary {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub version_count: i64,
    pub latest_version: Option<String>,
    pub download_count: i64,
    pub keywords: Option<String>,
}

/// Details of a specific package version.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VersionDetail {
    pub id: i64,
    pub version: String,
    pub semver: Option<String>,
    pub metadata: Option<String>,
    pub size: i64,
    pub sha256: Option<String>,
    pub is_yanked: bool,
    pub download_count: i64,
    pub files: Vec<FileDetail>,
    pub created_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileDetail {
    pub id: i64,
    pub filename: String,
    pub size: i64,
    pub sha256: Option<String>,
}

/// Publish a package version to the registry.
pub async fn publish(
    db: &DatabaseConnection,
    storage: &PackageStorage,
    info: PublishInfo,
) -> Result<PublishResult> {
    // 1. Find or create the package registry for this repo+type
    let repo = crate::repo::service::find_repo_by_owner_name(db, &info.owner, &info.repo)
        .await?
        .ok_or_else(|| anyhow::anyhow!("repository {}/{} not found", info.owner, info.repo))?;

    let registry =
        rg_db::ops::package_registry_ops::find_or_create(db, repo.id, &info.package_type).await?;

    // 2. Find or create the package
    let pkg = match rg_db::ops::package_ops::find_by_registry_and_name(db, registry.id, &info.name)
        .await?
    {
        Some(p) => p,
        None => {
            rg_db::ops::package_ops::create(
                db,
                registry.id,
                info.author_id,
                &info.name,
                info.description.as_deref(),
                info.homepage.as_deref(),
                info.repository_url.as_deref(),
            )
            .await?
        }
    };

    // 3. Check if version already exists
    let existing =
        rg_db::ops::package_version_ops::find_by_package_and_version(db, pkg.id, &info.version)
            .await?;
    let existing_version = existing.is_some();

    let version = if let Some(v) = existing {
        // Version already exists — allow re-upload? For safety, return existing.
        tracing::warn!(
            "version {} of package {} already exists, returning existing",
            info.version,
            info.name
        );
        v
    } else {
        // 4. Store files
        let total_size: i64 = info.files.iter().map(|(_, d)| d.len() as i64).sum();
        let mut stored_files: Vec<StoredFile> = Vec::new();
        let mut combined_sha256: Option<String> = None;

        for (filename, data) in &info.files {
            let sf = storage
                .store_file(
                    &info.owner,
                    &info.repo,
                    &info.package_type,
                    &info.name,
                    &info.version,
                    filename,
                    data,
                )
                .await?;
            stored_files.push(sf);
        }

        // Use first file's sha256 or combine
        if let Some(sf) = stored_files.first() {
            combined_sha256 = Some(sf.sha256.clone());
        }

        // 5. Create version record
        let v = match rg_db::ops::package_version_ops::create(
            db,
            pkg.id,
            &info.version,
            info.semver.as_deref(),
            info.metadata.as_deref(),
            total_size,
            combined_sha256.as_deref(),
            Some(info.author_id),
        )
        .await
        {
            Ok(version) => version,
            Err(error) => {
                let _ = storage
                    .delete_version(
                        &info.owner,
                        &info.repo,
                        &info.package_type,
                        &info.name,
                        &info.version,
                    )
                    .await;
                return Err(error.into());
            }
        };

        // 6. Create file records
        for sf in &stored_files {
            if let Err(error) = rg_db::ops::package_file_ops::create(
                db,
                v.id,
                &sf.filename,
                sf.size,
                Some(&sf.sha256),
                &sf.storage_path,
            )
            .await
            {
                let _ = storage
                    .delete_version(
                        &info.owner,
                        &info.repo,
                        &info.package_type,
                        &info.name,
                        &info.version,
                    )
                    .await;
                let _ = rg_db::ops::package_file_ops::delete_by_version(db, v.id).await;
                let _ = rg_db::ops::package_version_ops::delete_by_id(db, v.id).await;
                return Err(error.into());
            }
        }

        v
    };

    Ok(PublishResult {
        package_id: pkg.id,
        version_id: version.id,
        existing: existing_version,
    })
}

/// List all packages for a repository and package type.
pub async fn list_packages(
    db: &DatabaseConnection,
    owner: &str,
    repo: &str,
    package_type: &str,
) -> Result<Vec<PackageSummary>> {
    let repo_model = crate::repo::service::find_repo_by_owner_name(db, owner, repo)
        .await?
        .ok_or_else(|| anyhow::anyhow!("repository not found"))?;

    let registry =
        rg_db::ops::package_registry_ops::find_by_repo_and_type(db, repo_model.id, package_type)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("package type '{}' not enabled for this repo", package_type)
            })?;

    let packages = rg_db::ops::package_ops::list_by_registry(db, registry.id).await?;
    let mut summaries = Vec::new();

    for pkg in packages {
        let versions = rg_db::ops::package_version_ops::list_by_package(db, pkg.id).await?;
        let latest = versions.first().map(|v| v.version.clone());
        let count = versions.len() as i64;

        summaries.push(PackageSummary {
            id: pkg.id,
            name: pkg.name.clone(),
            description: pkg.description.clone(),
            homepage: pkg.homepage.clone(),
            version_count: count,
            latest_version: latest,
            download_count: pkg.download_count,
            keywords: None, // package DB table doesn't store keywords; extracted from versions
        });
    }

    Ok(summaries)
}

/// Get a package by name.
pub async fn get_package(
    db: &DatabaseConnection,
    owner: &str,
    repo: &str,
    package_type: &str,
    name: &str,
) -> Result<crate::package_registry::PackageDetail> {
    let repo_model = crate::repo::service::find_repo_by_owner_name(db, owner, repo)
        .await?
        .ok_or_else(|| anyhow::anyhow!("repository not found"))?;

    let registry =
        rg_db::ops::package_registry_ops::find_by_repo_and_type(db, repo_model.id, package_type)
            .await?
            .ok_or_else(|| anyhow::anyhow!("package type not enabled"))?;

    let pkg = rg_db::ops::package_ops::find_by_registry_and_name(db, registry.id, name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("package not found"))?;

    let versions = rg_db::ops::package_version_ops::list_by_package(db, pkg.id).await?;
    let version_details: Vec<VersionDetail> = futures_for_versions(db, versions).await?;

    Ok(crate::package_registry::PackageDetail {
        id: pkg.id,
        name: pkg.name,
        description: pkg.description,
        homepage: pkg.homepage,
        repository_url: pkg.repository_url,
        download_count: pkg.download_count,
        versions: version_details,
    })
}

/// List versions of a package.
pub async fn list_versions(
    db: &DatabaseConnection,
    owner: &str,
    repo: &str,
    package_type: &str,
    name: &str,
) -> Result<Vec<VersionDetail>> {
    let repo_model = crate::repo::service::find_repo_by_owner_name(db, owner, repo)
        .await?
        .ok_or_else(|| anyhow::anyhow!("repository not found"))?;

    let registry =
        rg_db::ops::package_registry_ops::find_by_repo_and_type(db, repo_model.id, package_type)
            .await?
            .ok_or_else(|| anyhow::anyhow!("package type not enabled"))?;

    let pkg = rg_db::ops::package_ops::find_by_registry_and_name(db, registry.id, name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("package not found"))?;

    let versions = rg_db::ops::package_version_ops::list_by_package(db, pkg.id).await?;
    futures_for_versions(db, versions).await
}

/// Get a specific version.
pub async fn get_version(
    db: &DatabaseConnection,
    owner: &str,
    repo: &str,
    package_type: &str,
    name: &str,
    version_str: &str,
) -> Result<VersionDetail> {
    let repo_model = crate::repo::service::find_repo_by_owner_name(db, owner, repo)
        .await?
        .ok_or_else(|| anyhow::anyhow!("repository not found"))?;

    let registry =
        rg_db::ops::package_registry_ops::find_by_repo_and_type(db, repo_model.id, package_type)
            .await?
            .ok_or_else(|| anyhow::anyhow!("package type not enabled"))?;

    let pkg = rg_db::ops::package_ops::find_by_registry_and_name(db, registry.id, name)
        .await?
        .ok_or_else(|| anyhow::anyhow!("package not found"))?;

    let v = rg_db::ops::package_version_ops::find_by_package_and_version(db, pkg.id, version_str)
        .await?
        .ok_or_else(|| anyhow::anyhow!("version not found"))?;

    let files = rg_db::ops::package_file_ops::list_by_version(db, v.id).await?;
    let file_details: Vec<FileDetail> = files
        .into_iter()
        .map(|f| FileDetail {
            id: f.id,
            filename: f.filename,
            size: f.size,
            sha256: f.sha256,
        })
        .collect();

    Ok(VersionDetail {
        id: v.id,
        version: v.version,
        semver: v.semver,
        metadata: v.metadata,
        size: v.size,
        sha256: v.sha256,
        is_yanked: v.is_yanked,
        download_count: v.download_count,
        files: file_details,
        created_at: v.created_at.to_rfc3339(),
    })
}

/// Download a version file.
#[allow(clippy::too_many_arguments)]
pub async fn download_file(
    db: &DatabaseConnection,
    storage: &PackageStorage,
    owner: &str,
    repo: &str,
    package_type: &str,
    name: &str,
    version_str: &str,
    filename: &str,
) -> Result<(Vec<u8>, String, i64)> {
    // Resolve and increment download count
    let version_detail = get_version(db, owner, repo, package_type, name, version_str).await?;

    let file = version_detail
        .files
        .iter()
        .find(|f| f.filename == filename)
        .ok_or_else(|| {
            anyhow::anyhow!("file '{}' not found in version {}", filename, version_str)
        })?;

    let file_model = rg_db::ops::package_file_ops::find_by_id(db, file.id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("file record not found"))?;

    let data = storage.read_file(&file_model.storage_path).await?;

    // Increment download counts
    let _ = rg_db::ops::package_version_ops::increment_download_count(db, version_detail.id).await;
    // Need to get package_id from version — we already know it
    let v = rg_db::ops::package_version_ops::find_by_id(db, version_detail.id).await?;
    if let Some(v) = v {
        let _ = rg_db::ops::package_ops::increment_download_count(db, v.package_id).await;
    }

    let content_type = mime_guess_for_filename(filename);

    Ok((data, content_type, file.size))
}

/// Delete a package version.
pub async fn delete_version(
    db: &DatabaseConnection,
    storage: &PackageStorage,
    owner: &str,
    repo: &str,
    package_type: &str,
    name: &str,
    version_str: &str,
) -> Result<()> {
    let v = get_version(db, owner, repo, package_type, name, version_str).await?;

    // Delete files from storage
    storage
        .delete_version(owner, repo, package_type, name, version_str)
        .await?;

    // Delete DB records
    rg_db::ops::package_file_ops::delete_by_version(db, v.id).await?;
    rg_db::ops::package_version_ops::delete_by_id(db, v.id).await?;

    Ok(())
}

/// Yank a version (soft delete — mark as pulled).
pub async fn yank_version(
    db: &DatabaseConnection,
    owner: &str,
    repo: &str,
    package_type: &str,
    name: &str,
    version_str: &str,
    yank: bool,
) -> Result<()> {
    let v = get_version(db, owner, repo, package_type, name, version_str).await?;
    rg_db::ops::package_version_ops::set_yanked(db, v.id, yank).await?;
    Ok(())
}

/// Package detail for API responses.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PackageDetail {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub repository_url: Option<String>,
    pub download_count: i64,
    pub versions: Vec<VersionDetail>,
}

// ── helpers ────────────────────────────────────────────────

async fn futures_for_versions(
    db: &DatabaseConnection,
    versions: Vec<rg_db::entities::package_version::Model>,
) -> Result<Vec<VersionDetail>> {
    let mut details = Vec::new();
    for v in versions {
        let files = rg_db::ops::package_file_ops::list_by_version(db, v.id).await?;
        let file_details: Vec<FileDetail> = files
            .into_iter()
            .map(|f| FileDetail {
                id: f.id,
                filename: f.filename,
                size: f.size,
                sha256: f.sha256,
            })
            .collect();

        details.push(VersionDetail {
            id: v.id,
            version: v.version,
            semver: v.semver,
            metadata: v.metadata,
            size: v.size,
            sha256: v.sha256,
            is_yanked: v.is_yanked,
            download_count: v.download_count,
            files: file_details,
            created_at: v.created_at.to_rfc3339(),
        });
    }
    Ok(details)
}

fn mime_guess_for_filename(filename: &str) -> String {
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    match ext {
        "gz" | "tgz" => "application/gzip".into(),
        _ => "application/octet-stream".into(),
    }
}

pub type Error = anyhow::Error;
pub type Result<T> = std::result::Result<T, Error>;
