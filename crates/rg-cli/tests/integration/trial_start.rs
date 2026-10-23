//! `plombir-git trial` end to end, as the README's first command promises it:
//! nothing prepared beforehand, one command, then the setup link it prints,
//! a repository and a clone (card_794de0f08b9a).
//!
//! The image smoke in `.github/workflows/regression.yml` runs the same walk
//! against the container (`scripts/trial-smoke.sh`); this one runs on every
//! `nextest`, without Docker.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The address a link printed by this test's trial points at. It is never
/// dialled — the server listens on port 0 and publishes where it landed — so
/// it only has to be recognisable in the log.
const PUBLIC_URL: &str = "http://localhost:18080";

/// The HTTP port the server published in `--listen-address-file`.
fn published_http_port(address_file: &Path) -> u16 {
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if let Some(port) = std::fs::read_to_string(address_file).ok().and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("http="))
                .and_then(|addr| addr.rsplit(':').next())
                .and_then(|port| port.trim().parse().ok())
        }) {
            return port;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("the trial never published its listen addresses");
}

/// One HTTP/1.1 exchange with `Connection: close`: status and body.
fn http(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to the trial server");
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .expect("read timeout");
    let mut request =
        format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    if let Some(token) = token {
        request.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    let body = body.unwrap_or("");
    if !body.is_empty() {
        request.push_str("Content-Type: application/json\r\n");
    }
    request.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    stream
        .write_all(request.as_bytes())
        .expect("send the request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read the response");
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("not an HTTP response: {response:?}"));
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default();
    (status, body)
}

/// The value of `"key":"…"` in a flat JSON body — enough for the token and a
/// boolean, without a JSON dependency in this crate's tests.
fn json_field<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let rest = &body[body.find(&format!("\"{key}\":"))? + key.len() + 3..];
    let rest = rest.trim_start();
    if let Some(quoted) = rest.strip_prefix('"') {
        quoted.split('"').next()
    } else {
        rest.split([',', '}']).next().map(str::trim)
    }
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _killed = self.0.kill();
        let _reaped = self.0.wait();
    }
}

fn start(data_dir: &Path, address_file: &Path, lines: mpsc::Sender<String>) -> Server {
    let _stale = std::fs::remove_file(address_file);
    let mut child = Command::new(env!("CARGO_BIN_EXE_plombir-git"))
        .args([
            "trial",
            "--data-dir",
            data_dir.to_str().expect("UTF-8 data dir"),
            "--http-addr",
            "127.0.0.1:0",
            "--ssh-addr",
            "127.0.0.1:0",
            "--public-url",
            PUBLIC_URL,
            "--listen-address-file",
            address_file.to_str().expect("UTF-8 address file"),
        ])
        .env_remove("PLOMBIR_GIT_JWT_SECRET")
        .env_remove("PLOMBIR_GIT_ENCRYPTION_KEY")
        .env_remove("PLOMBIR_GIT_REGISTRATION")
        .env("RUST_LOG", "info")
        // Nothing beside the data directory: a trial must not need a checkout.
        .current_dir(data_dir.parent().expect("data dir parent"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start plombir-git trial");
    for stream in [
        Box::new(child.stdout.take().expect("stdout")) as Box<dyn Read + Send>,
        Box::new(child.stderr.take().expect("stderr")),
    ] {
        let lines = lines.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                if lines.send(line).is_err() {
                    break;
                }
            }
        });
    }
    Server(child)
}

/// Read the log until `wanted` matches a line, or fail with what was logged.
fn wait_for_line(
    lines: &mpsc::Receiver<String>,
    seen: &mut Vec<String>,
    wanted: impl Fn(&str) -> bool,
) -> String {
    let deadline = Instant::now() + Duration::from_secs(120);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match lines.recv_timeout(left) {
            Ok(line) => {
                seen.push(line.clone());
                if wanted(&line) {
                    return line;
                }
            }
            Err(_) => break,
        }
    }
    panic!(
        "the trial never logged the expected line. Log:\n{}",
        seen.join("\n")
    );
}

fn wait_until_serving(port: u16) {
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if TcpStream::connect(("127.0.0.1", port)).is_ok()
            && http(port, "GET", "/readyz", None, None).0 == 200
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    panic!("the trial server never became ready on port {port}");
}

#[test]
fn one_command_gives_a_setup_link_a_repository_and_a_clone() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let data_dir = dir.path().join("trial-data");
    let address_file = dir.path().join("listen-addresses");
    let (tx, rx) = mpsc::channel();
    let mut seen = Vec::new();
    let server = start(&data_dir, &address_file, tx);

    // The banner's link, exactly as a person would copy it.
    let prefix = format!("{PUBLIC_URL}/register?setup_token=");
    let link_line = wait_for_line(&rx, &mut seen, |line| line.contains(&prefix));
    // base64url: whatever follows it (a colour reset, a newline) is not token.
    let token: String = link_line[link_line.find(&prefix).unwrap() + prefix.len()..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect();
    assert_eq!(token.len(), 43, "a 32-byte base64url token: {link_line:?}");
    assert!(
        seen.iter().any(|line| line.contains("TRIAL MODE")),
        "the log must say this is a trial:\n{}",
        seen.join("\n")
    );
    let http_port = published_http_port(&address_file);
    wait_until_serving(http_port);

    // Everything the instance needs was created in the data directory, the
    // secret-holding files owner-only.
    for file in [
        "plombir-git.toml",
        "ssh_host_key",
        "encryption_key",
        "setup_token",
        "plombir-git.db",
    ] {
        assert!(
            data_dir.join(file).is_file(),
            "{file} missing from the data directory"
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for file in ["plombir-git.toml", "encryption_key", "setup_token"] {
            let mode = std::fs::metadata(data_dir.join(file))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{file} is {mode:o}");
        }
    }

    let (status, info) = http(http_port, "GET", "/api/v1/instance", None, None);
    assert_eq!(status, 200, "{info}");
    assert_eq!(json_field(&info, "trial_mode"), Some("true"), "{info}");
    assert_eq!(json_field(&info, "setup_required"), Some("true"), "{info}");

    // Registration is closed: a stranger without the token gets nowhere.
    let stranger =
        r#"{"username":"stranger","email":"stranger@example.com","password":"Zq8$trialPass"}"#;
    let (status, body) = http(
        http_port,
        "POST",
        "/api/v1/users/register",
        None,
        Some(stranger),
    );
    assert_eq!(status, 403, "{body}");

    let founder = format!(
        r#"{{"username":"founder","email":"founder@example.com","password":"Zq8$trialPass","setup_token":"{token}"}}"#
    );
    let (status, body) = http(
        http_port,
        "POST",
        "/api/v1/users/register",
        None,
        Some(&founder),
    );
    assert!(
        status == 200 || status == 201,
        "register with the setup token: {status} {body}"
    );
    let jwt = json_field(&body, "token")
        .expect("a session token")
        .to_owned();

    let (status, body) = http(
        http_port,
        "POST",
        "/api/v1/repos",
        Some(&jwt),
        Some(r#"{"name":"hello","auto_init":true}"#),
    );
    assert!(
        status == 200 || status == 201,
        "create repository: {status} {body}"
    );

    let clone = dir.path().join("hello");
    // Through the workspace's git gateway, as every other clone test does: its
    // environment is the hermetic one, not whatever the developer's shell has.
    let gateway = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway must initialize");
    let url = format!("http://127.0.0.1:{http_port}/git/founder/hello");
    let output = gateway
        .run(
            &["clone", &url, clone.to_str().expect("UTF-8 clone path")],
            None,
        )
        .expect("run git clone");
    assert!(
        output.success(),
        "git clone failed: {}",
        output.stderr_str().trim()
    );
    assert!(clone.join(".git").is_dir());

    // A second start reuses the same instance: same secret, same account.
    let config_before = std::fs::read_to_string(data_dir.join("plombir-git.toml")).unwrap();
    drop(server);
    let (tx, rx) = mpsc::channel();
    let mut seen = Vec::new();
    let _again = start(&data_dir, &address_file, tx);
    wait_for_line(&rx, &mut seen, |line| line.contains("TRIAL MODE"));
    let http_port = published_http_port(&address_file);
    wait_until_serving(http_port);
    assert_eq!(
        std::fs::read_to_string(data_dir.join("plombir-git.toml")).unwrap(),
        config_before,
        "a restart must not mint a new secret"
    );
    let (status, body) = http(
        http_port,
        "GET",
        "/api/v1/repos/founder/hello",
        Some(&jwt),
        None,
    );
    assert_eq!(
        status, 200,
        "the first start's session and repository survive a restart: {body}"
    );
    assert!(
        !seen.iter().any(|line| line.contains("setup_token=")),
        "no setup link once the administrator exists:\n{}",
        seen.join("\n")
    );
}
