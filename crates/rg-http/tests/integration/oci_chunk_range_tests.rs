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

use crate::common::parked::assert_request_stays_blocked;
use crate::common::{
    create_repo, register_full, spawn_test_app_with_oci_root, spawn_test_app_with_state,
};

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

async fn put_range(
    base: &str,
    token: &str,
    location: &str,
    digest: &str,
    range: &str,
    payload: &[u8],
) -> reqwest::Response {
    reqwest::Client::new()
        .put(format!("{base}{location}"))
        .query(&[("digest", digest)])
        .bearer_auth(token)
        .header("content-range", range)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap()
}

async fn upload_row(
    db: &rg_db::DatabaseConnection,
    uuid: &str,
) -> rg_db::entities::oci_upload::Model {
    rg_db::ops::oci_ops::find_upload(db, uuid)
        .await
        .expect("read upload row")
        .expect("live upload row")
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

async fn open_raw_put(
    base: &str,
    token: &str,
    location: &str,
    digest: &str,
    range: &str,
    payload_len: usize,
    prefix: &[u8],
) -> tokio::net::TcpStream {
    let authority = base.strip_prefix("http://").expect("HTTP test base URL");
    let mut stream = tokio::net::TcpStream::connect(authority).await.unwrap();
    let headers = format!(
        "PUT {location}?digest={digest} HTTP/1.1\r\n\
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
async fn final_put_can_replay_an_acknowledged_range_without_doubling_it() {
    let (base, _repo_root, _oci_root) = spawn_test_app_with_oci_root().await;
    let client = reqwest::Client::new();
    let (token, _) = register_full(
        &base,
        "oci_final_range_retry",
        "oci_final_range_retry@example.com",
    )
    .await;
    create_repo(&base, &token, "retried-final-layer").await;

    let payload = b"the final PUT repeats bytes whose PATCH response was lost";
    let digest = sha256(payload);
    let range = format!("0-{}", payload.len() - 1);
    let (location, _) = start_upload(
        &base,
        &token,
        "oci_final_range_retry",
        "retried-final-layer",
    )
    .await;
    let accepted = patch_range(&base, &token, &location, &range, payload).await;
    assert_eq!(accepted.status(), 202);

    let complete = put_range(&base, &token, &location, &digest, &range, payload).await;
    crate::common::assert_blob_push_created(complete, payload.len()).await;

    let blob = client
        .get(format!(
            "{base}/v2/oci_final_range_retry/retried-final-layer/blobs/{digest}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(blob.status(), 200);
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
async fn final_put_accepts_the_next_range_and_refuses_out_of_order_without_mutation() {
    let (base, db, state) = spawn_test_app_with_state().await;
    let client = reqwest::Client::new();
    let (token, _) = register_full(&base, "oci_final_range", "oci_final_range@example.com").await;
    create_repo(&base, &token, "final-range-layer").await;

    let first_chunk = b"0123456789";
    let final_chunk = b"abcde";
    let payload = [first_chunk.as_slice(), final_chunk.as_slice()].concat();
    let digest = sha256(&payload);
    let (location, uuid) =
        start_upload(&base, &token, "oci_final_range", "final-range-layer").await;
    let staged = state
        .oci_storage
        .upload_file("oci_final_range", "final-range-layer", &uuid);

    let first = patch_range(&base, &token, &location, "0-9", first_chunk).await;
    assert_eq!(first.status(), 202);
    let before_row = upload_row(&db, &uuid).await;
    let before_bytes = std::fs::read(&staged).expect("read staged upload");

    for (range, chunk) in [
        ("15-19", final_chunk.as_slice()),
        ("5-14", b"56789abcde".as_slice()),
    ] {
        let refused = put_range(&base, &token, &location, &digest, range, chunk).await;
        assert_eq!(refused.status(), 416, "final range {range} must be refused");
        assert_eq!(
            refused
                .headers()
                .get(reqwest::header::RANGE)
                .and_then(|value| value.to_str().ok()),
            Some("0-9")
        );
        assert_eq!(
            upload_row(&db, &uuid).await,
            before_row,
            "refused final range {range} changed the session row"
        );
        assert_eq!(
            std::fs::read(&staged).expect("read staged upload after refusal"),
            before_bytes,
            "refused final range {range} changed staged bytes"
        );
    }

    let complete = put_range(&base, &token, &location, &digest, "10-14", final_chunk).await;
    crate::common::assert_blob_push_created(complete, payload.len()).await;

    let blob = client
        .get(format!(
            "{base}/v2/oci_final_range/final-range-layer/blobs/{digest}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(blob.status(), 200);
    assert_eq!(blob.bytes().await.unwrap().as_ref(), payload.as_slice());
}

#[tokio::test]
async fn monolithic_put_without_a_preceding_patch_still_publishes_the_blob() {
    let (base, _repo_root, _oci_root) = spawn_test_app_with_oci_root().await;
    let client = reqwest::Client::new();
    let (token, _) = register_full(
        &base,
        "oci_monolithic_put",
        "oci_monolithic_put@example.com",
    )
    .await;
    create_repo(&base, &token, "monolithic-layer").await;

    let payload = b"one body from POST straight to PUT";
    let digest = sha256(payload);
    let (location, _) = start_upload(&base, &token, "oci_monolithic_put", "monolithic-layer").await;
    let complete = client
        .put(format!("{base}{location}"))
        .query(&[("digest", digest.as_str())])
        .bearer_auth(&token)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap();
    crate::common::assert_blob_push_created(complete, payload.len()).await;
}

#[tokio::test]
async fn final_put_and_patch_on_one_session_do_not_interleave_bytes() {
    let (base, _repo_root, oci_root) = spawn_test_app_with_oci_root().await;
    let client = reqwest::Client::new();
    let (token, _) = register_full(
        &base,
        "oci_final_range_race",
        "oci_final_range_race@example.com",
    )
    .await;
    create_repo(&base, &token, "final-raced-layer").await;

    let payload = b"the final PUT pauses after its first byte while PATCH races it";
    let digest = sha256(payload);
    let range = format!("0-{}", payload.len() - 1);
    let (location, uuid) =
        start_upload(&base, &token, "oci_final_range_race", "final-raced-layer").await;
    let staged = oci_root
        .join("oci-uploads")
        .join("oci_final_range_race")
        .join("final-raced-layer")
        .join(uuid)
        .join("data");

    let final_put = open_raw_put(
        &base,
        &token,
        &location,
        &digest,
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
    .expect("the final PUT never reached the staging write");

    let retry = open_raw_patch(&base, &token, &location, &range, payload.len(), payload).await;
    let mut retry = tokio::spawn(async move { finish_raw_patch(retry, &[]).await });
    assert_request_stays_blocked(
        &mut retry,
        "PATCH completed while final PUT still owned the upload session",
    )
    .await;

    assert_eq!(finish_raw_patch(final_put, &payload[1..]).await, 201);
    assert_eq!(
        retry.await.unwrap(),
        404,
        "the serialized PATCH must observe that final PUT consumed the session"
    );

    let blob = client
        .get(format!(
            "{base}/v2/oci_final_range_race/final-raced-layer/blobs/{digest}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(blob.status(), 200);
    assert_eq!(blob.bytes().await.unwrap().as_ref(), payload);
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
    assert_request_stays_blocked(
        &mut second,
        "a retry completed while the first request still owned the same upload range",
    )
    .await;

    assert_eq!(finish_raw_patch(first, &payload[1..]).await, 202);
    assert_eq!(second.await.unwrap(), 202);
    assert_eq!(
        std::fs::read(staged).unwrap(),
        payload,
        "serialized retries must leave one copy of the chunk"
    );
}
