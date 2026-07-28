//! Package Registry core service.
//!
//! Provides generic package publish/download/list/delete operations,
//! coordinating the DB ops and the storage layer.

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
    /// SHA-1 of the file, for the protocols whose checksum field is defined as
    /// SHA-1 (`dist.shasum` in npm and Composer). `None` for files published
    /// before the registry recorded it, and the field is then left out of the
    /// metadata rather than filled with a different algorithm's digest.
    pub sha1: Option<String>,
    /// SHA-512 of the file, published as npm's `dist.integrity`. `None` for
    /// files published before the registry recorded it.
    pub sha512: Option<String>,
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
        // The version is already there, so this request is adding a file to it —
        // and the files it carries are the whole point of the request. They used
        // to be dropped on the floor here, under a `200 OK` that said nothing:
        // a version is routinely uploaded in several requests (`mvn deploy`
        // sends the POM, then the JAR, then the sources; PyPI puts an sdist
        // beside a wheel; NuGet pushes a symbols package next to the main one),
        // and every request after the first left no trace in storage or the
        // database.
        add_files_to_version(db, storage, &info, &v).await?;
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
            combined_sha256 = Some(sf.digests.sha256.clone());
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
                // Compensation on the error path: the caller must still see the
                // DB failure, so a failed rollback can only be reported here.
                if let Err(cleanup_error) = storage
                    .delete_version(
                        &info.owner,
                        &info.repo,
                        &info.package_type,
                        &info.name,
                        &info.version,
                    )
                    .await
                {
                    tracing::warn!(
                        package = %format!("{}/{}", info.owner, info.repo),
                        package_type = %info.package_type,
                        name = %info.name,
                        version = %info.version,
                        error = %format!("{cleanup_error:#}"),
                        "orphaned package files: the version record was not created and the rollback delete failed too — the uploaded files stay in storage with no version row pointing at them"
                    );
                }
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
                file_digests(sf),
                &sf.storage_path,
            )
            .await
            {
                // Three-part compensation; each part can fail on its own and
                // leaves a different kind of residue behind. The caller only
                // ever sees the original file-record error, so every failed
                // step has to name what it left behind.
                if let Err(cleanup_error) = storage
                    .delete_version(
                        &info.owner,
                        &info.repo,
                        &info.package_type,
                        &info.name,
                        &info.version,
                    )
                    .await
                {
                    tracing::warn!(
                        package = %format!("{}/{}", info.owner, info.repo),
                        package_type = %info.package_type,
                        name = %info.name,
                        version = %info.version,
                        error = %format!("{cleanup_error:#}"),
                        "orphaned package files: publish failed and the rollback delete failed too — the uploaded files stay in storage"
                    );
                }
                if let Err(cleanup_error) =
                    rg_db::ops::package_file_ops::delete_by_version(db, v.id).await
                {
                    tracing::warn!(
                        version_id = v.id,
                        name = %info.name,
                        version = %info.version,
                        error = %format!("{cleanup_error:#}"),
                        "orphaned package_file rows: publish failed and deleting the already-inserted file records failed too — they point at files the rollback is removing"
                    );
                }
                if let Err(cleanup_error) =
                    rg_db::ops::package_version_ops::delete_by_id(db, v.id).await
                {
                    tracing::warn!(
                        version_id = v.id,
                        name = %info.name,
                        version = %info.version,
                        error = %format!("{cleanup_error:#}"),
                        "orphaned package_version row: publish failed and deleting the version record failed too — the version is listed but its files are gone"
                    );
                }
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

/// The digests of a freshly stored file, in the shape the DB layer records.
///
/// Every one of them is written at publish: a metadata route can then answer
/// with the digest its protocol names instead of passing off the one column
/// that happened to exist.
fn file_digests(stored: &StoredFile) -> rg_db::ops::package_file_ops::FileDigests<'_> {
    rg_db::ops::package_file_ops::FileDigests {
        sha256: Some(&stored.digests.sha256),
        sha1: Some(&stored.digests.sha1),
        sha512: Some(&stored.digests.sha512),
    }
}

/// Add the files of a publish request to a version that already exists.
///
/// A filename the version already holds is refused with a [`conflict`] rather
/// than overwritten: a published artifact is what everyone who resolved that
/// name already got, and swapping its bytes underneath them is the one thing a
/// registry must not do. The check is on the filename alone, so re-sending a
/// byte-identical file is refused too — the request is still asking to replace
/// something, and answering "already published" is honest where a silent 200
/// was not.
///
/// Rollback is per-file and never touches what the version already held:
/// `delete_version` is the tool the create path uses, and here it would wipe
/// the artifacts of every earlier request.
async fn add_files_to_version(
    db: &DatabaseConnection,
    storage: &PackageStorage,
    info: &PublishInfo,
    version: &rg_db::entities::package_version::Model,
) -> Result<()> {
    let published = rg_db::ops::package_file_ops::list_by_version(db, version.id).await?;
    for (filename, _) in &info.files {
        if published.iter().any(|f| &f.filename == filename) {
            return Err(crate::error::conflict(format!(
                "file '{}' is already published in version {} of package '{}'",
                filename, info.version, info.name
            )));
        }
    }

    // Everything this request adds, so a failure part-way through can be undone
    // without disturbing the files that were already there.
    let mut added: Vec<StoredFile> = Vec::new();
    let mut added_rows: Vec<i64> = Vec::new();

    for (filename, data) in &info.files {
        let stored = match storage
            .store_file(
                &info.owner,
                &info.repo,
                &info.package_type,
                &info.name,
                &info.version,
                filename,
                data,
            )
            .await
        {
            Ok(stored) => stored,
            Err(error) => {
                undo_added_files(db, storage, &added, &added_rows, info).await;
                return Err(error);
            }
        };

        match rg_db::ops::package_file_ops::create(
            db,
            version.id,
            &stored.filename,
            stored.size,
            file_digests(&stored),
            &stored.storage_path,
        )
        .await
        {
            Ok(row) => added_rows.push(row.id),
            Err(error) => {
                added.push(stored);
                undo_added_files(db, storage, &added, &added_rows, info).await;
                return Err(error.into());
            }
        }

        added.push(stored);
    }

    // The version's recorded size is the total of what it holds, so it has to
    // grow with the files just added — otherwise every listing under-reports the
    // version from its second upload onwards.
    let added_size: i64 = added.iter().map(|f| f.size).sum();
    if let Err(error) = rg_db::ops::package_version_ops::add_size(db, version.id, added_size).await
    {
        // The files themselves are published and downloadable; only the total is
        // now short. Failing the request would be worse than saying so.
        tracing::warn!(
            version_id = version.id,
            name = %info.name,
            version = %info.version,
            added_size,
            error = %format!("{error:#}"),
            "package version size not updated — the version's reported size now under-counts the files just added to it"
        );
    }

    Ok(())
}

/// Undo the files one publish request added to an existing version.
///
/// Both halves can fail on their own and each leaves a different residue: an
/// undeleted row points at a file that is gone, an undeleted file is storage
/// nobody will ever ask for again. The caller only ever sees the original
/// error, so each failure has to name what it left behind.
async fn undo_added_files(
    db: &DatabaseConnection,
    storage: &PackageStorage,
    added: &[StoredFile],
    added_rows: &[i64],
    info: &PublishInfo,
) {
    for id in added_rows {
        if let Err(cleanup_error) = rg_db::ops::package_file_ops::delete_by_id(db, *id).await {
            tracing::warn!(
                file_id = *id,
                name = %info.name,
                version = %info.version,
                error = %format!("{cleanup_error:#}"),
                "orphaned package_file row: adding a file to an existing version failed and deleting the record of an already-added file failed too — it points at a file the rollback is removing"
            );
        }
    }
    for file in added {
        if let Err(cleanup_error) = storage.delete_file(&file.storage_path).await {
            tracing::warn!(
                name = %info.name,
                version = %info.version,
                filename = %file.filename,
                error = %format!("{cleanup_error:#}"),
                "orphaned package file: adding a file to an existing version failed and the rollback delete failed too — the file stays in storage with no record pointing at it"
            );
        }
    }
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
            sha1: f.sha1,
            sha512: f.sha512,
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

    // Increment download counts. A failure here must not fail the download the
    // client already got — but it does mean the published statistics undercount
    // from now on, so it cannot pass unnoticed either.
    if let Err(error) =
        rg_db::ops::package_version_ops::increment_download_count(db, version_detail.id).await
    {
        tracing::warn!(
            version_id = version_detail.id,
            name = %name,
            version = %version_str,
            error = %format!("{error:#}"),
            "package version download counter not incremented — the version's download count now undercounts this download"
        );
    }
    // Need to get package_id from version — we already know it
    let v = rg_db::ops::package_version_ops::find_by_id(db, version_detail.id).await?;
    if let Some(v) = v {
        if let Err(error) =
            rg_db::ops::package_ops::increment_download_count(db, v.package_id).await
        {
            tracing::warn!(
                package_id = v.package_id,
                name = %name,
                version = %version_str,
                error = %format!("{error:#}"),
                "package download counter not incremented — the package's download count now undercounts this download"
            );
        }
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
                sha1: f.sha1,
                sha512: f.sha512,
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
