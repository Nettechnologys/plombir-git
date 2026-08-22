//! card_766653553e74: whose fault is it when the LDAP directory does not answer.
//!
//! `POST /api/v1/users/login` used to answer `401 invalid credentials` to every
//! failure of the directory leg — a refused connection, a service account the
//! directory no longer accepts, a bind that came back `busy`. The door itself is
//! built fail-closed: it enumerates the three *verdicts* the service may bail
//! with and turns anything else into a `5xx`. The defect was one layer above it,
//! in `login_via_ldap_inner`, which laundered every directory failure into the
//! literal string `invalid credentials` before the door could tell them apart.
//!
//! The cost is not just a wrong status code. That verdict is what makes the door
//! call `record_failed_login`, so a ten-minute directory outage spends five
//! strikes of every LDAP account's brute-force budget and locks them out for
//! fifteen minutes *past* the recovery — an outage that outlives its cause, and
//! that never appears in the operator's `5xx` rate.
//!
//! These drive the real `POST /users/login` against a mock directory whose
//! failure mode each test picks, because the split has to hold on the path a
//! person actually walks. Each outage test asserts both halves — the status and
//! the untouched counter — and is paired with the `401` that must survive it: a
//! directory that answered and said the password is wrong still rejects, and
//! still counts.

use crate::common::{build_test_app_state, setup_test_db, TEST_ENCRYPTION_KEY};
use axum::http::StatusCode;
use rg_db::ops::sso_provider_ops::SsoProviderInput;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const SERVICE_BIND_DN: &str = "cn=service,dc=example,dc=com";
const BASE_DN: &str = "dc=example,dc=com";
const USER_DN: &str = "uid=ldapuser,dc=example,dc=com";
const USERNAME: &str = "ldapuser";
const PASSWORD: &str = "directory-password";

// ── A directory just real enough to bind against ─────────────────
//
// `ldap3` speaks BER over a socket, so a mock has to as well: there is no HTTP
// layer to stub the way the SSO tests stub theirs. What follows is the smallest
// subset that carries `authenticate` end to end — bind, search, unbind — with
// the result code of each step chosen by the test.

/// What the mock directory does when ForgeKeep binds against it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    /// Everything answers; the login completes.
    Healthy,
    /// The directory answered, and its answer is about this password: `49`,
    /// `invalidCredentials`. The one failure here that is a verdict.
    WrongPassword,
    /// Nothing is listening at all.
    Unreachable,
    /// The directory is up and refuses the *forge's* service account — an
    /// expired bind password, a service DN somebody moved. Nothing to do with
    /// the person signing in.
    ServiceBindRefused,
    /// The password bind came back `51 busy`: the directory never got as far as
    /// judging the password.
    DirectoryBusy,
    /// The search the forge runs with its own credentials is refused.
    SearchRefused,
}

/// LDAP result codes used below (RFC 4511 §A.1).
const RC_SUCCESS: u8 = 0;
const RC_BUSY: u8 = 51;
const RC_INVALID_CREDENTIALS: u8 = 49;
const RC_INSUFFICIENT_ACCESS: u8 = 50;

fn ber_len(len: usize, out: &mut Vec<u8>) {
    if len < 0x80 {
        out.push(len as u8);
    } else if len < 0x100 {
        out.push(0x81);
        out.push(len as u8);
    } else {
        out.push(0x82);
        out.push((len >> 8) as u8);
        out.push(len as u8);
    }
}

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    ber_len(content.len(), &mut out);
    out.extend_from_slice(content);
    out
}

fn octet(value: &str) -> Vec<u8> {
    tlv(0x04, value.as_bytes())
}

fn integer(value: u32) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let first = bytes.iter().position(|byte| *byte != 0).unwrap_or(3);
    let mut body = bytes[first..].to_vec();
    // BER integers are signed: a leading bit set would read as negative.
    if body[0] & 0x80 != 0 {
        body.insert(0, 0);
    }
    tlv(0x02, &body)
}

/// `LDAPResult ::= resultCode, matchedDN, diagnosticMessage`.
fn ldap_result(rc: u8) -> Vec<u8> {
    let mut body = tlv(0x0a, &[rc]);
    body.extend(octet(""));
    body.extend(octet(""));
    body
}

fn message(message_id: u32, op_tag: u8, op_body: &[u8]) -> Vec<u8> {
    let mut body = integer(message_id);
    body.extend(tlv(op_tag, op_body));
    tlv(0x30, &body)
}

/// One `SearchResultEntry` for [`USER_DN`], carrying the attributes
/// `authenticate` reads back off it.
fn search_entry(message_id: u32) -> Vec<u8> {
    let attribute = |name: &str, value: &str| {
        let mut body = octet(name);
        body.extend(tlv(0x31, &octet(value)));
        tlv(0x30, &body)
    };
    let mut attributes = attribute("uid", USERNAME);
    attributes.extend(attribute("mail", "ldapuser@example.com"));
    attributes.extend(attribute("displayName", "LDAP User"));

    let mut body = octet(USER_DN);
    body.extend(tlv(0x30, &attributes));
    message(message_id, 0x64, &body)
}

/// Total on-wire size of the message at the head of `buf`, once enough of its
/// length header has arrived to know it.
fn framed_len(buf: &[u8]) -> Option<usize> {
    let first = *buf.get(1)?;
    if first & 0x80 == 0 {
        return Some(2 + first as usize);
    }
    let width = (first & 0x7f) as usize;
    let mut length = 0usize;
    for index in 0..width {
        length = (length << 8) | *buf.get(2 + index)? as usize;
    }
    Some(2 + width + length)
}

fn read_len(buf: &[u8], pos: &mut usize) -> usize {
    let first = buf[*pos];
    *pos += 1;
    if first & 0x80 == 0 {
        return first as usize;
    }
    let width = (first & 0x7f) as usize;
    let mut length = 0usize;
    for _ in 0..width {
        length = (length << 8) | buf[*pos] as usize;
        *pos += 1;
    }
    length
}

/// The bits of a request the mock actually branches on.
struct Request {
    message_id: u32,
    op_tag: u8,
    /// The DN of a `BindRequest`, so the service bind and the password bind can
    /// be told apart — they arrive on two different connections.
    bind_dn: String,
}

fn parse_request(msg: &[u8]) -> Option<Request> {
    let mut pos = 1usize;
    read_len(msg, &mut pos);
    // messageID
    if *msg.get(pos)? != 0x02 {
        return None;
    }
    pos += 1;
    let id_len = read_len(msg, &mut pos);
    let mut message_id = 0u32;
    for _ in 0..id_len {
        message_id = (message_id << 8) | *msg.get(pos)? as u32;
        pos += 1;
    }
    let op_tag = *msg.get(pos)?;
    pos += 1;
    read_len(msg, &mut pos);

    let mut bind_dn = String::new();
    if op_tag == 0x60 {
        // BindRequest ::= version INTEGER, name LDAPDN, authentication
        if *msg.get(pos)? == 0x02 {
            pos += 1;
            let version_len = read_len(msg, &mut pos);
            pos += version_len;
        }
        if *msg.get(pos)? == 0x04 {
            pos += 1;
            let dn_len = read_len(msg, &mut pos);
            bind_dn = String::from_utf8_lossy(msg.get(pos..pos + dn_len)?).to_string();
        }
    }
    Some(Request {
        message_id,
        op_tag,
        bind_dn,
    })
}

async fn serve_connection(mut socket: tokio::net::TcpStream, behaviour: Behaviour) {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let total = match framed_len(&buf) {
            Some(total) if buf.len() >= total => total,
            _ => {
                let mut chunk = [0u8; 1024];
                match socket.read(&mut chunk).await {
                    Ok(0) | Err(_) => return,
                    Ok(read) => {
                        buf.extend_from_slice(&chunk[..read]);
                        continue;
                    }
                }
            }
        };
        let msg: Vec<u8> = buf.drain(..total).collect();
        let Some(request) = parse_request(&msg) else {
            return;
        };
        let reply = match request.op_tag {
            // BindRequest
            0x60 => {
                let rc = if request.bind_dn == SERVICE_BIND_DN {
                    match behaviour {
                        Behaviour::ServiceBindRefused => RC_INVALID_CREDENTIALS,
                        _ => RC_SUCCESS,
                    }
                } else {
                    match behaviour {
                        Behaviour::WrongPassword => RC_INVALID_CREDENTIALS,
                        Behaviour::DirectoryBusy => RC_BUSY,
                        _ => RC_SUCCESS,
                    }
                };
                message(request.message_id, 0x61, &ldap_result(rc))
            }
            // SearchRequest
            0x63 => {
                if behaviour == Behaviour::SearchRefused {
                    message(
                        request.message_id,
                        0x65,
                        &ldap_result(RC_INSUFFICIENT_ACCESS),
                    )
                } else {
                    let mut reply = search_entry(request.message_id);
                    reply.extend(message(request.message_id, 0x65, &ldap_result(RC_SUCCESS)));
                    reply
                }
            }
            // UnbindRequest — no response, the client hangs up.
            0x42 => return,
            _ => return,
        };
        if socket.write_all(&reply).await.is_err() {
            return;
        }
    }
}

/// What the fixture puts in the database besides the working provider.
#[derive(Clone, Copy, Default)]
struct Fixture {
    /// Enable a second LDAP provider, ahead of the working one, whose stored
    /// row cannot be turned into a bindable config at all.
    broken_sibling: bool,
    /// Pre-create the LDAP account. Off for the first-login case, which has to
    /// provision one.
    account: bool,
}

struct Harness {
    base: String,
    db: rg_db::DatabaseConnection,
    provider_id: i64,
    user_id: Option<i64>,
    client: reqwest::Client,
    app_server: tokio::task::JoinHandle<()>,
    directory_server: Option<tokio::task::JoinHandle<()>>,
    _app_dir: tempfile::TempDir,
}

impl Harness {
    async fn start(behaviour: Behaviour) -> Harness {
        Harness::start_with(
            behaviour,
            Fixture {
                account: true,
                ..Default::default()
            },
        )
        .await
    }

    async fn start_with(behaviour: Behaviour, fixture: Fixture) -> Harness {
        // Keep ownership of the outage port and sever every connection before
        // an LDAP reply exists. This is a deterministic transport failure;
        // another test process cannot claim the address in between.
        let (directory_port, directory_server) = if behaviour == Behaviour::Unreachable {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    drop(stream);
                }
            });
            (port, Some(server))
        } else {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                loop {
                    let Ok((socket, _)) = listener.accept().await else {
                        return;
                    };
                    tokio::spawn(serve_connection(socket, behaviour));
                }
            });
            crate::common::wait_for_listener(&addr.to_string()).await;
            (addr.port(), Some(server))
        };

        let (db, app_dir) = setup_test_db().await;
        let repo_root = app_dir.path().join("repos");
        std::fs::create_dir_all(&repo_root).unwrap();

        let key = rg_core::auth::encryption::derive_key(TEST_ENCRYPTION_KEY);
        let bind_password_enc = rg_core::auth::encryption::encrypt("service-secret", &key).unwrap();

        // Inserted first so it sorts ahead of the working one — `list_enabled`
        // orders by id, and a sibling tried *after* the login already succeeded
        // would prove nothing.
        if fixture.broken_sibling {
            rg_db::ops::sso_provider_ops::upsert(
                &db,
                None,
                SsoProviderInput {
                    name: "Broken Directory",
                    slug: "broken",
                    provider_type: "ldap",
                    // No host: `ldap_config_from_provider` refuses this row
                    // before anything is dialled.
                    ldap_bind_dn: Some(SERVICE_BIND_DN),
                    ldap_bind_password_enc: Some(&bind_password_enc),
                    ldap_base_dn: Some(BASE_DN),
                    enabled: true,
                    auto_provision: true,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }

        let provider = rg_db::ops::sso_provider_ops::upsert(
            &db,
            None,
            SsoProviderInput {
                name: "Mock Directory",
                slug: "dir",
                provider_type: "ldap",
                ldap_host: Some("127.0.0.1"),
                ldap_port: Some(i32::from(directory_port)),
                ldap_bind_dn: Some(SERVICE_BIND_DN),
                ldap_bind_password_enc: Some(&bind_password_enc),
                ldap_base_dn: Some(BASE_DN),
                ldap_user_filter: Some("(uid={username})"),
                enabled: true,
                auto_provision: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let user_id = if fixture.account {
            Some(
                rg_db::ops::user_ops::create_ldap_user(
                    &db,
                    provider.id,
                    USERNAME,
                    "ldapuser@example.com",
                    Some("LDAP User"),
                    Some(USERNAME),
                )
                .await
                .unwrap()
                .id,
            )
        } else {
            None
        };

        let app = rg_http::create_router_for_test(build_test_app_state(db.clone(), repo_root));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let base = format!("http://{addr}");
        let app_server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        crate::common::wait_for_listener(&addr).await;

        Harness {
            base,
            db,
            provider_id: provider.id,
            user_id,
            client: reqwest::Client::new(),
            app_server,
            directory_server,
            _app_dir: app_dir,
        }
    }

    async fn sign_in(&self) -> (StatusCode, String) {
        let response = self
            .client
            .post(format!("{}/api/v1/users/login", self.base))
            .json(&serde_json::json!({ "login": USERNAME, "password": PASSWORD }))
            .send()
            .await
            .unwrap();
        let status = StatusCode::from_u16(response.status().as_u16()).unwrap();
        (status, response.text().await.unwrap())
    }

    /// The admin's "test connection" button, pressed by an instance admin of
    /// this instance. The admin is registered per call and named after the
    /// provider, so a test may press the button for more than one row.
    async fn press_test_button(&self, provider_id: i64) -> (StatusCode, String) {
        let (token, admin_id) = crate::common::register_full(
            &self.base,
            &format!("dir_admin_{provider_id}"),
            &format!("dir_admin_{provider_id}@example.com"),
        )
        .await;
        rg_db::ops::user_ops::update_by_id(&self.db, admin_id, None, None, Some(true), None)
            .await
            .unwrap()
            .expect("the registered admin must exist");

        let response = self
            .client
            .post(format!(
                "{}/api/v1/admin/sso/providers/{provider_id}/test",
                self.base
            ))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        let status = StatusCode::from_u16(response.status().as_u16()).unwrap();
        (status, response.text().await.unwrap())
    }

    /// Store an extra provider row and return its id.
    async fn add_provider(&self, input: SsoProviderInput<'_>) -> i64 {
        rg_db::ops::sso_provider_ops::upsert(&self.db, None, input)
            .await
            .unwrap()
            .id
    }

    /// Strikes on the brute-force counter of the account that tried to sign in.
    async fn login_attempts(&self) -> i32 {
        let user_id = self.user_id.expect("this fixture has no account");
        rg_db::ops::user_ops::find_by_id(&self.db, user_id)
            .await
            .unwrap()
            .expect("the fixture account must still exist")
            .login_attempts
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.app_server.abort();
        if let Some(server) = &self.directory_server {
            server.abort();
        }
    }
}

/// The baseline that makes every refusal below mean something: the identical
/// login completes when the directory is healthy. Without it, a handler that
/// answered `502` to everything would pass the outage tests.
#[tokio::test]
async fn a_healthy_directory_still_signs_in() {
    let harness = Harness::start(Behaviour::Healthy).await;

    let (status, body) = harness.sign_in().await;

    assert_eq!(
        status,
        StatusCode::OK,
        "the healthy bind must still sign in, got body: {body}"
    );
    assert_eq!(
        harness.login_attempts().await,
        0,
        "a successful sign-in leaves no strike"
    );
}

/// The defect: the directory transport fails before judging a password, and
/// the person signing in was told their password was wrong.
#[tokio::test]
async fn an_unreachable_directory_is_not_a_rejected_password() {
    let harness = Harness::start(Behaviour::Unreachable).await;

    let (status, body) = harness.sign_in().await;

    assert!(
        status.is_server_error(),
        "a directory nobody could reach must not be reported as a rejected credential, \
         got {status} with body: {body}"
    );
    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "the outage belongs to the host we do not own, got body: {body}"
    );
}

/// The half of the defect that outlives the outage: the strike.
#[tokio::test]
async fn an_unreachable_directory_costs_the_account_no_strike() {
    let harness = Harness::start(Behaviour::Unreachable).await;

    for _ in 0..3 {
        harness.sign_in().await;
    }

    assert_eq!(
        harness.login_attempts().await,
        0,
        "three attempts against a dead directory must not spend the account's \
         brute-force budget — five of them would lock it for fifteen minutes \
         past the recovery"
    );
}

/// Same failure one layer up: the directory answers, and refuses the *forge's*
/// service account. The person signing in has nothing to do with it.
#[tokio::test]
async fn a_refused_service_bind_is_not_the_callers_fault() {
    let harness = Harness::start(Behaviour::ServiceBindRefused).await;

    let (status, body) = harness.sign_in().await;

    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "our own expired service credential is not their wrong password, got body: {body}"
    );
    assert_eq!(harness.login_attempts().await, 0);
}

/// And the search leg, which also runs with the forge's credentials.
#[tokio::test]
async fn a_refused_search_is_not_the_callers_fault() {
    let harness = Harness::start(Behaviour::SearchRefused).await;

    let (status, body) = harness.sign_in().await;

    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "a search the directory refused to run judged nobody's password, got body: {body}"
    );
    assert_eq!(harness.login_attempts().await, 0);
}

/// The subtle one: the password bind *completed* and came back `51 busy`. A
/// classification that only checks "is the result code zero" reads that as a
/// rejection, which is exactly the mistake being fixed one level down.
#[tokio::test]
async fn a_busy_directory_is_not_a_rejected_password() {
    let harness = Harness::start(Behaviour::DirectoryBusy).await;

    let (status, body) = harness.sign_in().await;

    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "`busy` means the bind never judged the password, got body: {body}"
    );
    assert_eq!(harness.login_attempts().await, 0);
}

/// The skip that has to survive the change: a provider whose stored row cannot
/// be turned into a bindable config at all is still passed over, and the next
/// provider still signs the person in. Configuration is read before anything is
/// dialled, so that failure judged nobody and reached nobody — treating it as an
/// outage would let one mistyped admin form take down every LDAP login on the
/// instance.
#[tokio::test]
async fn a_misconfigured_provider_is_still_skipped_for_a_working_one() {
    let harness = Harness::start_with(
        Behaviour::Healthy,
        Fixture {
            broken_sibling: true,
            // First login: the multi-provider loop only runs for an account the
            // forge has not seen, since a known one is pinned to its own
            // provider.
            account: false,
        },
    )
    .await;

    let (status, body) = harness.sign_in().await;

    assert_eq!(
        status,
        StatusCode::OK,
        "a broken provider ahead of a working one must not stop the login, got body: {body}"
    );
    assert!(
        rg_db::ops::user_ops::find_by_username(&harness.db, USERNAME)
            .await
            .unwrap()
            .is_some(),
        "the working provider's bind must still have provisioned the account"
    );
}

// ── The admin's "test connection" button ─────────────────────────
//
// card_a86f0776021c: the same split, one door further in. `POST
// /admin/sso/providers/{id}/test` answered `400` to both halves of its own
// question — "you asked me to test something that is not an LDAP provider" and
// "the directory did not answer" — which is the one answer a diagnostic button
// must never give: it exists precisely to say which of the two happened, and
// `400` tells the admin to fix a request that was already correct.

/// The baseline that makes the refusals below mean something: pressing the
/// button against a directory that answers reports success. Without it, a
/// handler that answered `502` to everything would pass the outage tests.
#[tokio::test]
async fn the_test_button_reports_a_healthy_directory() {
    let harness = Harness::start(Behaviour::Healthy).await;

    let (status, body) = harness.press_test_button(harness.provider_id).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "a directory that binds must still test green, got body: {body}"
    );
}

/// The defect: nothing is listening, and the admin was told their request was
/// malformed.
#[tokio::test]
async fn an_unreachable_directory_makes_the_test_button_a_502() {
    let harness = Harness::start(Behaviour::Unreachable).await;

    let (status, body) = harness.press_test_button(harness.provider_id).await;

    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "a directory nobody could reach is not a bad request, got body: {body}"
    );
    assert!(
        !body.contains(SERVICE_BIND_DN) && !body.contains("service-secret"),
        "the operator's detail must stay in the log, not in the response: {body}"
    );
}

/// The directory answered, and refused the forge's own service account. Still
/// its answer, still not a defect of the request that asked for the check.
#[tokio::test]
async fn a_refused_service_bind_makes_the_test_button_a_502() {
    let harness = Harness::start(Behaviour::ServiceBindRefused).await;

    let (status, body) = harness.press_test_button(harness.provider_id).await;

    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "a directory that refused our service bind answered — the request did not, \
         got body: {body}"
    );
}

/// The first `400` that has to survive: the row is not an LDAP provider at all,
/// so there is nothing to dial and the ask itself was wrong.
#[tokio::test]
async fn the_test_button_still_refuses_a_non_ldap_provider_with_a_400() {
    let harness = Harness::start(Behaviour::Healthy).await;
    let oidc = harness
        .add_provider(SsoProviderInput {
            name: "Mock IdP",
            slug: "idp",
            provider_type: "oidc",
            client_id: Some("client-id"),
            discovery_url: Some("https://example.com/.well-known/openid-configuration"),
            enabled: true,
            ..Default::default()
        })
        .await;

    let (status, body) = harness.press_test_button(oidc).await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "testing a provider that has no directory to dial is the caller's to fix, \
         got body: {body}"
    );
}

/// The second `400`: an LDAP row whose stored configuration cannot be turned
/// into a bindable config. Nothing was dialled, so no directory failed — and the
/// admin has a form field to fill in, which the refusal names.
#[tokio::test]
async fn the_test_button_still_refuses_an_unconfigured_ldap_provider_with_a_400() {
    let harness = Harness::start(Behaviour::Healthy).await;
    let key = rg_core::auth::encryption::derive_key(TEST_ENCRYPTION_KEY);
    let bind_password_enc = rg_core::auth::encryption::encrypt("service-secret", &key).unwrap();
    let unconfigured = harness
        .add_provider(SsoProviderInput {
            name: "Half-filled Directory",
            slug: "half-filled",
            provider_type: "ldap",
            // No host: the config is refused before anything is dialled.
            ldap_bind_dn: Some(SERVICE_BIND_DN),
            ldap_bind_password_enc: Some(&bind_password_enc),
            ldap_base_dn: Some(BASE_DN),
            enabled: true,
            ..Default::default()
        })
        .await;

    let (status, body) = harness.press_test_button(unconfigured).await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an incomplete provider row is the admin's form to fix, got body: {body}"
    );
    assert!(
        body.contains("LDAP host is missing"),
        "the refusal has to name the field that is missing, got: {body}"
    );
}

/// The `401` that has to survive the change: the directory answered, and its
/// answer is `49 invalidCredentials`. That is a verdict about this password —
/// it rejects, and it counts.
#[tokio::test]
async fn a_directory_that_rejects_the_password_is_still_a_401_and_still_counts() {
    let harness = Harness::start(Behaviour::WrongPassword).await;

    let (status, body) = harness.sign_in().await;

    assert_eq!(
        status,
        StatusCode::UNAUTHORIZED,
        "a directory that judged the password and said no is a rejected credential, \
         got body: {body}"
    );
    assert_eq!(
        harness.login_attempts().await,
        1,
        "a real rejection must still advance the brute-force counter, or the fix \
         has disarmed the lockout for every LDAP account"
    );
}
