//! Data migration import — GitHub / GitLab → ForgeKeep.
//!
//! Supports importing repositories and their metadata (issues, PRs,
//! labels, milestones, releases, wiki) from external platforms.

pub mod github_client;
pub mod gitlab_client;
pub mod service;
pub mod trust;

/// A raw-socket HTTP server for the pagination tests of both clients.
///
/// Raw rather than a mock-HTTP crate on purpose: the state under test is a
/// header value that is present and *undecodable*, and no typed builder will
/// let you construct one — `HeaderValue` accepts obs-text and `to_str` is what
/// refuses it, so the bytes have to go onto the wire by hand.
#[cfg(test)]
pub(crate) mod pagination_test_server {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// One `200 OK` with `headers` spliced in verbatim and `body` as JSON.
    pub(crate) fn respond(headers: &[u8], body: &str) -> Vec<u8> {
        let mut out = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n".to_vec();
        out.extend_from_slice(headers);
        out.extend_from_slice(
            format!(
                "Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(body.as_bytes());
        out
    }

    /// Answer one connection per entry of `responses`, in order, and return the
    /// request head each one carried — so a test can assert not just what came
    /// back but how many pages were actually asked for, and for which URLs.
    pub(crate) async fn serve(listener: TcpListener, responses: Vec<Vec<u8>>) -> Vec<String> {
        let mut requests = Vec::new();
        for response in responses {
            let accepted =
                tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept()).await;
            let Ok(Ok((mut stream, _))) = accepted else {
                break;
            };
            requests.push(read_head(&mut stream).await);
            stream.write_all(&response).await.expect("write response");
            // Half-close so the client sees the body end without waiting on a
            // keep-alive it was told (`Connection: close`) not to expect.
            stream.shutdown().await.expect("close connection");
        }
        requests
    }

    async fn read_head(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = stream.read(&mut buffer).await.expect("read request");
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// Deterministic DNS answers shared by the GitHub/GitLab connector tests.
#[cfg(test)]
pub(crate) mod api_client_test_support {
    use std::collections::VecDeque;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    pub(crate) struct SequencedResolver {
        answers: Mutex<VecDeque<Vec<SocketAddr>>>,
        calls: Arc<AtomicUsize>,
    }

    impl SequencedResolver {
        pub(crate) fn new(answers: Vec<Vec<SocketAddr>>, calls: Arc<AtomicUsize>) -> Self {
            Self {
                answers: Mutex::new(answers.into()),
                calls,
            }
        }
    }

    impl reqwest::dns::Resolve for SequencedResolver {
        fn resolve(&self, _name: reqwest::dns::Name) -> reqwest::dns::Resolving {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let answer = self
                .answers
                .lock()
                .expect("resolver answers lock")
                .pop_front()
                .unwrap_or_default();
            Box::pin(async move { Ok(Box::new(answer.into_iter()) as reqwest::dns::Addrs) })
        }
    }
}
