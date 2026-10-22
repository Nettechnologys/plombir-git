//! Which media type a file this server did not write is kept and served under.
//!
//! One list for every way such a file comes in — a release asset uploaded over
//! HTTP, one an import brought from the source forge — so a type cannot be
//! passive on one path and active on the other.

/// The media types a user-uploaded download is served under as they were
/// stored: ones a browser only ever *displays* — never runs as script or
/// style, never renders as a document of our origin.
///
/// Anything else is served as `application/octet-stream`. The stored type is
/// whatever the uploader said (a release asset's `Content-Type`, a multipart
/// part's), and `attachment` plus `nosniff` do not stop `<script src>` from
/// running a file served with a JavaScript type: `script-src 'self'` lets it
/// in, so any later HTML injection would become a full XSS with no nonce
/// needed (card_36b620ab3467).
const PASSIVE_UPLOAD_TYPES: &[&str] = &[
    "application/gzip",
    "application/json",
    "application/pdf",
    "application/zip",
    "image/avif",
    "image/gif",
    "image/jpeg",
    "image/png",
    // Scripts inside an SVG never run from `<img>`; opened directly it is a
    // download, and `UPLOAD_SANDBOX_CSP` holds even if a browser renders it.
    "image/svg+xml",
    "image/webp",
    "text/csv",
    "text/plain",
    "video/mp4",
    "video/quicktime",
    "video/webm",
];

/// The type a user-uploaded file is kept and served under, given the one the
/// uploader sent. See [`PASSIVE_UPLOAD_TYPES`].
///
/// Applied on the way in as well as on the way out: a row that never carried
/// an active type cannot be served under one by a reader that forgot to ask.
pub fn passive_upload_type(stored: &str) -> &'static str {
    let essence = stored
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    PASSIVE_UPLOAD_TYPES
        .iter()
        .find(|passive| **passive == essence)
        .copied()
        .unwrap_or("application/octet-stream")
}
