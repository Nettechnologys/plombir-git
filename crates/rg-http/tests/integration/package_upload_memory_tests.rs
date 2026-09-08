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
