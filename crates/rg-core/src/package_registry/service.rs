//! Package Registry core service.
//!
//! Provides generic package publish/download/list/delete operations,
//! coordinating the DB ops and the storage layer.

use anyhow::Context as _;
use rg_db::package_version_key::NuGetVersion;
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, SqlErr, TransactionTrait};
use sha2::{Digest as _, Sha256};

use crate::error::not_found;
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
    /// The mutable npm selector carried by a real publish packument. `None`
    /// for every other package protocol and for ForgeKeep's generic uploader.
    pub npm_dist_tag: Option<String>,
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

/// Whether a stored version may participate in a fresh dependency resolution.
///
/// Protocols differ in how they express the answer: Cargo, PyPI and NuGet keep
/// an exact pin addressable and attach a marker, while protocols without such a
/// marker omit the version. The underlying decision is nevertheless the same
/// for every registry and belongs here, next to the persisted yank state.
pub const fn is_install_candidate(is_yanked: bool) -> bool {
    !is_yanked
}

impl VersionDetail {
    pub const fn is_install_candidate(&self) -> bool {
        is_install_candidate(self.is_yanked)
    }

    /// The SHA-256 of one of this version's files, as far as the registry can
    /// honestly state it.
    ///
    /// Every index that puts a checksum next to a download link has to answer
    /// "the digest of *what*", and the two candidates are not interchangeable.
    /// `FileDetail::sha256` is the file's own. `VersionDetail::sha256` is the
    /// digest of the **first file of the first publish request** (see
    /// `combined_sha256` below), which is all that was recorded before the
    /// per-file digests existed — and a version is routinely built up over
    /// several requests (`twine upload dist/*` sends a wheel and an sdist,
    /// `mvn deploy` a POM then a JAR).
    ///
    /// So the version-level digest is only used as a fallback when this version
    /// holds exactly one file, where it provably *is* that file's digest. With
    /// more than one, the answer is `None` and the caller leaves the field out:
    /// a client that gets no checksum skips the check, while one that gets the
    /// wrong checksum fails the install outright with `THESE PACKAGES DO NOT
    /// MATCH THE HASHES`.
    pub fn sha256_of(&self, file: &FileDetail) -> Option<String> {
        file.sha256.clone().or_else(|| {
            (self.files.len() == 1)
                .then(|| self.sha256.clone())
                .flatten()
        })
    }
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
    if let Some(tag) = info.npm_dist_tag.as_deref() {
        if info.package_type != package_types::NPM {
            return Err(crate::error::invalid_request(
                "an npm dist-tag cannot be attached to a non-npm package",
            ));
        }
        validate_npm_dist_tag(tag)?;
    }

    // The package row stores the spelling supplied by the publisher, while
    // this key stores protocol identity. NuGet considers `1`, `1.0.0`, a zero
    // Revision, case-only prerelease changes and build metadata aliases. The
    // key is nullable so unparsable historical spellings and protocols without
    // a separate identity contract retain their exact-text behavior.
    let protocol_version_key = (info.package_type == package_types::NUGET)
        .then(|| NuGetVersion::parse(&info.version))
        .flatten()
        .map(|version| version.normalized());

    // 1. Find or create the package registry for this repo+type
    let repo = crate::repo::service::find_repo_by_owner_name(db, &info.owner, &info.repo)
        .await?
        .ok_or_else(|| not_found("repository"))?;

    let registry =
        rg_db::ops::package_registry_ops::find_or_create(db, repo.id, &info.package_type).await?;

    // 2. Find or create the package. Get-or-create, not read-then-insert: two
    //    CI jobs publishing different versions of the same new package race
    //    here, and the loser must adopt the winner's row rather than fail a
    //    request that contradicts nothing.
    let pkg = rg_db::ops::package_ops::find_or_create(
        db,
        registry.id,
        info.author_id,
        &info.name,
        info.description.as_deref(),
        info.homepage.as_deref(),
        info.repository_url.as_deref(),
    )
    .await?;

    // 3. Check if version already exists. A parseable NuGet version is looked
    //    up by protocol identity, not by its raw spelling.
    let existing = match protocol_version_key.as_deref() {
        Some(key) => {
            rg_db::ops::package_version_ops::find_by_package_and_protocol_version_key(
                db, pkg.id, key,
            )
            .await?
        }
        None => {
            rg_db::ops::package_version_ops::find_by_package_and_version(db, pkg.id, &info.version)
                .await?
        }
    };
    let existing_version = existing.is_some();

    let version = if let Some(v) = existing {
        if protocol_version_key.is_some() && v.version != info.version {
            return Err(crate::error::conflict(format!(
                "NuGet version {} normalizes to the already-published version {} of package '{}'",
                info.version, v.version, info.name
            )));
        }
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

        for (filename, data) in &info.files {
            let sf = match storage
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
                Ok(sf) => sf,
                Err(error) => {
                    // A publish of several files (`mvn deploy` sends the POM
                    // then the JAR, PyPI an sdist beside a wheel) that fails
                    // half-way has already written the files before this one.
                    // Nothing points at them: the version row is created below,
                    // at step 5, so retention and every listing walk past them
                    // and only `df` ever sees them again. The caller must still
                    // see the storage failure, so a failed rollback can only be
                    // reported here.
                    discard_stored_files(
                        storage,
                        &stored_files,
                        &info,
                        "storing a file failed part-way through a publish",
                    )
                    .await;
                    return Err(error);
                }
            };
            stored_files.push(sf);
        }

        // Use first file's sha256 or combine
        let combined_sha256 = stored_files
            .first()
            .map(|stored| stored.digests.sha256.clone());

        // The version row and every file row are one visibility boundary. In
        // particular, another publish must not observe the version between
        // those writes and start adding files that our rollback could remove.
        let transaction = match db.begin().await {
            Ok(transaction) => transaction,
            Err(error) => {
                discard_stored_files(
                    storage,
                    &stored_files,
                    &info,
                    "starting the package publish transaction failed",
                )
                .await;
                return Err(error.into());
            }
        };

        // 5. Create version record
        let v = match rg_db::ops::package_version_ops::create(
            &transaction,
            pkg.id,
            &info.version,
            protocol_version_key.as_deref(),
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
                let concurrent_publish =
                    matches!(error.sql_err(), Some(SqlErr::UniqueConstraintViolation(_)));
                rollback_publish_transaction(
                    transaction,
                    &info,
                    "creating the package version row failed",
                )
                .await;
                discard_stored_files(
                    storage,
                    &stored_files,
                    &info,
                    "creating the package version row failed",
                )
                .await;
                if concurrent_publish {
                    return Err(crate::error::conflict(format!(
                        "version {} of package '{}' was published concurrently",
                        info.version, info.name
                    )));
                }
                return Err(error.into());
            }
        };

        // 6. Create file records
        for sf in &stored_files {
            if let Err(error) = rg_db::ops::package_file_ops::create(
                &transaction,
                v.id,
                &sf.filename,
                sf.size,
                file_digests(sf),
                &sf.storage_path,
            )
            .await
            {
                rollback_publish_transaction(
                    transaction,
                    &info,
                    "creating a package file row failed",
                )
                .await;
                discard_stored_files(
                    storage,
                    &stored_files,
                    &info,
                    "creating a package file row failed",
                )
                .await;
                return Err(error.into());
            }
        }

        // The version, its file row and the selector the client supplied are
        // one promise.  Writing the tag after commit would let a DB failure
        // return an error for a version that is nevertheless published; the
        // retry would then conflict and the requested selector would remain
        // lost.  Keep all three inside the visibility transaction.
        if let Some(tag) = info.npm_dist_tag.as_deref() {
            if let Err(error) = persist_npm_publish_dist_tag(&transaction, pkg.id, v.id, tag).await
            {
                rollback_publish_transaction(
                    transaction,
                    &info,
                    "persisting the npm dist-tag failed",
                )
                .await;
                discard_stored_files(
                    storage,
                    &stored_files,
                    &info,
                    "persisting the npm dist-tag failed",
                )
                .await;
                return Err(error);
            }
        }

        if let Err(error) = transaction.commit().await {
            // A commit error has an ambiguous outcome when the connection dies:
            // deleting these private objects could turn a commit that reached
            // the database into live rows pointing at missing bytes. Prefer an
            // observable orphan over data loss; the keys are request-private and
            // can be reconciled later.
            tracing::warn!(
                package = %format!("{}/{}", info.owner, info.repo),
                package_type = %info.package_type,
                name = %info.name,
                version = %info.version,
                error = %format!("{error:#}"),
                "package publish transaction commit failed with an ambiguous outcome — uploaded request-private files were left in storage rather than risk deleting files referenced by a commit that may have succeeded"
            );
            return Err(error.into());
        }

        v
    };

    // Package metadata describes the latest release, not the package's first
    // release forever. A newly created version is the latest by the registry's
    // creation-time ordering, so refresh only the fields its manifest carries.
    // Additional files uploaded into an older existing version must not roll
    // newer metadata back, and absent fields must not erase prior values.
    if !existing_version {
        rg_db::ops::package_ops::update_metadata(
            db,
            pkg.id,
            info.description.as_deref(),
            info.homepage.as_deref(),
            info.repository_url.as_deref(),
        )
        .await?;
    }

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

/// Reject only selector spellings the protocol cannot address safely. npm's
/// own CLI performs the richer npm-semver validation before sending a request;
/// this server-side boundary still prevents empty/path-like/control names and
/// the SemVer ranges Rust can recognize from entering the shared namespace.
pub fn validate_npm_dist_tag(tag: &str) -> Result<()> {
    if tag.is_empty()
        || tag.len() > 255
        || tag.trim() != tag
        || tag == "_etag"
        || matches!(tag, "." | "..")
        || tag.contains(['/', '\\'])
        || tag.chars().any(char::is_control)
    {
        return Err(crate::error::invalid_request("invalid npm dist-tag name"));
    }
    if semver::VersionReq::parse(tag).is_ok() {
        return Err(crate::error::invalid_request(
            "npm dist-tag name must not be a valid SemVer range",
        ));
    }
    Ok(())
}

/// Turn a legacy package's derived `latest` into an explicit tag exactly once.
///
/// `skip_version_id` is the version being published in the same transaction.
/// A first `--tag beta` publish must preserve the `latest` the client could read
/// immediately before this request, not manufacture `latest=beta` from the new
/// row that has not crossed the transaction boundary yet.
async fn initialize_npm_dist_tags(
    db: &impl ConnectionTrait,
    package_id: i64,
    skip_version_id: Option<i64>,
) -> Result<()> {
    let token = uuid::Uuid::new_v4().to_string();
    if !rg_db::ops::npm_dist_tag_ops::ensure_initialized(db, package_id, &token).await? {
        return Ok(());
    }

    let versions = rg_db::ops::package_version_ops::list_by_package(db, package_id).await?;
    let legacy_latest = crate::package_registry::adapters::npm::latest_live_semver(
        versions
            .iter()
            .filter(|version| Some(version.id) != skip_version_id)
            .map(|version| (version.version.as_str(), version.is_yanked)),
    );
    if let Some(legacy_latest) = legacy_latest {
        let version_id = versions
            .iter()
            .find(|version| version.version == legacy_latest)
            .map(|version| version.id)
            .ok_or_else(|| anyhow::anyhow!("selected legacy npm latest version disappeared"))?;
        rg_db::ops::npm_dist_tag_ops::upsert(db, package_id, "latest", version_id).await?;
    }
    Ok(())
}

async fn persist_npm_publish_dist_tag(
    db: &impl ConnectionTrait,
    package_id: i64,
    version_id: i64,
    tag: &str,
) -> Result<()> {
    initialize_npm_dist_tags(db, package_id, Some(version_id)).await?;
    rg_db::ops::npm_dist_tag_ops::upsert(db, package_id, tag, version_id).await?;
    Ok(())
}

/// Roll back DB work that has not crossed the transaction boundary yet.
async fn rollback_publish_transaction(
    transaction: DatabaseTransaction,
    info: &PublishInfo,
    reason: &'static str,
) {
    if let Err(cleanup_error) = transaction.rollback().await {
        tracing::warn!(
            package = %format!("{}/{}", info.owner, info.repo),
            package_type = %info.package_type,
            name = %info.name,
            version = %info.version,
            reason,
            error = %format!("{cleanup_error:#}"),
            "package publish DB transaction could not be rolled back"
        );
    }
}

/// Delete only the request-private objects returned by `store_file`.
async fn discard_stored_files(
    storage: &PackageStorage,
    stored_files: &[StoredFile],
    info: &PublishInfo,
    reason: &'static str,
) {
    for file in stored_files {
        if let Err(cleanup_error) = storage.delete_file(&file.storage_path).await {
            tracing::warn!(
                package = %format!("{}/{}", info.owner, info.repo),
                package_type = %info.package_type,
                name = %info.name,
                version = %info.version,
                filename = %file.filename,
                storage_path = %file.storage_path,
                reason,
                error = %format!("{cleanup_error:#}"),
                "orphaned package file: publish failed and deleting this request's stored object failed too"
            );
        }
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
/// The read below is only a cheap early answer. The database's
/// `UNIQUE(version_id, filename)` index is the concurrency boundary: file rows
/// and the corresponding size increase commit together, while the losing
/// request deletes only its request-private blobs.
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

    let mut stored_files: Vec<StoredFile> = Vec::new();

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
                discard_stored_files(
                    storage,
                    &stored_files,
                    info,
                    "storing a file failed while adding to an existing package version",
                )
                .await;
                return Err(error);
            }
        };
        stored_files.push(stored);
    }

    let transaction = match db.begin().await {
        Ok(transaction) => transaction,
        Err(error) => {
            discard_stored_files(
                storage,
                &stored_files,
                info,
                "starting the existing-version publish transaction failed",
            )
            .await;
            return Err(error.into());
        }
    };

    for stored in &stored_files {
        if let Err(error) = rg_db::ops::package_file_ops::create(
            &transaction,
            version.id,
            &stored.filename,
            stored.size,
            file_digests(stored),
            &stored.storage_path,
        )
        .await
        {
            let filename_conflict =
                matches!(error.sql_err(), Some(SqlErr::UniqueConstraintViolation(_)));
            rollback_publish_transaction(
                transaction,
                info,
                "creating a package file row for an existing version failed",
            )
            .await;
            discard_stored_files(
                storage,
                &stored_files,
                info,
                "creating a package file row for an existing version failed",
            )
            .await;
            if filename_conflict {
                return Err(crate::error::conflict(format!(
                    "file '{}' was published concurrently in version {} of package '{}'",
                    stored.filename, info.version, info.name
                )));
            }
            return Err(error.into());
        }
    }

    let added_size: i64 = stored_files.iter().map(|file| file.size).sum();
    if let Err(error) =
        rg_db::ops::package_version_ops::add_size(&transaction, version.id, added_size).await
    {
        rollback_publish_transaction(
            transaction,
            info,
            "updating package version size after adding files failed",
        )
        .await;
        discard_stored_files(
            storage,
            &stored_files,
            info,
            "updating package version size after adding files failed",
        )
        .await;
        return Err(error.into());
    }

    if let Err(error) = transaction.commit().await {
        tracing::warn!(
            package = %format!("{}/{}", info.owner, info.repo),
            package_type = %info.package_type,
            name = %info.name,
            version = %info.version,
            error = %format!("{error:#}"),
            "existing-version package publish transaction commit failed with an ambiguous outcome — uploaded request-private files were left in storage rather than risk deleting files referenced by a commit that may have succeeded"
        );
        return Err(error.into());
    }

    Ok(())
}

/// List all packages for a repository and package type.
pub async fn list_packages(
    db: &DatabaseConnection,
    owner: &str,
    repo: &str,
    package_type: &str,
) -> Result<Vec<PackageSummary>> {
    list_packages_with_nuget_filter(
        db,
        owner,
        repo,
        package_type,
        crate::package_registry::adapters::nuget::NuGetVersionFilter::PACKAGE_SUMMARY,
    )
    .await
}

/// List NuGet packages using the SearchQueryService capability filters.
pub async fn list_nuget_search_packages(
    db: &DatabaseConnection,
    owner: &str,
    repo: &str,
    include_prerelease: bool,
    semver_level: Option<&str>,
) -> Result<Vec<PackageSummary>> {
    list_packages_with_nuget_filter(
        db,
        owner,
        repo,
        package_types::NUGET,
        crate::package_registry::adapters::nuget::NuGetVersionFilter::search(
            include_prerelease,
            semver_level,
        ),
    )
    .await
}

async fn list_packages_with_nuget_filter(
    db: &DatabaseConnection,
    owner: &str,
    repo: &str,
    package_type: &str,
    nuget_filter: crate::package_registry::adapters::nuget::NuGetVersionFilter,
) -> Result<Vec<PackageSummary>> {
    let repo_model = crate::repo::service::find_repo_by_owner_name(db, owner, repo)
        .await?
        .ok_or_else(|| not_found("repository"))?;

    let registry =
        rg_db::ops::package_registry_ops::find_by_repo_and_type(db, repo_model.id, package_type)
            .await?
            .ok_or_else(|| not_found("package registry"))?;

    let packages = rg_db::ops::package_ops::list_by_registry(db, registry.id).await?;
    let mut summaries = Vec::new();

    for pkg in packages {
        let versions = rg_db::ops::package_version_ops::list_by_package(db, pkg.id).await?;
        let candidates = || {
            versions
                .iter()
                .map(|version| (version.version.as_str(), version.is_yanked))
        };
        let latest = match package_type {
            package_types::NPM => {
                crate::package_registry::adapters::npm::latest_live_semver(candidates())
            }
            package_types::NUGET => crate::package_registry::adapters::nuget::latest_live_nuget(
                candidates(),
                nuget_filter,
            ),
            _ => versions
                .iter()
                .find(|version| is_install_candidate(version.is_yanked))
                .map(|version| version.version.as_str()),
        }
        .map(str::to_string);
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
        .ok_or_else(|| not_found("repository"))?;

    let registry =
        rg_db::ops::package_registry_ops::find_by_repo_and_type(db, repo_model.id, package_type)
            .await?
            .ok_or_else(|| not_found("package registry"))?;

    let pkg = rg_db::ops::package_ops::find_by_registry_and_name(db, registry.id, name)
        .await?
        .ok_or_else(|| not_found("package"))?;

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
        .ok_or_else(|| not_found("repository"))?;

    let registry =
        rg_db::ops::package_registry_ops::find_by_repo_and_type(db, repo_model.id, package_type)
            .await?
            .ok_or_else(|| not_found("package registry"))?;

    let pkg = rg_db::ops::package_ops::find_by_registry_and_name(db, registry.id, name)
        .await?
        .ok_or_else(|| not_found("package"))?;

    let versions = rg_db::ops::package_version_ops::list_by_package(db, pkg.id).await?;
    futures_for_versions(db, versions).await
}

async fn resolve_npm_package(
    db: &DatabaseConnection,
    owner: &str,
    repo: &str,
    name: &str,
) -> Result<rg_db::entities::package::Model> {
    let repo_model = crate::repo::service::find_repo_by_owner_name(db, owner, repo)
        .await?
        .ok_or_else(|| not_found("repository"))?;
    let registry = rg_db::ops::package_registry_ops::find_by_repo_and_type(
        db,
        repo_model.id,
        package_types::NPM,
    )
    .await?
    .ok_or_else(|| not_found("package registry"))?;
    rg_db::ops::package_ops::find_by_registry_and_name(db, registry.id, name)
        .await?
        .ok_or_else(|| not_found("package"))
}

/// Read the package's canonical npm tag map. Packages that predate persisted
/// tags retain their historical derived `latest` until the first tag mutation
/// atomically materializes it.
pub async fn list_npm_dist_tags(
    db: &DatabaseConnection,
    owner: &str,
    repo: &str,
    name: &str,
) -> Result<std::collections::BTreeMap<String, String>> {
    let package = resolve_npm_package(db, owner, repo, name).await?;
    if !rg_db::ops::npm_dist_tag_ops::is_initialized(db, package.id).await? {
        let versions = rg_db::ops::package_version_ops::list_by_package(db, package.id).await?;
        let latest = crate::package_registry::adapters::npm::latest_live_semver(
            versions
                .iter()
                .map(|version| (version.version.as_str(), version.is_yanked)),
        );
        return Ok(latest
            .map(|version| [("latest".to_string(), version.to_string())].into())
            .unwrap_or_default());
    }

    let mut tags = std::collections::BTreeMap::new();
    for (tag, version) in rg_db::ops::npm_dist_tag_ops::list_by_package(db, package.id).await? {
        // npm has no yanked marker in either the tag listing or the packument.
        // Advertising a tag to a version we omit from `versions` makes the
        // selector resolve to a document entry that does not exist.
        if is_install_candidate(version.is_yanked) {
            tags.insert(tag, version.version);
        }
    }
    Ok(tags)
}

/// Set or move a dist-tag to an existing live npm version.
pub async fn set_npm_dist_tag(
    db: &DatabaseConnection,
    owner: &str,
    repo: &str,
    name: &str,
    tag: &str,
    version: &str,
) -> Result<()> {
    validate_npm_dist_tag(tag)?;
    let package = resolve_npm_package(db, owner, repo, name).await?;
    let transaction = db.begin().await?;
    let version = rg_db::ops::package_version_ops::find_by_package_and_version(
        &transaction,
        package.id,
        version,
    )
    .await?
    .filter(|version| is_install_candidate(version.is_yanked))
    .ok_or_else(|| not_found("package version"))?;

    initialize_npm_dist_tags(&transaction, package.id, None).await?;
    rg_db::ops::npm_dist_tag_ops::upsert(&transaction, package.id, tag, version.id).await?;
    transaction.commit().await?;
    Ok(())
}

/// Remove a dist-tag while keeping the initialized empty set distinguishable
/// from a legacy package whose `latest` still needs compatibility derivation.
pub async fn remove_npm_dist_tag(
    db: &DatabaseConnection,
    owner: &str,
    repo: &str,
    name: &str,
    tag: &str,
) -> Result<()> {
    validate_npm_dist_tag(tag)?;
    let package = resolve_npm_package(db, owner, repo, name).await?;
    let transaction = db.begin().await?;
    initialize_npm_dist_tags(&transaction, package.id, None).await?;
    if !rg_db::ops::npm_dist_tag_ops::delete(&transaction, package.id, tag).await? {
        return Err(not_found("npm dist-tag"));
    }
    transaction.commit().await?;
    Ok(())
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
        .ok_or_else(|| not_found("repository"))?;

    let registry =
        rg_db::ops::package_registry_ops::find_by_repo_and_type(db, repo_model.id, package_type)
            .await?
            .ok_or_else(|| not_found("package registry"))?;

    let pkg = rg_db::ops::package_ops::find_by_registry_and_name(db, registry.id, name)
        .await?
        .ok_or_else(|| not_found("package"))?;

    let v = rg_db::ops::package_version_ops::find_by_package_and_version(db, pkg.id, version_str)
        .await?
        .ok_or_else(|| not_found("package version"))?;

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

/// One stored package file, read back and checked against its recorded digest.
pub struct DownloadedFile {
    pub data: Vec<u8>,
    pub content_type: String,
    pub size: i64,
    /// The digest the bytes were verified against, or `None` for a legacy row
    /// that carries no recorded hash. Served on to the client as
    /// `X-Checksum-Sha256`.
    pub sha256: Option<String>,
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
) -> Result<DownloadedFile> {
    // Resolve and increment download count
    let version_detail = get_version(db, owner, repo, package_type, name, version_str).await?;

    let file = version_detail
        .files
        .iter()
        .find(|f| f.filename == filename)
        .ok_or_else(|| not_found("package file"))?;

    let file_model = rg_db::ops::package_file_ops::find_by_id(db, file.id)
        .await?
        .ok_or_else(|| not_found("package file"))?;

    let data = storage.read_file(&file_model.storage_path).await?;

    // Integrity check: the stored bytes must still hash to the digest recorded
    // at publish. This is the same digest the server hands clients as the
    // install checksum in the npm / PyPI / RubyGems / Helm indexes, so a package
    // served without checking it is a package the server vouches for and has not
    // looked at. Skipping the check does not make the mismatch go away — it moves
    // it to the client, where storage rot surfaces as `npm ERR! EINTEGRITY` and
    // reads as a broken registry or a broken client, with nothing on this side to
    // say otherwise (card_51c3ddd4c3e0).
    //
    // Legacy rows written before digest tracking carry no hash and are served
    // without this guard, exactly as `release::service::download_asset` does.
    if let Some(expected) = file_model.sha256.as_deref() {
        let actual = hex::encode(Sha256::digest(&data));
        if actual != expected {
            // Logged as well as returned: the client learns its download failed,
            // but only the operator can act on "the bytes under this key are not
            // the bytes that were published".
            tracing::error!(
                package_file_id = file_model.id,
                storage_path = %file_model.storage_path,
                owner = %owner,
                repo = %repo,
                name = %name,
                version = %version_str,
                filename = %filename,
                expected_sha256 = %expected,
                actual_sha256 = %actual,
                "stored package file does not match its recorded digest — refusing to serve it"
            );
            anyhow::bail!(
                "package file integrity check failed: expected sha256 {expected}, got {actual}"
            );
        }
    }

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

    Ok(DownloadedFile {
        data,
        content_type,
        size: file.size,
        sha256: file_model.sha256,
    })
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

    // Rows written before the blob-storage migration hold an absolute path
    // instead of a key, so they sit outside the version prefix and the prefix
    // move cannot reach them. They are staged by path alongside it.
    let legacy_paths: Vec<String> = rg_db::ops::package_file_ops::list_by_version(db, v.id)
        .await?
        .into_iter()
        .filter(|file| crate::blob_storage::BlobKey::new(&file.storage_path).is_err())
        .map(|file| file.storage_path)
        .collect();

    // Bytes leave the live namespace reversibly and *before* the metadata: the
    // storage half is the one that cannot be undone once it has run, so it is
    // the half that must be undoable while the metadata write can still fail.
    let deletion_id = uuid::Uuid::new_v4().simple().to_string();
    let staging = storage
        .stage_version_deletion(
            owner,
            repo,
            package_type,
            name,
            version_str,
            &legacy_paths,
            &deletion_id,
        )
        .await?;

    // Both metadata deletes are one statement pair. The file rows are a bulk
    // cascade — a version carrying no files legitimately matches zero of them —
    // but a failure between the two used to leave a live version whose files
    // had already been removed, which no rollback of the bytes could repair.
    match delete_version_metadata(db, v.id).await {
        // `get_version` above ran as a separate statement, so a concurrent
        // delete can take the row in between. That request owns the deletion, so
        // this one must not answer 204 for it — the staged bytes are still
        // retired rather than restored, since the metadata is gone either way.
        Ok(false) => {
            staging.retire(storage).await?;
            return Err(not_found("package version"));
        }
        Ok(true) => {}
        Err(error) => {
            staging.restore(storage).await;
            return Err(error).context("failed to delete package version metadata");
        }
    }

    staging.retire(storage).await
}

/// Remove a version's file rows and the version row as one transaction.
///
/// Returns whether the version row itself was still there. A zero-row version
/// delete rolls the whole thing back: the concurrent request that took the row
/// owns its file rows too, and committing a partial delete on top of it would
/// destroy rows this call was never entitled to.
async fn delete_version_metadata(db: &DatabaseConnection, version_id: i64) -> Result<bool> {
    let transaction = db
        .begin()
        .await
        .context("db: begin package version delete")?;
    rg_db::ops::package_file_ops::delete_by_version(&transaction, version_id).await?;
    let deleted = rg_db::ops::package_version_ops::delete_by_id(&transaction, version_id).await?;
    if deleted == 0 {
        transaction
            .rollback()
            .await
            .context("db: roll back package version delete")?;
        return Ok(false);
    }
    transaction
        .commit()
        .await
        .context("db: commit package version delete")?;
    Ok(true)
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
