//! Repository archive download.
//!
//! GET /api/v1/repos/{owner}/{name}/archive/{sha}.zip
//! GET /api/v1/repos/{owner}/{name}/archive/{sha}.tar.gz

use crate::AppState;
use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
};

use crate::api::repo_access;
use crate::error::AppError;

/// GET /api/v1/repos/{owner}/{name}/archive/{sha}.zip
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/archive/{archive}",
    tag = "Repositories",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("name" = String, Path, description = "Repository name"),
        ("archive" = String, Path, description = "Archive filename (e.g. main.zip, v1.0.tar.gz)"),
    ),
    responses(
        (status = 200, description = "Archive binary stream", content_type = "application/zip"),
        (status = 400, description = "Unsupported archive format"),
        (status = 404, description = "Repository not found"),
    ),
)]
pub async fn download_archive(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, name, archive)): Path<(String, String, String)>,
) -> impl IntoResponse {
    // axum 0.8 allows only one parameter per path segment, so the filename
    // (`<sha>.<ext>`) arrives as a single `{archive}` segment that we split
    // here. `.tar.gz`/`.tgz` are checked before `.zip`.
    let (sha, ext) = if let Some(s) = archive.strip_suffix(".tar.gz") {
        (s.to_string(), "tar.gz")
    } else if let Some(s) = archive.strip_suffix(".tgz") {
        (s.to_string(), "tgz")
    } else if let Some(s) = archive.strip_suffix(".zip") {
        (s.to_string(), "zip")
    } else {
        return AppError::bad_request("Unsupported format").into_response();
    };

    if let Err(e) =
        repo_access::require_read_with_ci(&state, &headers, &owner, &name, "repo:read").await
    {
        return e.into_response();
    }

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, name));
    if !repo_path.exists() {
        return AppError::not_found("repository data not found").into_response();
    }

    let format_flag = match ext {
        "zip" => "zip",
        "tar.gz" | "tgz" => "tar.gz",
        _ => return AppError::bad_request("Unsupported format").into_response(),
    };

    let mime = match ext {
        "zip" => "application/zip",
        _ => "application/gzip",
    };

    let git = match rg_git::cli_gateway::global_gateway().as_ref() {
        Ok(g) => g,
        Err(e) => return AppError::internal(e.to_string()).into_response(),
    };

    let git_out = match git.run(
        &["archive", &format!("--format={}", format_flag), &sha],
        Some(&repo_path),
    ) {
        Ok(o) => o,
        Err(e) => return AppError::from(e).into_response(),
    };

    let output = match git_out.ensure_success() {
        Ok(()) => git_out.stdout,
        Err(_) => return AppError::bad_request("Invalid ref or SHA").into_response(),
    };

    // Truncate by chars (not bytes) so a short ref like `main` or a
    // multi-byte ref name cannot panic on a non-char-boundary byte slice.
    let short: String = sha.chars().take(7).collect();
    let filename = format!("{}-{}.{}", name, short, ext);

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime),
            (
                header::CONTENT_DISPOSITION,
                &format!("attachment; filename=\"{}\"", filename),
            ),
        ],
        output,
    )
        .into_response()
}
