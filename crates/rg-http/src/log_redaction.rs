//! Keeping credentials that travel in a query string out of the logs.
//!
//! Every request runs inside the `http_request` span `routes` opens, and both
//! log layers print that span's fields on every event emitted under it, while
//! the OTLP layer exports them. The span used to carry the whole URI, so
//! anything a client put after the `?` — the SSO callback's `code` and `state`,
//! a password-reset `token`, a presigned-URL signature — was copied into every
//! log line the request wrote and into every exported trace. The span now
//! records the path and a query string that has gone through [`redact_query`].
//!
//! Redaction is by *key*: the values of the keys [`is_sensitive_key`] names are
//! replaced with [`REDACTED`], everything else is kept verbatim so a log line
//! still says which page was asked for and what was searched. A key is matched
//! after percent-decoding and case-folding, so `%74oken` and `TOKEN` hide
//! nothing from it.

use std::borrow::Cow;

/// What a redacted value is printed as.
pub(crate) const REDACTED: &str = "[redacted]";

/// Keys whose value is a credential or a one-time secret wherever they appear.
const SENSITIVE_KEYS: &[&str] = &[
    "token",
    "access_token",
    "id_token",
    "refresh_token",
    "code",
    "state",
    "key",
    "secret",
    "password",
    "signature",
    "sig",
    "authorization",
    "jwt",
    "nonce",
    "otp",
];

/// Whether `key` (as it appears on the wire, possibly percent-encoded) names a
/// value that must not reach a log line.
///
/// Beyond the fixed list, the `X-Amz-*` family of presigned-URL parameters is
/// covered as a whole, and so is any key ending in `_token`, `_secret` or
/// `_key` — the spellings OAuth-style and API-key-style parameters settle on.
pub(crate) fn is_sensitive_key(key: &str) -> bool {
    let decoded = percent_decode_lossy(key);
    let key = decoded.to_ascii_lowercase();
    SENSITIVE_KEYS.contains(&key.as_str())
        || key.starts_with("x-amz-")
        || key.ends_with("_token")
        || key.ends_with("_secret")
        || key.ends_with("_key")
}

/// `query` with every sensitive value replaced by [`REDACTED`].
///
/// Pairs are split on `&` and at the first `=`; the key keeps its original
/// spelling and order, so the output still reads as the query that was sent.
/// A sensitive key with no `=` at all carries no value and is left as it is.
pub(crate) fn redact_query(query: &str) -> String {
    if query.is_empty() {
        return String::new();
    }
    query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((key, _)) if is_sensitive_key(key) => Cow::Owned(format!("{key}={REDACTED}")),
            _ => Cow::Borrowed(pair),
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// Percent-decode `raw`, keeping it as it was when it is not valid UTF-8 — a
/// key that cannot be decoded is compared as spelled rather than dropped.
fn percent_decode_lossy(raw: &str) -> Cow<'_, str> {
    if !raw.contains('%') {
        return Cow::Borrowed(raw);
    }
    urlencoding::decode(raw).unwrap_or(Cow::Borrowed(raw))
}

#[cfg(test)]
mod tests {
    use super::{redact_query, REDACTED};

    #[test]
    fn a_plain_query_is_left_alone() {
        assert_eq!(redact_query("page=2&per_page=50"), "page=2&per_page=50");
        assert_eq!(
            redact_query("q=hello%20world&sort=asc"),
            "q=hello%20world&sort=asc"
        );
        assert_eq!(
            redact_query("service=git-upload-pack"),
            "service=git-upload-pack"
        );
    }

    #[test]
    fn an_empty_query_stays_empty() {
        assert_eq!(redact_query(""), "");
    }

    #[test]
    fn sensitive_values_are_replaced_and_the_rest_kept_in_order() {
        assert_eq!(
            redact_query("token=abc&page=2"),
            format!("token={REDACTED}&page=2")
        );
        assert_eq!(
            redact_query("code=4/0AX4&state=xyz&scope=openid"),
            format!("code={REDACTED}&state={REDACTED}&scope=openid")
        );
        let redacted = redact_query("page=2&token=eyJhbGciOiJIUzI1NiJ9.e30.sig");
        assert!(!redacted.contains("eyJ"), "{redacted}");
        assert_eq!(redacted, format!("page=2&token={REDACTED}"));
    }

    #[test]
    fn every_occurrence_of_a_repeated_key_is_redacted() {
        assert_eq!(
            redact_query("token=one&page=1&token=two"),
            format!("token={REDACTED}&page=1&token={REDACTED}")
        );
    }

    #[test]
    fn a_key_is_matched_after_percent_decoding_and_case_folding() {
        assert_eq!(redact_query("%74oken=abc"), format!("%74oken={REDACTED}"));
        assert_eq!(redact_query("TOKEN=abc"), format!("TOKEN={REDACTED}"));
        assert_eq!(
            redact_query("Access_Token=abc"),
            format!("Access_Token={REDACTED}")
        );
    }

    #[test]
    fn the_suffix_and_prefix_families_are_covered() {
        for key in [
            "access_token",
            "id_token",
            "refresh_token",
            "client_secret",
            "api_key",
            "ssh_key",
            "secret",
            "password",
            "signature",
            "sig",
            "X-Amz-Signature",
            "X-Amz-Credential",
            "x-amz-security-token",
        ] {
            assert_eq!(
                redact_query(&format!("{key}=value")),
                format!("{key}={REDACTED}"),
                "{key} was not redacted"
            );
        }
    }

    #[test]
    fn a_value_containing_an_equals_sign_is_redacted_whole() {
        assert_eq!(
            redact_query("token=a=b=c&page=2"),
            format!("token={REDACTED}&page=2")
        );
    }

    #[test]
    fn a_bare_key_without_a_value_is_left_as_it_is() {
        assert_eq!(redact_query("token&page=2"), "token&page=2");
        assert_eq!(
            redact_query("token=&page=2"),
            format!("token={REDACTED}&page=2")
        );
    }

    #[test]
    fn keys_that_merely_contain_a_sensitive_word_are_not_redacted() {
        assert_eq!(redact_query("tokens=3"), "tokens=3");
        assert_eq!(redact_query("keyword=rust"), "keyword=rust");
        assert_eq!(redact_query("codename=x"), "codename=x");
    }

    /// Everything a test subscriber wrote, readable after the fact.
    #[derive(Clone, Default)]
    struct CapturedLogs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl CapturedLogs {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().expect("log lock")).into_owned()
        }
    }

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl tracing_subscriber::fmt::MakeWriter<'_> for CapturedLogs {
        type Writer = CapturedLogs;

        fn make_writer(&self) -> Self::Writer {
            self.clone()
        }
    }

    /// The span the request layer opens is what every log line inherits, so
    /// this drives a request through that very layer and reads the line a
    /// handler wrote under it: the credential must be gone, the ordinary
    /// parameter must still be there.
    ///
    /// A current-thread runtime, so the thread-local subscriber installed
    /// here is the one the router's future runs under.
    #[tokio::test]
    async fn a_log_line_written_under_the_request_span_carries_the_redacted_query() {
        use axum::routing::get;
        use tower::ServiceExt as _;

        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let router = axum::Router::new()
            .route(
                "/x",
                get(|| async {
                    tracing::info!("handled inside the request span");
                    "ok"
                }),
            )
            .layer(
                tower_http::trace::TraceLayer::new_for_http()
                    .make_span_with(crate::routes::request_span),
            );
        let response = router
            .oneshot(
                axum::http::Request::builder()
                    .uri("/x?token=abc&page=2")
                    .body(axum::body::Body::empty())
                    .expect("a request"),
            )
            .await
            .expect("the router answers");
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        let rendered = logs.text();
        assert!(
            rendered.contains("handled inside the request span"),
            "the handler's line was not captured: {rendered}"
        );
        assert!(
            rendered.contains("path=/x"),
            "the span must still name the path: {rendered}"
        );
        assert!(
            rendered.contains(&format!("token={REDACTED}")),
            "the credential must be marked as redacted: {rendered}"
        );
        assert!(
            rendered.contains("page=2"),
            "an ordinary parameter must survive redaction: {rendered}"
        );
        assert!(
            !rendered.contains("abc"),
            "the credential's value leaked into the log: {rendered}"
        );
    }
}
