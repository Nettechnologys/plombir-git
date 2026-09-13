//! What a package publish costs the *process*, not the disk.
//!
//! Every other package test here would pass just as well against the handler
//! that collected its upload: buffering is correct, it is only expensive, and
//! the expense is invisible to any assertion about status codes, stored bytes
//! or response envelopes. The configured artifact ceiling is half a gigabyte by
//! default, nothing counts concurrent publishes, and the four publish protocols
//! all shared the same collect — so the number that has to be asserted is the
//! one the defect moves: the high-water mark of this process's resident set.
//!
//! The upload is streamed from one reused frame over a raw socket rather than
//! handed to a client that would collect it: if the *test* held the artifact,
//! the reading would be the test's own allocation and would say nothing about
//! the server.

use std::io::{Read as _, Write as _};

use crate::common::{register_full, spawn_test_app_with_db};

/// One frame of the streamed body.
const FRAME_BYTES: usize = 1024 * 1024;
/// How many frames the publish carries. 128 MiB is well under the 512 MiB
/// ceiling and far above anything the server legitimately keeps.
const FRAMES: usize = 128;

/// Peak resident set size of this process so far, in bytes.
///
/// The *peak*, not the current one: a collected body is back off the books by
/// the time the request finishes — a large allocation goes back to the kernel
/// on free — so a reading taken afterwards cannot tell a spool from a buffer.
/// `VmHWM` is the high-water mark, which is exactly the number the defect
/// moves. nextest runs every test in its own process, so the mark belongs to
/// this test alone.
fn peak_resident_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status
        .lines()
        .find(|line| line.starts_with("VmHWM:"))?
        .strip_prefix("VmHWM:")?;
    let kib: u64 = line.split_whitespace().next()?.parse().ok()?;
    Some(kib * 1024)
}

/// Send one request whose body is `FRAMES` copies of a single frame, written
/// straight onto the socket, and return the status line plus the response body.
///
/// Deliberately not `reqwest`: its `body()` takes the bytes, which would mean
/// the test process allocating the artifact it is asserting the server does not.
fn stream_request(base: &str, request_line_and_headers: String, frames: usize) -> (u16, String) {
    let authority = base
        .strip_prefix("http://")
        .expect("the test app is served over plain HTTP");
    let mut socket = std::net::TcpStream::connect(authority).expect("connect to the test app");
    socket
        .write_all(request_line_and_headers.as_bytes())
        .expect("write request head");

    let frame = vec![b'p'; FRAME_BYTES];
    for _ in 0..frames {
        socket.write_all(&frame).expect("write body frame");
    }
    socket.flush().expect("flush request");

    let mut response = Vec::new();
    socket
        .read_to_end(&mut response)
        .expect("read the response");
    let response = String::from_utf8_lossy(&response).into_owned();
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status line in response: {response}"));
    (status, response)
}

fn large_npm_tarball() -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().expect("create tarball spool");
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::none());
    let mut archive = tar::Builder::new(encoder);

    let package_json = br#"{"name":"npm-memory","version":"1.0.0"}"#;
    let mut manifest_header = tar::Header::new_gnu();
    manifest_header.set_mode(0o644);
    manifest_header.set_size(package_json.len() as u64);
    manifest_header.set_cksum();
    archive
        .append_data(
            &mut manifest_header,
            "package/package.json",
            package_json.as_slice(),
        )
        .expect("append package.json");

    let payload_len = (FRAME_BYTES * FRAMES) as u64;
    let mut payload_header = tar::Header::new_gnu();
    payload_header.set_mode(0o644);
    payload_header.set_size(payload_len);
    payload_header.set_cksum();
    archive
        .append_data(
            &mut payload_header,
            "package/payload.bin",
            std::io::repeat(b'p').take(payload_len),
        )
        .expect("append large payload");

    let encoder = archive.into_inner().expect("finish tar archive");
    let mut file = encoder.finish().expect("finish gzip stream");
    file.flush().expect("flush tarball spool");
    file
}

fn stream_npm_packument(
    base: &str,
    request_line_and_headers: String,
    prefix: &[u8],
    tarball: &std::path::Path,
    suffix: &[u8],
) -> (u16, String) {
    let authority = base
        .strip_prefix("http://")
        .expect("the test app is served over plain HTTP");
    let mut socket = std::net::TcpStream::connect(authority).expect("connect to the test app");
    socket
        .write_all(request_line_and_headers.as_bytes())
        .expect("write request head");
    socket.write_all(prefix).expect("write packument prefix");

    let mut tarball = std::fs::File::open(tarball).expect("open tarball spool");
    {
        let mut encoder = base64::write::EncoderWriter::new(
            &mut socket,
            &base64::engine::general_purpose::STANDARD,
        );
        std::io::copy(&mut tarball, &mut encoder).expect("stream base64 tarball");
        encoder.finish().expect("finish base64 tarball");
    }
    socket.write_all(suffix).expect("write packument suffix");
    socket.flush().expect("flush request");

    let mut response = Vec::new();
    socket
        .read_to_end(&mut response)
        .expect("read the response");
    let response = String::from_utf8_lossy(&response).into_owned();
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status line in response: {response}"));
    (status, response)
}

async fn create_repo(base: &str, token: &str, name: &str) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "is_private": false }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "create repository failed");
}

/// The measurement the whole change exists for.
///
/// It covers the entire publish path in one number — the ingress that stages
/// the body, the adapter that reads it, the digests, and the copy into blob
/// storage. Restoring any one of them to a `Vec<u8>` — the ingress'
/// `into_vec()`, a `to_bytes()` before `validate`, a `put` instead of
/// `put_file` — puts the artifact back in heap and turns this red.
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "reads VmHWM from /proc/self/status"
)]
#[tokio::test(flavor = "multi_thread")]
async fn publishing_a_large_artifact_does_not_grow_the_process_by_its_size() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _user_id) = register_full(&base, "pkg_memory", "pkg_memory@example.com").await;
    create_repo(&base, &token, "registry").await;

    let head = format!(
        "POST /api/v1/repos/pkg_memory/registry/packages/generic/publish\
         ?name=sample&version=1.0.0 HTTP/1.1\r\n\
         Host: {authority}\r\n\
         Authorization: Bearer {token}\r\n\
         Content-Disposition: attachment; filename=\"sample.bin\"\r\n\
         Content-Type: application/octet-stream\r\n\
         Content-Length: {length}\r\n\
         Connection: close\r\n\r\n",
        authority = base.strip_prefix("http://").unwrap(),
        length = FRAME_BYTES * FRAMES,
    );

    let before = peak_resident_bytes().expect("no /proc/self/status to measure against");
    let (status, body) = tokio::task::spawn_blocking({
        let base = base.clone();
        move || stream_request(&base, head, FRAMES)
    })
    .await
    .expect("streaming client task");
    let after = peak_resident_bytes().expect("no /proc/self/status to measure against");

    assert_eq!(status, 201, "publishing the large artifact failed: {body}");

    let grew = after.saturating_sub(before);
    let ceiling = (FRAME_BYTES * FRAMES / 4) as u64;
    assert!(
        grew < ceiling,
        "a {} MiB publish grew the process by {} MiB — the artifact is being collected, \
         not spooled",
        FRAME_BYTES * FRAMES / (1024 * 1024),
        grew / (1024 * 1024)
    );
}

/// npm hides the tarball inside a JSON string, so merely spooling the request
/// body is not enough: deserializing `data: String` and then base64-decoding it
/// creates two artifact-sized heap allocations. This sends the encoded string
/// straight from a file and asserts the production path keeps the decoded
/// tarball file-backed too.
#[cfg_attr(
    not(target_os = "linux"),
    ignore = "reads VmHWM from /proc/self/status"
)]
#[tokio::test(flavor = "multi_thread")]
async fn publishing_a_large_npm_packument_does_not_materialize_its_attachment() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _user_id) =
        register_full(&base, "npm_memory", "npm_package_memory@example.com").await;
    create_repo(&base, &token, "registry").await;

    let tarball = large_npm_tarball();
    let tarball_len = tarball.as_file().metadata().unwrap().len();
    let encoded_len = tarball_len.div_ceil(3) * 4;
    let prefix = br#"{"_id":"npm-memory","name":"npm-memory","dist-tags":{"latest":"1.0.0"},"versions":{"1.0.0":{"name":"npm-memory","version":"1.0.0"}},"_attachments":{"npm-memory-1.0.0.tgz":{"content_type":"application/octet-stream","data":""#;
    let suffix = format!(r#"","length":{tarball_len}}}}}}}"#);
    let request_len = prefix.len() as u64 + encoded_len + suffix.len() as u64;
    let head = format!(
        "PUT /api/v1/repos/npm_memory/registry/packages/npm/npm-memory HTTP/1.1\r\n\
         Host: {authority}\r\n\
         Authorization: Bearer {token}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {request_len}\r\n\
         Connection: close\r\n\r\n",
        authority = base.strip_prefix("http://").unwrap(),
    );

    let before = peak_resident_bytes().expect("no /proc/self/status to measure against");
    let (status, body) = tokio::task::spawn_blocking({
        let base = base.clone();
        let tarball_path = tarball.path().to_path_buf();
        let suffix = suffix.into_bytes();
        move || stream_npm_packument(&base, head, prefix, &tarball_path, &suffix)
    })
    .await
    .expect("streaming npm client task");
    let after = peak_resident_bytes().expect("no /proc/self/status to measure against");

    assert_eq!(status, 201, "publishing the npm packument failed: {body}");
    let grew = after.saturating_sub(before);
    let ceiling = tarball_len / 4;
    assert!(
        grew < ceiling,
        "a {} MiB npm tarball grew the process by {} MiB — the attachment was materialized",
        tarball_len / (1024 * 1024),
        grew / (1024 * 1024)
    );
}
