//! card_51c3ddd4c3e0: the package download served stored bytes without ever
//! comparing them to the `sha256` recorded at publish.
//!
//! That digest is not bookkeeping. This same server publishes it to clients as
//! the install checksum — `sha256_of(f)` in the npm, PyPI, RubyGems and Helm
//! indexes — so a client decides whether to install a package by it. Serving
//! bytes that do not match it does not make the mismatch go away: it moves it to
//! the client, where it surfaces as `npm ERR! EINTEGRITY` and reads as a broken
//! registry or a broken client, while the operator sees a clean `200` and no log
//! line at all.
//!
//! Every neighbouring download path with a recorded digest already checks it
//! before handing the bytes over: release assets, CI artifacts, the CI cache,
//! and attachments. This is the one that did not.

use crate::common::{create_repo, register_full, spawn_test_app_with_db};
use futures::StreamExt as _;
use reqwest::StatusCode;
use sea_orm::ConnectionTrait;

const OWNER: &str = "package-integrity-owner";
const REPO: &str = "package-integrity-repo";
const BODY: &[u8] = b"the bytes that were published";
const LARGE_FRAME_BYTES: usize = 1024 * 1024;
const LARGE_FRAMES: usize = 256;

struct Published {
    base: String,
    db: rg_db::DatabaseConnection,
    client: reqwest::Client,
}

impl Published {
    fn download_url(&self) -> String {
        format!(
            "{}/api/v1/repos/{OWNER}/{REPO}/packages/generic/widget/1.0.0/widget.bin",
            self.base
        )
    }
}

/// Publish one generic package file through the real publish route, so the blob
/// lands in storage exactly the way a client's upload would.
async fn publish() -> Published {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _owner_id) =
        register_full(&base, OWNER, "package-integrity-owner@example.test").await;
    create_repo(&base, &token, REPO).await;
    let client = reqwest::Client::new();

    let mut publish_url = reqwest::Url::parse(&format!(
        "{base}/api/v1/repos/{OWNER}/{REPO}/packages/generic/publish"
    ))
    .unwrap();
    publish_url
        .query_pairs_mut()
        .append_pair("name", "widget")
        .append_pair("version", "1.0.0");

    let published = client
        .post(publish_url)
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"widget.bin\"",
        )
        .body(BODY.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(
        published.status(),
        StatusCode::CREATED,
        "publish fixture failed: {}",
        published.text().await.unwrap()
    );

    Published { base, db, client }
}

/// Publish a large body without ever constructing the whole package in the
/// test process. Reusing one immutable frame keeps the client's heap cost
/// constant while the production upload path spools the request to disk.
async fn publish_large() -> Published {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _owner_id) =
        register_full(&base, OWNER, "package-integrity-owner@example.test").await;
    create_repo(&base, &token, REPO).await;
    let client = reqwest::Client::new();

    let mut publish_url = reqwest::Url::parse(&format!(
        "{base}/api/v1/repos/{OWNER}/{REPO}/packages/generic/publish"
    ))
    .unwrap();
    publish_url
        .query_pairs_mut()
        .append_pair("name", "widget")
        .append_pair("version", "1.0.0");

    let frame = axum::body::Bytes::from(vec![b'p'; LARGE_FRAME_BYTES]);
    let stream = futures::stream::iter(
        (0..LARGE_FRAMES).map(move |_| Ok::<_, std::io::Error>(frame.clone())),
    );
    let published = client
        .post(publish_url)
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"widget.bin\"",
        )
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await
        .unwrap();
    assert_eq!(
        published.status(),
        StatusCode::CREATED,
        "large publish fixture failed: {}",
        published.text().await.unwrap()
    );

    Published { base, db, client }
}

fn peak_resident_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status
        .lines()
        .find(|line| line.starts_with("VmHWM:"))?
        .strip_prefix("VmHWM:")?;
    let kib: u64 = line.split_whitespace().next()?.parse().ok()?;
    Some(kib * 1024)
}

/// The healthy path, and the half of it that is new: the digest the bytes were
/// checked against is advertised, the way the release-asset, CI-artifact and
/// CI-cache handlers advertise theirs.
#[tokio::test]
async fn an_intact_package_downloads_and_advertises_the_digest_it_was_checked_against() {
    let fixture = publish().await;

    let response = fixture
        .client
        .get(fixture.download_url())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let advertised = response
        .headers()
        .get("x-checksum-sha256")
        .expect("a verified download must say what it was verified against")
        .to_str()
        .expect("the digest is hex")
        .to_string();
    let body = response.bytes().await.unwrap();
    assert_eq!(body.as_ref(), BODY);

    use sha2::Digest as _;
    assert_eq!(
        advertised,
        hex::encode(sha2::Sha256::digest(BODY)),
        "the advertised digest must be the digest of the bytes actually served"
    );
}

/// The defect itself. The stored bytes and the recorded digest disagree — which
/// is what storage rot, a restored-from-the-wrong-backup blob, or a swapped file
/// looks like from here — and the server must refuse rather than pass the
/// problem to the client as a valid `200`.
#[tokio::test]
async fn a_package_whose_bytes_no_longer_match_its_recorded_digest_is_refused() {
    let fixture = publish().await;

    // Break the agreement between the row and the blob. Which side "moved" makes
    // no difference to the handler: it recomputes the digest of what it read and
    // compares. Doing it through the row keeps the test independent of whichever
    // blob backend the harness is configured with.
    fixture
        .db
        .execute_unprepared(
            "UPDATE package_files SET sha256 = \
             'deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef' \
             WHERE filename = 'widget.bin'",
        )
        .await
        .expect("rewrite the recorded digest");

    let response = fixture
        .client
        .get(fixture.download_url())
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.bytes().await.unwrap();

    assert!(
        status.is_server_error(),
        "a package that fails its own integrity check must be a 5xx, got {status}"
    );
    assert_ne!(
        body.as_ref(),
        BODY,
        "the package bytes must not be served under a failing integrity check"
    );
}

/// Rows written before digest tracking carry no hash, and must stay downloadable
/// — the guard refuses a *contradiction*, not the absence of a claim. This is
/// the same allowance `release::service::download_asset` makes.
#[tokio::test]
async fn a_legacy_row_with_no_recorded_digest_is_still_served() {
    let fixture = publish().await;

    fixture
        .db
        .execute_unprepared("UPDATE package_files SET sha256 = NULL WHERE filename = 'widget.bin'")
        .await
        .expect("clear the recorded digest");

    let response = fixture
        .client
        .get(fixture.download_url())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response.headers().get("x-checksum-sha256").is_none(),
        "with nothing recorded there is nothing to advertise"
    );
    assert_eq!(response.bytes().await.unwrap().as_ref(), BODY);
}

/// The resource contract behind card_da0201010179, measured through the real
/// publish, service and HTTP response path. The client drains chunks without
/// collecting them, so a package-sized VmHWM jump can only come from the
/// server resolving the local blob through `get() -> Vec<u8>`.
#[cfg_attr(not(target_os = "linux"), ignore = "reads /proc/self/status")]
#[tokio::test(flavor = "multi_thread")]
async fn a_large_package_download_does_not_grow_the_process_by_its_size() {
    const TOTAL: usize = LARGE_FRAME_BYTES * LARGE_FRAMES;

    let fixture = publish_large().await;
    let before = peak_resident_bytes().expect("no /proc/self/status to measure against");

    let response = fixture
        .client
        .get(fixture.download_url())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.content_length(), Some(TOTAL as u64));

    let mut delivered = 0usize;
    let mut body = response.bytes_stream();
    while let Some(chunk) = body.next().await {
        delivered += chunk.unwrap().len();
    }
    assert_eq!(
        delivered, TOTAL,
        "the complete package must reach the client"
    );

    let after = peak_resident_bytes().expect("no /proc/self/status to measure against");
    let grew = after.saturating_sub(before);
    let ceiling = (TOTAL / 4) as u64;
    assert!(
        grew < ceiling,
        "a {} MiB package grew the process by {} MiB — the download was buffered instead of streamed",
        TOTAL / (1024 * 1024),
        grew / (1024 * 1024)
    );
}
