//! The npm registry driven by the real `npm` binary.
//!
//! The HTTP integration tests pin the wire contract, but only the client can
//! prove which relative URLs `npm dist-tag` derives and whether `pkg@beta`
//! actually resolves through the returned packument.  This test is ignored in
//! the ordinary suite because npm is an external executable; the package and
//! cache directories are private temporary paths, so it neither reads nor
//! mutates the developer's npm configuration.

use std::path::Path;
use std::process::Command;

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

struct NpmRun {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

impl NpmRun {
    fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

fn npm(workdir: &Path, userconfig: &Path, cache: &Path, args: &[&str]) -> NpmRun {
    let output = Command::new("npm")
        .current_dir(workdir)
        .args(args)
        .arg("--userconfig")
        .arg(userconfig)
        .arg("--cache")
        .arg(cache)
        .output()
        .unwrap_or_else(|error| panic!("run npm {args:?}: {error}"));
    NpmRun {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

fn npm_ok(workdir: &Path, userconfig: &Path, cache: &Path, args: &[&str]) -> NpmRun {
    let run = npm(workdir, userconfig, cache, args);
    assert!(
        run.status.success(),
        "npm {args:?} failed:\n{}",
        run.combined()
    );
    run
}

fn write_package(directory: &Path, version: &str, marker: &str) {
    std::fs::write(
        directory.join("package.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "name": "matrix-live-dist-tag",
            "version": version,
            "main": "index.js"
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        directory.join("index.js"),
        format!("module.exports = {marker:?};\n"),
    )
    .unwrap();
}

// Multi-threaded because `Command::output()` blocks while the test server runs
// as another Tokio task in this process.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs the npm executable; run with --ignored"]
async fn npm_publish_beta_and_install_by_tag_round_trip() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "npmliveowner", "npmliveowner@example.com").await;
    create_repo(&base, &token, "packages").await;

    let registry = format!(
        "{}/api/v1/repos/npmliveowner/packages/packages/npm/",
        base.trim_end_matches('/')
    );
    let sandbox = tempfile::tempdir().unwrap();
    let package_dir = sandbox.path().join("package");
    let install_dir = sandbox.path().join("consumer");
    let cache = sandbox.path().join("cache");
    std::fs::create_dir_all(&package_dir).unwrap();
    std::fs::create_dir_all(&install_dir).unwrap();
    std::fs::create_dir_all(&cache).unwrap();

    let userconfig = sandbox.path().join("npmrc");
    let auth_scope = registry.strip_prefix("http:").unwrap();
    std::fs::write(
        &userconfig,
        format!("registry={registry}\n{auth_scope}:_authToken={token}\nalways-auth=true\n"),
    )
    .unwrap();

    write_package(&package_dir, "1.0.0", "stable bytes");
    npm_ok(
        &package_dir,
        &userconfig,
        &cache,
        &["publish", "--registry", &registry],
    );

    write_package(&package_dir, "2.0.0-beta.1", "beta bytes");
    npm_ok(
        &package_dir,
        &userconfig,
        &cache,
        &["publish", "--tag", "beta", "--registry", &registry],
    );

    let tags = npm_ok(
        &package_dir,
        &userconfig,
        &cache,
        &[
            "dist-tag",
            "ls",
            "matrix-live-dist-tag",
            "--registry",
            &registry,
        ],
    );
    assert!(tags.stdout.contains("latest: 1.0.0"), "{}", tags.combined());
    assert!(
        tags.stdout.contains("beta: 2.0.0-beta.1"),
        "{}",
        tags.combined()
    );

    npm_ok(
        &install_dir,
        &userconfig,
        &cache,
        &[
            "install",
            "matrix-live-dist-tag@beta",
            "--registry",
            &registry,
            "--ignore-scripts",
            "--no-audit",
            "--no-fund",
        ],
    );
    let installed =
        std::fs::read_to_string(install_dir.join("node_modules/matrix-live-dist-tag/index.js"))
            .unwrap();
    assert_eq!(installed, "module.exports = \"beta bytes\";\n");

    // The standalone mutation commands use `/-/package/.../dist-tags`, not the
    // packument PUT URL. Driving both catches a route that only happens to make
    // `publish --tag` work.
    npm_ok(
        &package_dir,
        &userconfig,
        &cache,
        &[
            "dist-tag",
            "add",
            "matrix-live-dist-tag@1.0.0",
            "stable",
            "--registry",
            &registry,
        ],
    );
    npm_ok(
        &package_dir,
        &userconfig,
        &cache,
        &[
            "dist-tag",
            "rm",
            "matrix-live-dist-tag",
            "stable",
            "--registry",
            &registry,
        ],
    );
}

// `npm --provenance` can mint a bundle only in a supported OIDC CI provider.
// Outside one, the equivalent real-client path is `--provenance-file`: npm
// verifies the supplied Sigstore bundle and emits the same second `.sigstore`
// attachment; the test then probes the registry's advertised read endpoint.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs npm plus registry.npmjs.org; run with --ignored"]
async fn npm_publish_with_public_provenance_bundle_round_trips() {
    let upstream: serde_json::Value = reqwest::get("https://registry.npmjs.org/pino/9.13.1")
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let tarball_url = upstream["dist"]["tarball"].as_str().unwrap();
    let upstream_attestations_url = upstream["dist"]["attestations"]["url"].as_str().unwrap();
    let tarball = reqwest::get(tarball_url)
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let upstream_attestations: serde_json::Value = reqwest::get(upstream_attestations_url)
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let bundle = upstream_attestations["attestations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|attestation| attestation["predicateType"] == "https://slsa.dev/provenance/v1")
        .unwrap()["bundle"]
        .clone();

    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(
        &base,
        "npmprovenanceowner",
        "npmprovenanceowner@example.com",
    )
    .await;
    create_repo(&base, &token, "packages").await;
    let registry = format!(
        "{}/api/v1/repos/npmprovenanceowner/packages/packages/npm/",
        base.trim_end_matches('/')
    );
    let sandbox = tempfile::tempdir().unwrap();
    let cache = sandbox.path().join("cache");
    std::fs::create_dir_all(&cache).unwrap();
    let tarball_path = sandbox.path().join("pino-9.13.1.tgz");
    let bundle_path = sandbox.path().join("pino-9.13.1.sigstore");
    std::fs::write(&tarball_path, &tarball).unwrap();
    std::fs::write(&bundle_path, serde_json::to_vec(&bundle).unwrap()).unwrap();

    let userconfig = sandbox.path().join("npmrc");
    let auth_scope = registry.strip_prefix("http:").unwrap();
    std::fs::write(
        &userconfig,
        format!("registry={registry}\n{auth_scope}:_authToken={token}\nalways-auth=true\n"),
    )
    .unwrap();
    npm_ok(
        sandbox.path(),
        &userconfig,
        &cache,
        &[
            "publish",
            tarball_path.to_str().unwrap(),
            "--provenance-file",
            bundle_path.to_str().unwrap(),
            "--registry",
            &registry,
        ],
    );

    let local_packument: serde_json::Value = reqwest::get(format!("{registry}pino"))
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    let local_attestations_url = local_packument["versions"]["9.13.1"]["dist"]["attestations"]
        ["url"]
        .as_str()
        .unwrap();
    let local_attestations: serde_json::Value = reqwest::get(local_attestations_url)
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(local_attestations["attestations"][0]["bundle"], bundle);
}
