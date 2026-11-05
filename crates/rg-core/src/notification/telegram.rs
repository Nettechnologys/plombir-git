//! Personal Telegram delivery. The only production destination is Telegram's
//! HTTPS API. Neither a transport error nor the remote response is an error
//! source: both may contain the bot token from the request URL.

use std::time::Duration;

use serde::{Deserialize, Serialize};

const MAX_RESPONSE_BYTES: usize = 16 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CHAT_ID: i64 = (1_i64 << 52) - 1;

/// Deliberately not Debug or Serialize. The raw secret is only exposed to the
/// fixed-endpoint sender, never to response DTOs or tracing instrumentation.
pub struct Destination {
    token: String,
    chat_id: i64,
    message_thread_id: Option<i32>,
}

impl Destination {
    pub fn new(token: String, chat_id: i64, message_thread_id: Option<i32>) -> Result<Self, Error> {
        validate_token(&token)?;
        if chat_id == 0 || !(-MAX_CHAT_ID..=MAX_CHAT_ID).contains(&chat_id) {
            return Err(Error::InvalidChat);
        }
        if message_thread_id.is_some_and(|id| id <= 0) {
            return Err(Error::InvalidTopic);
        }
        Ok(Self {
            token,
            chat_id,
            message_thread_id,
        })
    }
}

/// Validate without putting the input into any diagnostic. Restricting the
/// token to its BotFather alphabet also prevents URL/path injection.
pub fn validate_token(token: &str) -> Result<(), Error> {
    let (id, secret) = token.split_once(':').ok_or(Error::InvalidToken)?;
    if id.is_empty()
        || id.len() > 20
        || !id.bytes().all(|byte| byte.is_ascii_digit())
        || id.bytes().all(|byte| byte == b'0')
        || !(30..=128).contains(&secret.len())
        || !secret
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(Error::InvalidToken);
    }
    Ok(())
}

/// Safe, finite reasons for the UI and outbox. No URL, remote description,
/// response bytes, recipient or credential is stored in an error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidToken,
    InvalidChat,
    InvalidTopic,
    InvalidText,
    Timeout,
    Network,
    Unauthorized,
    Forbidden,
    InvalidDestination,
    RateLimited { retry_after: u32 },
    Unavailable,
    Redirect,
    Protocol,
    ResponseTooLarge,
}

impl Error {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvalidToken => "telegram_invalid_token",
            Self::InvalidChat => "telegram_invalid_chat",
            Self::InvalidTopic => "telegram_invalid_topic",
            Self::InvalidText => "telegram_invalid_text",
            Self::Timeout => "telegram_timeout",
            Self::Network => "telegram_network",
            Self::Unauthorized => "telegram_unauthorized",
            Self::Forbidden => "telegram_forbidden",
            Self::InvalidDestination => "telegram_invalid_destination",
            Self::RateLimited { .. } => "telegram_rate_limited",
            Self::Unavailable => "telegram_unavailable",
            Self::Redirect => "telegram_redirect_refused",
            Self::Protocol => "telegram_invalid_response",
            Self::ResponseTooLarge => "telegram_response_too_large",
        }
    }

    /// Delay the outbox must respect, in addition to its own bounded backoff.
    /// A retry_after beyond the job's lifetime expires the job, rather than
    /// being shortened and violating Telegram's flood-control instruction.
    pub fn retry_after(self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after } => Some(Duration::from_secs(retry_after.into())),
            Self::Timeout | Self::Network | Self::Unavailable => Some(Duration::from_secs(30)),
            _ => None,
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for Error {}

fn transport_error(error: reqwest::Error) -> Error {
    // Do not format, log, retain as source, or return `error.without_url()`:
    // even an inner source is not part of this module's safe error contract.
    if error.is_timeout() {
        Error::Timeout
    } else {
        Error::Network
    }
}

#[derive(Serialize)]
struct Message<'a> {
    chat_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    message_thread_id: Option<i32>,
    text: &'a str,
    link_preview_options: Preview,
}

#[derive(Serialize)]
struct Preview {
    is_disabled: bool,
}

#[derive(Deserialize)]
struct ApiResponse {
    ok: bool,
    error_code: Option<u16>,
    parameters: Option<Parameters>,
    result: Option<SentMessage>,
}

#[derive(Deserialize)]
struct Parameters {
    retry_after: Option<u32>,
}

#[derive(Deserialize)]
struct SentMessage {
    message_id: i64,
}

fn response_result(status: reqwest::StatusCode, bytes: &[u8]) -> Result<(), Error> {
    if status.is_redirection() {
        return Err(Error::Redirect);
    }
    // Status itself remains useful even if an upstream proxy answered HTML.
    if status.is_server_error() {
        return Err(Error::Unavailable);
    }
    let parsed = serde_json::from_slice::<ApiResponse>(bytes).ok();
    let code = parsed
        .as_ref()
        .and_then(|response| response.error_code)
        .unwrap_or(status.as_u16());
    if status.as_u16() == 429 || code == 429 {
        let retry_after = parsed
            .and_then(|response| response.parameters)
            .and_then(|parameters| parameters.retry_after)
            .unwrap_or(60)
            .max(1);
        return Err(Error::RateLimited { retry_after });
    }
    match code {
        401 | 404 => return Err(Error::Unauthorized),
        403 => return Err(Error::Forbidden),
        400 => return Err(Error::InvalidDestination),
        500..=599 => return Err(Error::Unavailable),
        _ => {}
    }
    let response = parsed.ok_or(Error::Protocol)?;
    if status.is_success()
        && response.ok
        && response.result.is_some_and(|result| result.message_id >= 0)
    {
        Ok(())
    } else {
        Err(Error::Protocol)
    }
}

/// Reusable HTTP client with no configurable base URL or redirects. It does
/// not implement Debug because a future client field might carry a secret.
pub struct Client(reqwest::Client);

impl Client {
    pub fn new() -> Result<Self, Error> {
        Ok(Self(
            Self::builder()
                .https_only(true)
                .build()
                .map_err(transport_error)?,
        ))
    }

    fn builder() -> reqwest::ClientBuilder {
        crate::platform::tls::install_crypto_provider();
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            // The durable dispatcher owns retries and their accounting.
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(3))
            .timeout(REQUEST_TIMEOUT)
            .no_proxy()
            .connection_verbose(false)
    }

    pub async fn send(&self, destination: &Destination, text: &str) -> Result<(), Error> {
        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            destination.token
        );
        self.send_to(&url, destination, text).await
    }

    // Private test seam. There is no public/user-configurable endpoint, and
    // the production client additionally refuses HTTP at the transport level.
    async fn send_to(&self, url: &str, destination: &Destination, text: &str) -> Result<(), Error> {
        if text.is_empty() || text.encode_utf16().count() > 4096 {
            return Err(Error::InvalidText);
        }
        let mut response = self
            .0
            .post(url)
            .json(&Message {
                chat_id: destination.chat_id,
                message_thread_id: destination.message_thread_id,
                text,
                link_preview_options: Preview { is_disabled: true },
            })
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status();
        if status.is_redirection() {
            return Err(Error::Redirect);
        }
        if status.is_server_error() {
            // Retry even when an upstream failure has an enormous or stalled
            // body. Its contents cannot refine this transport verdict.
            return Err(Error::Unavailable);
        }
        if response
            .content_length()
            .is_some_and(|size| size > MAX_RESPONSE_BYTES as u64)
        {
            return Err(Error::ResponseTooLarge);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(Error::ResponseTooLarge);
            }
            bytes.extend_from_slice(&chunk);
        }
        response_result(status, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const TOKEN: &str = "123456:ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghi";

    #[derive(Clone, Default)]
    struct Logs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Logs {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl tracing_subscriber::fmt::MakeWriter<'_> for Logs {
        type Writer = Self;
        fn make_writer(&self) -> Self {
            self.clone()
        }
    }

    fn destination(topic: Option<i32>) -> Destination {
        Destination::new(TOKEN.into(), -1001234567890, topic)
            .ok()
            .unwrap()
    }

    /// Local HTTP only, a single request, fake token; no Telegram traffic.
    async fn serve(response: String) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            loop {
                let mut buffer = [0; 1024];
                let read = stream.read(&mut buffer).await.unwrap();
                assert!(read > 0);
                bytes.extend_from_slice(&buffer[..read]);
                assert!(bytes.len() < 32 * 1024);
                if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let length = headers
                        .lines()
                        .find_map(|line| {
                            let (key, value) = line.split_once(':')?;
                            key.eq_ignore_ascii_case("content-length")
                                .then(|| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            stream.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(bytes).unwrap()
        });
        (format!("http://{address}/bot{TOKEN}/sendMessage"), server)
    }

    fn http(status: u16, body: &str) -> String {
        format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
    }

    fn client() -> Client {
        Client(Client::builder().build().unwrap())
    }

    #[test]
    fn validates_secret_and_numeric_destination_without_echoing_input() {
        assert!(validate_token(TOKEN).is_ok());
        for token in [
            "",
            "0:ABCDEFGHIJKLMNOPQRSTUVWXYZ_abcdefghi",
            "123:short",
            "123:ABCDEFGHIJKLMNOPQRSTUVWXYZ/abcdefghi",
            "123:ABCDEFGHIJKLMNOPQRSTUVWXYZ?abcdefghi",
            "123:ABCDEFGHIJKLMNOPQRSTUVWXYZ\nabcdefghi",
        ] {
            assert_eq!(validate_token(token), Err(Error::InvalidToken));
        }
        assert!(Destination::new(TOKEN.into(), -1001234567890, Some(17)).is_ok());
        for chat_id in [0, i64::MIN, i64::MAX, MAX_CHAT_ID + 1] {
            assert!(matches!(
                Destination::new(TOKEN.into(), chat_id, None),
                Err(Error::InvalidChat)
            ));
        }
        for topic in [0, -1] {
            assert!(matches!(
                Destination::new(TOKEN.into(), 1, Some(topic)),
                Err(Error::InvalidTopic)
            ));
        }
    }

    #[tokio::test]
    async fn sends_plain_text_with_optional_topic_and_no_link_previews() {
        for topic in [None, Some(17)] {
            let (url, server) = serve(http(200, r#"{"ok":true,"result":{"message_id":42}}"#)).await;
            assert_eq!(
                client()
                    .send_to(&url, &destination(topic), "<b>Тест / 测试</b>")
                    .await,
                Ok(())
            );
            let request = server.await.unwrap();
            assert!(request.starts_with(&format!("POST /bot{TOKEN}/sendMessage HTTP/1.1\r\n")));
            let body: serde_json::Value =
                serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert_eq!(body["chat_id"], -1001234567890_i64);
            assert_eq!(
                body["message_thread_id"],
                topic.map_or(serde_json::Value::Null, Into::into)
            );
            assert_eq!(body["text"], "<b>Тест / 测试</b>");
            assert_eq!(body["link_preview_options"]["is_disabled"], true);
            assert!(body.get("parse_mode").is_none());
            assert!(!body.to_string().contains(TOKEN));
        }
    }

    #[tokio::test]
    async fn remote_errors_never_retain_the_token_url_or_description() {
        for (status, code, expected) in [
            (200, 400, Error::InvalidDestination),
            (401, 401, Error::Unauthorized),
            (403, 403, Error::Forbidden),
            (503, 503, Error::Unavailable),
        ] {
            let body = serde_json::json!({"ok": false, "error_code": code, "description": format!("secret {TOKEN} https://api.telegram.org/bot{TOKEN}/sendMessage")}).to_string();
            let (url, server) = serve(http(status, &body)).await;
            let error = client()
                .send_to(&url, &destination(None), "test")
                .await
                .unwrap_err();
            server.await.unwrap();
            assert_eq!(error, expected);
            assert!(!format!("{error:?} {error}").contains(TOKEN));
            assert!(std::error::Error::source(&error).is_none());
        }
    }

    #[tokio::test]
    async fn respects_flood_control_and_rejects_redirects() {
        let (url, server) = serve(http(
            429,
            r#"{"ok":false,"error_code":429,"parameters":{"retry_after":123456}}"#,
        ))
        .await;
        let error = client()
            .send_to(&url, &destination(None), "test")
            .await
            .unwrap_err();
        server.await.unwrap();
        assert_eq!(error.retry_after(), Some(Duration::from_secs(123456)));

        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (url, server) = serve(format!(
            "HTTP/1.1 302 Found\r\nLocation: http://{}/capture\r\nContent-Length: 0\r\n\r\n",
            target.local_addr().unwrap()
        ))
        .await;
        assert_eq!(
            client().send_to(&url, &destination(None), "test").await,
            Err(Error::Redirect)
        );
        server.await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), target.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn bounds_declared_and_streamed_responses() {
        for response in [
            format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", MAX_RESPONSE_BYTES + 1),
            format!("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n", MAX_RESPONSE_BYTES + 1, "x".repeat(MAX_RESPONSE_BYTES + 1)),
        ] {
            let (url, server) = serve(response).await;
            assert_eq!(client().send_to(&url, &destination(None), "test").await, Err(Error::ResponseTooLarge));
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn server_failure_does_not_wait_for_or_parse_its_body() {
        let (url, server) = serve(format!(
            "HTTP/1.1 503 Unavailable\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            MAX_RESPONSE_BYTES + 1
        ))
        .await;
        assert_eq!(
            client().send_to(&url, &destination(None), "test").await,
            Err(Error::Unavailable)
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn network_errors_and_input_rejection_are_safe() {
        // A malformed URL creates a real reqwest error carrying the secret;
        // no network request is needed to prove its public error is redacted.
        let url = format!("http://[invalid/bot{TOKEN}/sendMessage");
        let error = client()
            .send_to(&url, &destination(None), "test")
            .await
            .unwrap_err();
        assert_eq!(error, Error::Network);
        assert!(!format!("{error:?} {error}").contains(TOKEN));
        for text in [String::new(), "x".repeat(4097), "😀".repeat(2049)] {
            assert_eq!(
                client().send_to(&url, &destination(None), &text).await,
                Err(Error::InvalidText)
            );
        }
        let production = Client::new().unwrap();
        assert_eq!(
            production
                .send_to("http://127.0.0.1:1/", &destination(None), "test")
                .await,
            Err(Error::Network)
        );
    }

    #[test]
    fn malformed_or_false_success_is_not_acknowledged() {
        for body in [
            b"not json".as_slice(),
            br#"{"ok":false}"#,
            br#"{"ok":true}"#,
            br#"{"ok":true,"result":{"message_id":"wrong"}}"#,
        ] {
            assert_eq!(
                response_result(reqwest::StatusCode::OK, body),
                Err(Error::Protocol)
            );
        }
    }

    #[tokio::test]
    async fn trace_output_and_formatted_error_do_not_disclose_the_secret() {
        let logs = Logs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        tracing::info!("telegram-test-capture");
        let body =
            serde_json::json!({"ok": false, "error_code": 401, "description": TOKEN}).to_string();
        let (url, server) = serve(http(401, &body)).await;
        let error = client()
            .send_to(&url, &destination(None), "test")
            .await
            .unwrap_err();
        server.await.unwrap();
        tracing::warn!(error = %error, "delivery failed");
        let rendered = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
        assert!(rendered.contains("telegram-test-capture"));
        assert!(rendered.contains("telegram_unauthorized"));
        assert!(!rendered.contains(TOKEN));
        assert!(!rendered.contains("/sendMessage"));
    }

    #[tokio::test]
    async fn a_stalled_response_is_a_safe_retryable_timeout() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/bot{TOKEN}/sendMessage",
            listener.local_addr().unwrap()
        );
        let client = Client(
            Client::builder()
                .timeout(Duration::from_millis(30))
                .build()
                .unwrap(),
        );
        let error = client
            .send_to(&url, &destination(None), "test")
            .await
            .unwrap_err();
        assert_eq!(error, Error::Timeout);
        assert!(error.retry_after().is_some());
        assert!(std::error::Error::source(&error).is_none());
    }
}
