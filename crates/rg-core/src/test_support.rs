//! Test-only fixtures shared by more than one service module.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

/// A remote that speaks HTTP Basic: 401 until an `Authorization` header shows
/// up, then 403 so `git` stops instead of retrying. Returns the bound address
/// and the list of credentials the remote actually received.
///
/// Both outbound-credential paths — mirror sync and repository import — prove
/// the same thing with it: that the secret we stored actually reaches the
/// remote, rather than being kept and then dropped on the floor.
pub fn spawn_authenticating_remote() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("addr").to_string();
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut authorization = None;
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
                if let Some(value) = line
                    .strip_prefix("Authorization: ")
                    .or_else(|| line.strip_prefix("authorization: "))
                {
                    authorization = Some(value.trim().to_string());
                }
            }
            let response = match authorization {
                Some(value) => {
                    recorder.lock().expect("lock").push(value);
                    "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                }
                None => {
                    "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"forgekeep\"\
                     \r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                }
            };
            if stream.write_all(response.as_bytes()).is_ok() {
                drop(stream.flush());
            }
        }
    });

    (address, seen)
}
