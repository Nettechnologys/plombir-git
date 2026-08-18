use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

#[allow(dead_code)]
mod rust_source {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/support/rust_source.rs"
    ));
}

const FORBIDDEN_SSH_SERVER_CALLS: [&str; 3] =
    ["wait_for_listener", "TcpListener::bind", "start_ssh_server"];

fn forbidden_ssh_server_calls(source: &str) -> Vec<(&'static str, usize)> {
    FORBIDDEN_SSH_SERVER_CALLS
        .into_iter()
        .flat_map(|name| {
            rust_source::call_sites(source, &[name])
                .into_iter()
                .map(move |call| (name, call.line))
        })
        .collect()
}

pub struct AcceptAnyServer;

impl russh::client::Handler for AcceptAnyServer {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::ssh_key::PublicKey,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

pub type Client = russh::client::Handle<AcceptAnyServer>;

/// A test SSH server whose socket is bound before its task is spawned.
///
/// The old harness reserved an ephemeral port with a temporary listener,
/// dropped it, and then asked a spawned task to bind the same address. Another
/// concurrent test could claim the port in that gap. Its TCP-only readiness
/// probe would then bless the wrong listener, and the first real SSH handshake
/// could be reset when that other test stopped its server.
pub struct TestSshServer {
    addr: String,
    task: tokio::task::JoinHandle<()>,
    stopped: watch::Receiver<Option<String>>,
}

impl TestSshServer {
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Connect with a real SSH handshake and preserve the server-task failure
    /// in the panic instead of hiding arbitrary resets behind a retry loop.
    pub async fn connect(&self) -> Client {
        let mut stopped = self.stopped.clone();
        let connect = russh::client::connect(
            Arc::new(russh::client::Config::default()),
            self.addr.clone(),
            AcceptAnyServer,
        );
        tokio::pin!(connect);

        tokio::select! {
            result = &mut connect => match result {
                Ok(client) => client,
                Err(error) => {
                    let server_state = stopped.borrow().clone().unwrap_or_else(|| {
                        if self.task.is_finished() {
                            "server task finished without publishing its result".to_string()
                        } else {
                            "server task is still running".to_string()
                        }
                    });
                    panic!(
                        "SSH handshake with {} failed: {error}; {server_state}",
                        self.addr
                    );
                }
            },
            changed = stopped.changed() => {
                let server_state = if changed.is_ok() {
                    stopped.borrow().clone().unwrap_or_else(|| {
                        "server task stopped without an error message".to_string()
                    })
                } else {
                    "server task result channel closed unexpectedly".to_string()
                };
                panic!(
                    "SSH server stopped before the handshake with {} completed: {server_state}",
                    self.addr
                );
            }
            _ = tokio::time::sleep(Duration::from_secs(10)) => {
                let server_state = stopped.borrow().clone().unwrap_or_else(|| {
                    if self.task.is_finished() {
                        "server task finished without publishing its result".to_string()
                    } else {
                        "server task is still running".to_string()
                    }
                });
                panic!(
                    "SSH handshake with {} did not complete within 10s; {server_state}",
                    self.addr
                );
            }
        }
    }

    pub fn abort(&self) {
        self.task.abort();
    }
}

impl Drop for TestSshServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn spawn_ssh_server(mut config: rg_ssh::SshServerConfig) -> TestSshServer {
    config.listen_addr = "127.0.0.1:0".to_string();
    let listener = tokio::net::TcpListener::bind(&config.listen_addr)
        .await
        .expect("bind test SSH listener");
    let addr = listener
        .local_addr()
        .expect("read test SSH listener address")
        .to_string();
    config.listen_addr = addr.clone();

    let (stopped_tx, stopped) = watch::channel(None);
    let task = tokio::spawn(async move {
        let message = match rg_ssh::start_ssh_server_on_listener(config, listener).await {
            Ok(()) => "test SSH server returned unexpectedly".to_string(),
            Err(error) => format!("test SSH server failed: {error:#}"),
        };
        drop(stopped_tx.send(Some(message)));
    });

    TestSshServer {
        addr,
        task,
        stopped,
    }
}

#[test]
fn every_ssh_integration_module_uses_the_shared_server_contract() {
    let integration_dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/integration");

    for entry in std::fs::read_dir(integration_dir).expect("read SSH integration directory") {
        let path = entry.expect("read SSH integration entry").path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs")
            || matches!(file_name, "common.rs" | "main.rs")
        {
            continue;
        }

        let source = std::fs::read_to_string(&path).expect("read SSH integration module");
        let forbidden_calls = forbidden_ssh_server_calls(&source);
        assert!(
            forbidden_calls.is_empty(),
            "{file_name} bypasses the shared race-free SSH server contract at \
             {forbidden_calls:?}"
        );
    }
}

fn ssh_integration_policy_fixture_contract(source: &str) -> Result<(), String> {
    let found: Vec<_> = forbidden_ssh_server_calls(source)
        .into_iter()
        .map(|(name, _)| name)
        .collect();
    let expected = FORBIDDEN_SSH_SERVER_CALLS.to_vec();
    if found != expected {
        return Err(format!(
            "expected exactly the live forbidden calls {expected:?}, found {found:?}"
        ));
    }
    Ok(())
}

fn without_live_forbidden_call(source: &str, name: &str) -> String {
    let calls = rust_source::call_sites(source, &[name]);
    let [call] = calls.as_slice() else {
        panic!(
            "fixture must contain exactly one live `{name}` call, found {}",
            calls.len()
        );
    };
    let name_at = source[..call.open_paren]
        .rfind(name)
        .expect("call name must precede its opening parenthesis");
    let mut mutated = source.to_owned();
    mutated.replace_range(name_at..name_at + name.len(), &"_".repeat(name.len()));
    mutated
}

#[test]
fn ssh_integration_policy_guard_ignores_non_code_decoys() {
    const SOURCE: &str = r####"
fn integration_test() {
    // wait_for_listener(); TcpListener::bind(addr); start_ssh_server(config);
    /* wait_for_listener(); TcpListener::bind(addr); start_ssh_server(config); */
    let normal = "wait_for_listener(); TcpListener::bind(addr); start_ssh_server(config);";
    let raw = r#"wait_for_listener(); TcpListener::bind(addr); start_ssh_server(config);"#;
    let bytes = b"wait_for_listener(); TcpListener::bind(addr); start_ssh_server(config);";
    let raw_bytes = br##"wait_for_listener(); TcpListener::bind(addr); start_ssh_server(config);"##;

    wait_for_listener();
    tokio::net::TcpListener::bind(addr);
    rg_ssh::start_ssh_server(config);
}
"####;

    ssh_integration_policy_fixture_contract(SOURCE).unwrap_or_else(|error| panic!("{error}"));

    for name in FORBIDDEN_SSH_SERVER_CALLS {
        let mutated = without_live_forbidden_call(SOURCE, name);
        assert!(
            ssh_integration_policy_fixture_contract(&mutated).is_err(),
            "removing the live `{name}` call must fail the fixture contract despite retained \
             non-code decoys"
        );
    }
}
