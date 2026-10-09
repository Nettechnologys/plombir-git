//! A renamed or transferred repository's old address (card_e83bf21a5e5b).
//!
//! The rename keeps the repository's id and moves every storage family, but
//! the old `owner/name` answered `404`. Here a `404` under
//! `/api/v1/repos/{owner}/{name}` is given one more look: if a rename or a
//! transfer left that address and the caller may read the repository it went
//! to, a read is redirected (`308`) to the same path under the new name, and a
//! write is refused with a `404` that names the new address — a write is never
//! carried to a repository the caller did not name.
//!
//! The look happens only after the request was answered `404`, so a live
//! repository at the address always wins and the ordinary path costs nothing.
//! A caller who may not read the target gets the original `404`, untouched: a
//! redirect must not reveal where a repository it cannot see went.
//!
//! The Git LFS API under `…/lfs` is the exception to "writes are refused"
//! (card_e0351e77eabd). It is not a REST client naming a repository: it is
//! `git lfs`, deriving its endpoint from the remote URL `git` kept after a
//! clone of the old address, and the git transport already carries that clone,
//! fetch and push to the new name by redirecting `info/refs`. Refusing the
//! batch `POST` left the files of every such clone as pointer text. So the LFS
//! API is redirected with `307`, which keeps the method and the body, and an
//! anonymous request for a private repository gets the `401` its live name
//! would give — the only answer that makes `git lfs` come back with
//! credentials.

use axum::extract::{OriginalUri, Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::AppError;
use crate::AppState;

const API_REPOS: &str = "/api/v1/repos/";

/// `owner`, `name`, and the rest of the path after them (empty, or starting
/// with `/`).
fn split_repo_path<'a>(path: &'a str, prefix: &str) -> Option<(&'a str, &'a str, &'a str)> {
    let tail = path.strip_prefix(prefix)?;
    let (owner, tail) = tail.split_once('/')?;
    let (name, rest) = match tail.find('/') {
        Some(at) => (&tail[..at], &tail[at..]),
        None => (tail, ""),
    };
    (!owner.is_empty() && !name.is_empty()).then_some((owner, name, rest))
}

/// Layered on the `/api/v1` table, inside the PAT translation, so the access
/// check below reads the same credential the handlers did.
pub(crate) async fn follow_renamed_repository(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    let method = request.method().clone();
    let uri = request
        .extensions()
        .get::<OriginalUri>()
        .map(|original| original.0.clone())
        .unwrap_or_else(|| request.uri().clone());
    let headers = request.headers().clone();
    let response = next.run(request).await;
    if response.status() != StatusCode::NOT_FOUND {
        return response;
    }
    // `git lfs` reaches this table through `…/{repo}.git/info/lfs/…`, which
    // is rewritten onto the REST path before routing — so the original URI
    // still has the git spelling (`routes::with_lfs_endpoint_discovery`).
    let api_path =
        crate::routes::lfs_discovery_api_path(uri.path()).unwrap_or_else(|| uri.path().to_string());
    let Some((owner, name, rest)) = split_repo_path(&api_path, API_REPOS) else {
        return response;
    };
    let lfs = is_lfs_api(rest);
    match redirect_target(&state, &headers, owner, name).await {
        RenamedTarget::Readable(new_owner, new_name) => {
            let location = format!(
                "{API_REPOS}{new_owner}/{new_name}{rest}{}",
                uri.query()
                    .map(|query| format!("?{query}"))
                    .unwrap_or_default()
            );
            if method == Method::GET || method == Method::HEAD {
                (
                    StatusCode::PERMANENT_REDIRECT,
                    [(header::LOCATION, location)],
                )
                    .into_response()
            } else if lfs {
                (
                    StatusCode::TEMPORARY_REDIRECT,
                    [(header::LOCATION, location)],
                )
                    .into_response()
            } else {
                AppError::not_found(format!(
                    "repository '{owner}/{name}' was renamed to '{new_owner}/{new_name}'; send \
                     the request to {location}"
                ))
                .into_response()
            }
        }
        RenamedTarget::Refused(refusal) if lfs && refusal.status() == StatusCode::UNAUTHORIZED => {
            refusal.into_response()
        }
        RenamedTarget::Refused(_) | RenamedTarget::Nothing => response,
    }
}

/// Whether `rest` — the path after `owner/name` — is the Git LFS API.
fn is_lfs_api(rest: &str) -> bool {
    rest == "/lfs" || rest.starts_with("/lfs/")
}

/// What the address `owner/name` leads to, as the caller may learn it.
enum RenamedTarget {
    /// The repository moved to this `owner/name`, and the caller may read it.
    Readable(String, String),
    /// The repository moved, and the read check refused the caller with this.
    /// Never shown to a REST client: the new name stays undisclosed.
    Refused(AppError),
    /// Nothing moved out of the address, or the lookup failed.
    Nothing,
}

/// The repository `owner/name` was moved away from, checked against the caller.
async fn redirect_target(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
) -> RenamedTarget {
    let found = match rg_core::repo::service::find_renamed_repo(&state.db, owner, name).await {
        Ok(Some(found)) => found,
        Ok(None) => return RenamedTarget::Nothing,
        Err(error) => {
            tracing::warn!(owner, name, error = %format!("{error:#}"), "renamed-repository lookup failed");
            return RenamedTarget::Nothing;
        }
    };
    let (target, new_owner) = found;
    match crate::api::repo_access::check_read(state, headers, &target).await {
        Ok(()) => RenamedTarget::Readable(new_owner, target.name),
        Err(refusal) => RenamedTarget::Refused(refusal),
    }
}

#[cfg(test)]
mod tests {
    use super::{is_lfs_api, split_repo_path};

    #[test]
    fn only_the_lfs_subtree_is_the_lfs_api() {
        assert!(is_lfs_api("/lfs/objects/batch"));
        assert!(is_lfs_api("/lfs/locks/verify"));
        assert!(!is_lfs_api("/lfs-settings"));
        assert!(!is_lfs_api("/issues"));
        assert!(!is_lfs_api(""));
    }

    #[test]
    fn the_repository_address_is_split_off_the_rest_of_the_path() {
        assert_eq!(
            split_repo_path("/api/v1/repos/alice/proj", "/api/v1/repos/"),
            Some(("alice", "proj", ""))
        );
        assert_eq!(
            split_repo_path("/api/v1/repos/alice/proj/issues/3", "/api/v1/repos/"),
            Some(("alice", "proj", "/issues/3"))
        );
        assert_eq!(
            split_repo_path("/api/v1/repos/alice", "/api/v1/repos/"),
            None
        );
        assert_eq!(split_repo_path("/api/v1/user", "/api/v1/repos/"), None);
    }
}
