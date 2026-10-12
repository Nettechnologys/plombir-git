//! The registry, driven by the `docker` binary rather than by our own idea of
//! what it sends.
//!
//! `oci_push_pull_tests` replays the wire format a client uses, which is what
//! caught `schemaVersion` vs `schema_version` — but a replay only ever contains
//! the requests we thought to write down. It cannot answer the questions that
//! are about the client's *own* logic: which challenge it reads, when it goes
//! for a token, what it does with a `401` that carries no `WWW-Authenticate`.
//! Those are the questions `card_87107a1e40bd` asks, and the phase's first
//! success criterion asks the same of every registry here.
//!
//! `#[ignore]`d, like `multi_backend_smoke`: it needs a working Docker daemon
//! and CI drives it by name. Docker treats `127.0.0.0/8` as an insecure
//! registry by default, so the test app's own loopback port is reachable over
//! plain HTTP with no daemon configuration at all.
//!
//! Every `docker` invocation gets `--config <tempdir>`, so a login here never
//! touches the credentials of whoever is running the suite.
//! A rootless daemon may have a different loopback namespace from the test
//! process. Running the compiled test in a temporary container with
//! `--network host` and the daemon socket mounted puts both ends in the
//! daemon's namespace without changing its registry configuration.

use std::path::Path;
use std::process::Command;

use crate::common::{register_full, spawn_test_app_with_db, spawn_test_app_with_state};

/// A `docker` run and what it said, with the streams kept apart: the daemon
/// puts progress on stderr and the useful failure text there too.
struct DockerRun {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

impl DockerRun {
    fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

fn docker(config: &Path, args: &[&str]) -> DockerRun {
    let output = Command::new("docker")
        .arg("--config")
        .arg(config)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run docker {args:?}: {error}"));
    DockerRun {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn docker_ok(config: &Path, args: &[&str]) -> DockerRun {
    let run = docker(config, args);
    assert!(
        run.status.success(),
        "docker {args:?} failed:\n{}",
        run.combined()
    );
    run
}

/// The smallest image a daemon will hold: an empty filesystem imported from an
/// empty tar. Pushing it exercises the whole protocol — config blob, layer
/// blob, manifest — without pulling anything from the internet.
fn tiny_image(config: &Path, tag: &str) {
    let empty_tar = tempfile::NamedTempFile::new().expect("create the empty layer tar");
    // 1024 zero bytes is a valid empty tar archive (two 512-byte end blocks).
    std::fs::write(empty_tar.path(), [0u8; 1024]).expect("write the empty layer tar");
    docker_ok(
        config,
        &[
            "import",
            empty_tar.path().to_str().expect("UTF-8 tar path"),
            tag,
        ],
    );
}

fn registry_host(base_url: &str) -> &str {
    base_url
        .strip_prefix("http://")
        .expect("the test app serves plain HTTP")
}

/// A private repository, its owner's credentials, and a docker config dir that
/// is logged in to it.
struct Registry {
    host: String,
    reference: String,
    token: String,
    logged_in: tempfile::TempDir,
}

async fn private_repo(base_url: &str, user: &str) -> Registry {
    let (token, _id) = register_full(base_url, user, &format!("{user}@example.invalid")).await;
    let created = reqwest::Client::new()
        .post(format!("{base_url}/api/v1/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "images", "is_private": true}))
        .send()
        .await
        .expect("create the private repository");
    assert_eq!(created.status(), 201, "create private repo");

    let host = registry_host(base_url).to_string();
    let reference = format!("{host}/{user}/images:v1");

    let logged_in = tempfile::tempdir().expect("docker config dir");
    docker_ok(
        logged_in.path(),
        &[
            "login", &host, "-u", user, // The password `register_full` registers with.
            "-p", "Qz7$wRtm",
        ],
    );

    Registry {
        host,
        reference,
        token,
        logged_in,
    }
}

/// Drive the complete lifecycle with the stock Docker client. The unit wire
/// replay cannot prove Docker accepts the challenge, digest and 202 response.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a working Docker daemon; run with --ignored"]
async fn docker_image_inventory_delete_and_gc() {
    let (base_url, db, state) = spawn_test_app_with_state().await;
    let registry = private_repo(&base_url, "ocilivegc").await;
    let config = registry.logged_in.path();
    tiny_image(config, &registry.reference);
    docker_ok(config, &["push", &registry.reference]);

    let client = reqwest::Client::new();
    let listed = client
        .get(format!(
            "{base_url}/api/v1/repos/ocilivegc/images/packages/oci/images"
        ))
        .bearer_auth(&registry.token)
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), 200);
    let body: serde_json::Value = listed.json().await.unwrap();
    let images = body["images"].as_array().expect("OCI images array");
    assert_eq!(images.len(), 1, "{body}");
    assert_eq!(images[0]["namespace"], "ocilivegc/images", "{body}");
    assert_eq!(images[0]["tag"], "v1", "{body}");
    assert!(
        images[0]["size_bytes"]
            .as_u64()
            .is_some_and(|size| size > 0),
        "{body}"
    );
    assert!(
        images[0]["pushed_at"]
            .as_str()
            .is_some_and(|date| chrono::DateTime::parse_from_rfc3339(date).is_ok()),
        "{body}"
    );
    let digest = images[0]["digest"].as_str().expect("digest");
    let removed = client
        .delete(format!(
            "{base_url}/api/v1/repos/ocilivegc/images/packages/oci/images/{digest}"
        ))
        .bearer_auth(&registry.token)
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), 202, "{}", removed.text().await.unwrap());

    forget(config, &registry.reference);
    let pull = docker(config, &["pull", &registry.reference]);
    assert!(
        !pull.status.success(),
        "deleted image remained pullable: {}",
        pull.combined()
    );

    let repo = rg_core::repo::service::find_repo_by_owner_name(&db, "ocilivegc", "images")
        .await
        .unwrap()
        .unwrap();
    let oci_repo = rg_db::ops::oci_ops::find_repo_by_id(&db, repo.id)
        .await
        .unwrap()
        .unwrap();
    let blobs = rg_db::ops::oci_ops::list_mature_blobs(
        &db,
        oci_repo.id,
        chrono::Utc::now() + chrono::Duration::hours(1),
    )
    .await
    .unwrap();
    assert!(
        !blobs.is_empty(),
        "docker push must have stored layer bytes"
    );
    let digests: Vec<_> = blobs.iter().map(|blob| blob.digest.clone()).collect();
    for digest in &digests {
        assert!(state
            .oci_storage
            .blob_exists("ocilivegc", "images", digest)
            .await
            .unwrap());
    }
    for blob in blobs {
        let mut aged: rg_db::entities::oci_blob::ActiveModel = blob.into();
        aged.created_at = sea_orm::Set(chrono::Utc::now() - chrono::Duration::hours(25));
        sea_orm::ActiveModelTrait::update(aged, &db).await.unwrap();
    }
    let sweep = rg_http::api::ci_retention::cleanup_expired_storage(&state, Some(repo.id))
        .await
        .unwrap();
    assert!(sweep.oci_blobs_deleted > 0, "{sweep:?}");
    assert_eq!(sweep.failures, 0, "{sweep:?}");
    for digest in &digests {
        assert!(!state
            .oci_storage
            .blob_exists("ocilivegc", "images", digest)
            .await
            .unwrap());
    }
}

/// Best-effort teardown: a test that leaves images behind poisons the next run
/// of itself, and `docker rmi` on an already-absent tag is not a failure.
fn forget(config: &Path, reference: &str) {
    let _ = docker(config, &["image", "rm", "-f", reference]);
}

/// The phase's first success criterion for this registry: a real client pushes
/// and pulls, not a replay of what we believe it sends.
// Multi-threaded on purpose: `docker` is driven with a blocking
// `Command::output()`, and the test app this suite spawns is a task on the same
// runtime. On the default current-thread flavour the first blocking wait starves
// the server, and the daemon's request to the registry times out instead of
// being answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a working Docker daemon; run with --ignored"]
async fn docker_pushes_and_pulls_a_private_repository() {
    let (base_url, _db) = spawn_test_app_with_db().await;
    let registry = private_repo(&base_url, "ociliveowner").await;
    let config = registry.logged_in.path();

    tiny_image(config, &registry.reference);
    docker_ok(config, &["push", &registry.reference]);

    // Drop the local copy, so the pull below has to come from the registry
    // rather than from the daemon's own store.
    forget(config, &registry.reference);
    docker_ok(config, &["pull", &registry.reference]);

    forget(config, &registry.reference);
}

/// card_87107a1e40bd: what a client actually does with the `401` that
/// `oci_unauthorized` returns.
///
/// Every OCI `401` except the one from `GET /v2/` used to go out with no
/// `WWW-Authenticate`, so a client that reads the challenge off the failing
/// request — rather than caching the one from the version check — had nowhere
/// to go for a token. The two halves are pinned separately because they fail
/// differently: an anonymous caller must be *told where to authenticate*, and a
/// caller who authenticated and still may not read must be told *no* rather
/// than sent around the loop again.
// Multi-threaded on purpose: `docker` is driven with a blocking
// `Command::output()`, and the test app this suite spawns is a task on the same
// runtime. On the default current-thread flavour the first blocking wait starves
// the server, and the daemon's request to the registry times out instead of
// being answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a working Docker daemon; run with --ignored"]
async fn an_anonymous_pull_of_a_private_repository_is_refused_with_a_challenge() {
    let (base_url, _db) = spawn_test_app_with_db().await;
    let registry = private_repo(&base_url, "ocichallengeowner").await;

    tiny_image(registry.logged_in.path(), &registry.reference);
    docker_ok(registry.logged_in.path(), &["push", &registry.reference]);
    forget(registry.logged_in.path(), &registry.reference);

    // The challenge itself, before the client's interpretation of it: the
    // manifest endpoint of a private repository, with no credentials at all.
    let anonymous = reqwest::Client::new()
        .get(format!(
            "http://{}/v2/ocichallengeowner/images/manifests/v1",
            registry.host
        ))
        .send()
        .await
        .expect("request the manifest anonymously");
    assert_eq!(anonymous.status(), 401);
    let challenge = anonymous
        .headers()
        .get(reqwest::header::WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        challenge.starts_with("Bearer realm="),
        "a 401 must say where to authenticate — RFC 7235 requires the header and a client that \
         does not cache the challenge from GET /v2/ has nowhere else to read it from. Got: {challenge:?}"
    );
    assert!(
        challenge.contains(r#"scope="repository:ocichallengeowner/images:pull""#),
        "the challenge must name the scope of the operation that was refused, or the token the \
         client comes back with grants something else. Got: {challenge:?}"
    );

    // And the client's own verdict: a pull with no credentials fails, and says
    // it is an authorization problem rather than a missing image.
    let anonymous_config = tempfile::tempdir().expect("docker config dir");
    let refused = docker(anonymous_config.path(), &["pull", &registry.reference]);
    assert!(
        !refused.status.success(),
        "an anonymous pull of a private repository must not succeed:\n{}",
        refused.combined()
    );
    let said = refused.combined().to_lowercase();
    assert!(
        said.contains("unauthorized")
            || said.contains("authentication required")
            || said.contains("denied"),
        "the refusal must read as an authorization problem, not as a missing image:\n{}",
        refused.combined()
    );
}
