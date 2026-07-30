//! Driving a WebSocket handshake far enough to read what the server answered.
//!
//! A refused upgrade is an ordinary HTTP reply — the same `AppError` envelope
//! every REST route answers with — but it arrives through a handshake rather
//! than through `reqwest`, so the id-scope sweeps cannot reach it with the
//! driver they use for everything else. This module is that driver, and it
//! returns the same [`Answer`] the rest of them compare, so the normalization of
//! "what did the caller learn" stays in one place.
//!
//! Two files need it — `job_websocket_tests` for the bespoke pair and
//! `foreign_id_scope_sweep_tests` for the table-walking one — and a second copy
//! of a comparison is how the three `shape()` helpers this directory used to
//! carry drifted apart.

use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::Request;
use tokio_tungstenite::tungstenite::Error;

use super::answer::Answer;

/// A handshake request for `path` on this test server, carrying `token` as the
/// `bearer.<jwt>` subprotocol a browser client uses.
///
/// `token: None` is the anonymous caller — no credential at all, which is a
/// probe in its own right rather than an omission.
#[allow(dead_code)]
pub fn handshake_request(base: &str, path: &str, token: Option<&str>) -> Request<()> {
    let url = format!("{}{path}", base.replacen("http://", "ws://", 1));
    let mut request = url.into_client_request().expect("a routable ws:// url");
    if let Some(token) = token {
        request.headers_mut().insert(
            "sec-websocket-protocol",
            format!("bearer.{token}")
                .parse()
                .expect("a header-safe token"),
        );
    }
    request
}

/// Everything a caller learns from a handshake the server refused.
///
/// A handshake that is *accepted* is reported as the failure it is rather than
/// returned: a probe owed a denial that got a socket instead would otherwise be
/// compared against another accepted handshake and match.
#[allow(dead_code)]
pub async fn refusal(base: &str, path: &str, token: Option<&str>) -> Answer {
    match tokio_tungstenite::connect_async(handshake_request(base, path, token)).await {
        Err(Error::Http(response)) => Answer {
            status: reqwest::StatusCode::from_u16(response.status().as_u16())
                .expect("the handshake answered with a real status"),
            body: String::from_utf8_lossy(response.body().as_deref().unwrap_or(&[])).into_owned(),
        },
        Ok((_socket, response)) => panic!(
            "the handshake on {path} was accepted ({}) where a refusal was owed",
            response.status()
        ),
        Err(other) => panic!("the handshake on {path} failed for the wrong reason: {other:?}"),
    }
}
