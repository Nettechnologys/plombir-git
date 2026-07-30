//! Reading a route *pattern* the way the id-scope sweeps need it.
//!
//! `RouteFact::path` is the pattern a client's URL has to match —
//! `/api/v1/users/ssh-keys/{id}` — and every sweep that selects routes by the
//! kind of id they carry has to take it apart the same way. This was one
//! function private to `cross_repo_id_scope_sweep_tests`; the moment a second
//! sweep needed it, it moved here rather than being copied, for the reason
//! `common::answer` exists at all — the three copies of the body normalizer that
//! preceded that module had already drifted apart in what they normalized.

/// The placeholder names of a path, in order.
///
/// The leading `*` of a wildcard capture is stripped, so `{*tail}` is reported
/// as `tail`: a sweep classifies a placeholder by what it names, and the capture
/// syntax is not part of that.
#[allow(dead_code)]
pub fn placeholders(path: &str) -> impl Iterator<Item = &str> {
    path.split('{').skip(1).filter_map(|chunk| {
        chunk
            .split('}')
            .next()
            .map(|name| name.strip_prefix('*').unwrap_or(name))
    })
}
