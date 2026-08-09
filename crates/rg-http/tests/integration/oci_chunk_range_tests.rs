//! A retried OCI chunk is the same upload, not new bytes.
//!
//! A client may send a `PATCH` successfully and lose only the `202` response.
//! Its retry carries the same inclusive `Content-Range`; appending that body a
//! second time doubles the staging file and makes the final digest look like a
//! client error. These tests drive the live HTTP route and inspect the staged
//! bytes, so a response-only implementation cannot pass.

use sha2::Digest as _;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::common::{create_repo, register_full, spawn_test_app_with_oci_root};

fn sha256(payload: &[u8]) -> String {
    format!("sha256:{}", hex::encode(sha2::Sha256::digest(payload)))
}

async fn start_upload(base: &str, token: &str, owner: &str, repo: &str) -> (String, String) {
    let response = reqwest::Client::new()
        .post(format!("{base}/v2/{owner}/{repo}/blobs/uploads/"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202, "start upload failed");
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .expect("start upload must return a Location")
        .to_string();
    let uuid = location
        .rsplit('/')
        .next()
        .expect("upload uuid in Location")
        .to_string();
    (location, uuid)
}

async fn patch_range(
    base: &str,
    token: &str,
    location: &str,
    range: &str,
    payload: &[u8],
) -> reqwest::Response {
    reqwest::Client::new()
        .patch(format!("{base}{location}"))
        .bearer_auth(token)
        .header("content-range", range)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap()
}

async fn open_raw_patch(
    base: &str,
    token: &str,
    location: &str,
    range: &str,
    payload_len: usize,
    prefix: &[u8],
) -> tokio::net::TcpStream {
    let authority = base.strip_prefix("http://").expect("HTTP test base URL");
    let mut stream = tokio::net::TcpStream::connect(authority).await.unwrap();
    let headers = format!(
        "PATCH {location} HTTP/1.1\r\n\
         Host: {authority}\r\n\
         Authorization: Bearer {token}\r\n\
         Content-Type: application/octet-stream\r\n\
         Content-Range: {range}\r\n\
         Content-Length: {payload_len}\r\n\
         Connection: close\r\n\r\n"
    );
    stream.write_all(headers.as_bytes()).await.unwrap();
    stream.write_all(prefix).await.unwrap();
    stream.flush().await.unwrap();
    stream
}

async fn finish_raw_patch(mut stream: tokio::net::TcpStream, suffix: &[u8]) -> u16 {
    stream.write_all(suffix).await.unwrap();
    stream.flush().await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    let status_line = std::str::from_utf8(&response)
        .unwrap()
        .lines()
        .next()
        .expect("HTTP status line");
    status_line
        .split_ascii_whitespace()
        .nth(1)
        .expect("HTTP status")
        .parse()
        .unwrap()
}

#[tokio::test]
async fn retrying_the_same_chunk_is_idempotent_and_the_blob_stays_exact() {
    let (base, _repo_root, oci_root) = spawn_test_app_with_oci_root().await;
    let client = reqwest::Client::new();
    let (token, _) = register_full(&base, "oci_range_retry", "oci_range_retry@example.com").await;
    create_repo(&base, &token, "retry-layer").await;

    let payload = b"one network-safe OCI chunk";
    let range = format!("0-{}", payload.len() - 1);
    let (location, uuid) = start_upload(&base, &token, "oci_range_retry", "retry-layer").await;
    let staged = oci_root
        .join("oci-uploads")
        .join("oci_range_retry")
        .join("retry-layer")
        .join(&uuid)
        .join("data");

    let first = patch_range(&base, &token, &location, &range, payload).await;
    assert_eq!(first.status(), 202, "first chunk must be accepted");
    assert_eq!(
        first
            .headers()
            .get(reqwest::header::RANGE)
            .and_then(|value| value.to_str().ok()),
        Some(range.as_str())
    );
    assert_eq!(std::fs::read(&staged).unwrap(), payload);

    // The registry accepted the first request, but the client did not observe
    // its response and sent the byte-identical range again.
    let retry = patch_range(&base, &token, &location, &range, payload).await;
    assert_eq!(retry.status(), 202, "an exact retry must be idempotent");
    assert_eq!(
        retry
            .headers()
            .get(reqwest::header::RANGE)
            .and_then(|value| value.to_str().ok()),
        Some(range.as_str())
    );
    assert_eq!(
        std::fs::read(&staged).unwrap(),
        payload,
        "the retry must not append a second copy to staging"
    );

    let digest = sha256(payload);
    let finish = client
        .put(format!("{base}{location}"))
        .query(&[("digest", digest.as_str())])
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    crate::common::assert_blob_push_created(finish, payload.len()).await;

    let blob = client
        .get(format!(
            "{base}/v2/oci_range_retry/retry-layer/blobs/{digest}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(blob.status(), 200);
    assert_eq!(blob.content_length(), Some(payload.len() as u64));
    assert_eq!(blob.bytes().await.unwrap().as_ref(), payload);
}

#[tokio::test]
async fn a_partially_overlapping_chunk_is_refused_without_touching_staging() {
    let (base, _repo_root, oci_root) = spawn_test_app_with_oci_root().await;
    let client = reqwest::Client::new();
    let (token, _) =
        register_full(&base, "oci_range_overlap", "oci_range_overlap@example.com").await;
    create_repo(&base, &token, "overlap-layer").await;

    let accepted = b"0123456789";
    let (location, uuid) = start_upload(&base, &token, "oci_range_overlap", "overlap-layer").await;
    let staged = oci_root
        .join("oci-uploads")
        .join("oci_range_overlap")
        .join("overlap-layer")
        .join(uuid)
        .join("data");

    let first = patch_range(&base, &token, &location, "0-9", accepted).await;
    assert_eq!(first.status(), 202);

    let unpositioned = client
        .patch(format!("{base}{location}"))
        .bearer_auth(&token)
        .body(b"cannot tell whether this is new or retried".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(
        unpositioned.status(),
        416,
        "an unpositioned PATCH is only safe while the upload is empty"
    );
    assert_eq!(std::fs::read(&staged).unwrap(), accepted);

    let overlap = patch_range(&base, &token, &location, "5-14", b"56789abcde").await;
    assert_eq!(
        overlap.status(),
        416,
        "a range that begins inside accepted bytes and extends past them is out of order"
    );
    assert_eq!(
        overlap
            .headers()
            .get(reqwest::header::RANGE)
            .and_then(|value| value.to_str().ok()),
        Some("0-9"),
        "the refusal must tell the client the current accepted offset"
    );
    assert_eq!(
        std::fs::read(&staged).unwrap(),
        accepted,
        "a refused overlap must not change staging"
    );

    let digest = sha256(accepted);
    let finish = client
        .put(format!("{base}{location}"))
        .query(&[("digest", digest.as_str())])
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    crate::common::assert_blob_push_created(finish, accepted.len()).await;
}

#[tokio::test]
async fn simultaneous_retries_of_one_range_are_serialized() {
    let (base, _repo_root, oci_root) = spawn_test_app_with_oci_root().await;
    let (token, _) = register_full(&base, "oci_range_race", "oci_range_race@example.com").await;
    create_repo(&base, &token, "raced-layer").await;

    let payload = b"the first request deliberately pauses after this first byte";
    let range = format!("0-{}", payload.len() - 1);
    let (location, uuid) = start_upload(&base, &token, "oci_range_race", "raced-layer").await;
    let staged = oci_root
        .join("oci-uploads")
        .join("oci_range_race")
        .join("raced-layer")
        .join(uuid)
        .join("data");

    // Hold the first handler inside its body stream after it has passed the
    // offset check and written one byte. A concurrent retry must wait at the
    // session boundary; otherwise it observes or appends half a request.
    let first = open_raw_patch(
        &base,
        &token,
        &location,
        &range,
        payload.len(),
        &payload[..1],
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(std::fs::metadata(&staged).map(|meta| meta.len()), Ok(1)) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the first PATCH never reached the staging write");

    let second_base = base.clone();
    let second_token = token.clone();
    let second_location = location.clone();
    let second_range = range.clone();
    let mut second = tokio::spawn(async move {
        let stream = open_raw_patch(
            &second_base,
            &second_token,
            &second_location,
            &second_range,
            payload.len(),
            payload,
        )
        .await;
        finish_raw_patch(stream, &[]).await
    });
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut second)
            .await
            .is_err(),
        "a retry completed while the first request still owned the same upload range"
    );

    assert_eq!(finish_raw_patch(first, &payload[1..]).await, 202);
    assert_eq!(second.await.unwrap(), 202);
    assert_eq!(
        std::fs::read(staged).unwrap(),
        payload,
        "serialized retries must leave one copy of the chunk"
    );
}
