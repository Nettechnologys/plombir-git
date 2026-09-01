//! Every path `rg-runner` builds must be a route this server mounts
//! (card_66eead51bb23).
//!
//! `crates/rg-runner/src/api.rs` spells twelve `/api/v1/runners/...` URLs out
//! as format strings, in a crate that cannot see the route table. Nothing compared
//! them with [`rg_http::routes`] — the agreement was held by eye, and the server
//! tests that touch these endpoints (`runner_auth_tests`, `admin_runner_tests`)
//! type their own literals, so they are a *third* copy of the list rather than a
//! check on the second.
//!
//! The stakes are higher than the MCP sibling this is modelled on
//! (`mcp_route_coverage_tests`, card_3d05153aaca1): a broken tool path spoils
//! one answer to an agent, while a broken `jobs/poll` or `jobs/{id}/finish`
//! means runners quietly stop taking or completing work while the server looks
//! perfectly healthy.
//!
//! ## What is asserted, and why the runner is never registered
//!
//! Each request is held to the route table the very same build produced —
//! `rg_http::route_table::RouteFact`, method and full path — so a call is only
//! accepted when a route is mounted for *that method* at *that path*. The
//! status is checked too, but only for the two answers that mean the router
//! refused: `405` and the `404` the fallback writes for a path no route claims
//! (`routes::protocol_subtrees_are_not_pages`).
//!
//! The table is what carries the method half, and it has to. Every runner route
//! is behind a credential, and that layer answers `401` before axum ever gets to
//! method dispatch — so sending `GET` to a `POST`-only runner route is answered
//! `401`, not `405`, and a status-only sweep is blind to method drift.
//! Verified by mutation: `.post` → `.get` on `heartbeat` leaves the status
//! assertions green and is caught by the table alone.
//!
//! Not registering a runner is what keeps this cheap *and* safe: no fixture, no
//! long-poll to wait out (`jobs/poll` would otherwise hold for its 30s
//! `timeout`), and no call can leave a mark on the database.
//!
//! ## Why the answer is read off the server
//!
//! Five of the calls are fire-and-forget by design — `send_heartbeat`,
//! `start_job`, `upload_log`, `finish_job` and `deregister_runner` return `()`
//! and only log — and `restore_cache` turns `404` into `Ok(false)`, "there is no
//! cache for this key", which is precisely the answer an unmounted path would
//! produce. Reading each function's own error prose would therefore be blind on
//! six of them. The recording layer below sees every request whatever the client
//! makes of it.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::common::source_scan::{rust_code_only, workspace_crates};
use crate::common::{build_test_app_state, setup_test_db, wait_for_listener};

/// The prefix every URL in this sweep shares.
const RUNNER_API_PREFIX: &str = "/api/v1/runners";

/// One request as the server saw it: method, path with the query stripped, and
/// the status that went back.
#[derive(Clone, Debug)]
struct Seen {
    method: String,
    path: String,
    status: u16,
}

/// Requests the router answered, in order.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<Seen>>>);

impl Recorder {
    fn take(&self) -> Vec<Seen> {
        std::mem::take(&mut *self.0.lock().expect("the recorder mutex is not poisoned"))
    }
}

async fn record(State(recorder): State<Recorder>, request: Request, next: Next) -> Response {
    let method = request.method().to_string();
    let path = request.uri().path().to_string();
    let response = next.run(request).await;
    recorder
        .0
        .lock()
        .expect("the recorder mutex is not poisoned")
        .push(Seen {
            method,
            path,
            status: response.status().as_u16(),
        });
    response
}

/// `/api/v1/runners/7/jobs/11/log` → `/api/v1/runners/{}/jobs/{}/log`.
///
/// Only whole numeric segments are replaced, so `register`, `poll`, `heartbeat`
/// and the rest keep their names and a renamed segment cannot pass as an id.
fn template_of(path: &str) -> String {
    path.split('/')
        .map(|segment| {
            if !segment.is_empty() && segment.bytes().all(|byte| byte.is_ascii_digit()) {
                "{}"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// `/api/v1/runners/{id}/jobs/{job_id}/log` → `/api/v1/runners/{}/jobs/{}/log`.
///
/// The route table's own spelling of a path parameter, reduced to the same hole
/// [`template_of`] leaves behind, so a mounted route and an observed request are
/// two spellings of one string.
fn hole_of(path: &str) -> String {
    let mut reduced = String::with_capacity(path.len());
    let mut rest = path;
    while let Some(open) = rest.find('{') {
        reduced.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('}') else {
            break;
        };
        reduced.push_str("{}");
        rest = &rest[open + close + 1..];
    }
    reduced.push_str(rest);
    reduced
}

/// One probe, and the route the server has to have seen it address.
struct RouteExpectation {
    probe: &'static str,
    method: &'static str,
    route: &'static str,
}

/// Every route this sweep drives, spelled the way `routes.rs` spells it.
///
/// `rg-runner` assembles all twelve URLs out of ids at runtime, so the probes
/// prove the path without any test source ever naming it. `docs/ui-inventory.json`
/// reads test sources, so all twelve read there as routes nothing touches — and
/// for `POST .../jobs/{job_id}/log` and `PUT .../jobs/{job_id}/artifacts/staging`
/// there was no other test to fall back on (card_d482cf7e098e).
///
/// The table is executable, not decorative: [`assert_addressed_the_credited_route`]
/// holds every recorded request to the row that claims it, so a probe pointed at
/// another route — or a route renamed on either side — fails here instead of
/// quietly changing what the artefact claims.
const ROUTE_EXPECTATIONS: [RouteExpectation; 12] = [
    RouteExpectation {
        probe: "register_runner",
        method: "POST",
        route: "/api/v1/runners/register",
    },
    RouteExpectation {
        probe: "poll_job",
        method: "GET",
        route: "/api/v1/runners/{id}/jobs/poll",
    },
    RouteExpectation {
        probe: "send_heartbeat",
        method: "POST",
        route: "/api/v1/runners/{id}/heartbeat",
    },
    RouteExpectation {
        probe: "start_job",
        method: "POST",
        route: "/api/v1/runners/{id}/jobs/{job_id}/start",
    },
    RouteExpectation {
        probe: "upload_log",
        method: "POST",
        route: "/api/v1/runners/{id}/jobs/{job_id}/log",
    },
    RouteExpectation {
        probe: "download_workspace",
        method: "GET",
        route: "/api/v1/runners/{id}/jobs/{job_id}/workspace",
    },
    RouteExpectation {
        probe: "restore_cache",
        method: "GET",
        route: "/api/v1/runners/{id}/jobs/{job_id}/cache",
    },
    RouteExpectation {
        probe: "save_cache",
        method: "PUT",
        route: "/api/v1/runners/{id}/jobs/{job_id}/cache",
    },
    RouteExpectation {
        probe: "stage_artifact",
        method: "PUT",
        route: "/api/v1/runners/{id}/jobs/{job_id}/artifacts/staging",
    },
    RouteExpectation {
        probe: "publish_artifact",
        method: "POST",
        route: "/api/v1/runners/{id}/jobs/{job_id}/artifacts",
    },
    RouteExpectation {
        probe: "deregister_runner",
        method: "POST",
        route: "/api/v1/runners/{id}/deregister",
    },
    RouteExpectation {
        probe: "finish_job",
        method: "POST",
        route: "/api/v1/runners/{id}/jobs/{job_id}/finish",
    },
];

/// `/api/v1/runners/{id}/jobs/{job_id}/log` → `/api/v1/runners/1/jobs/1/log`.
fn probe_path(route: &str, runner_id: i64, job_id: i64) -> String {
    route
        .replace("{id}", &runner_id.to_string())
        .replace("{job_id}", &job_id.to_string())
}

/// This call addressed exactly the route credited to it, and nothing else.
fn assert_addressed_the_credited_route(name: &str, seen: &[Seen], runner_id: i64, job_id: i64) {
    let expected: Vec<String> = ROUTE_EXPECTATIONS
        .iter()
        .filter(|row| row.probe == name)
        .map(|row| {
            format!(
                "{} {}",
                row.method,
                probe_path(row.route, runner_id, job_id)
            )
        })
        .collect();
    assert!(
        !expected.is_empty(),
        "{name} sends a request no row of `ROUTE_EXPECTATIONS` claims — add it, or the route it \
         drives is credited to nothing"
    );
    let actual: Vec<String> = seen
        .iter()
        .map(|request| format!("{} {}", request.method, request.path))
        .collect();
    assert_eq!(
        actual, expected,
        "{name} no longer addresses the route credited to its executable coverage"
    );
}

/// The URL templates `rg-runner` writes down, read out of its source.
///
/// The completeness half of this sweep: a further call added to `api.rs` with
/// no probe beside it would otherwise be swept in silence, which is the failure
/// mode the whole file exists to end.
///
/// [`rust_code_only`] blanks comments *and* literals byte-for-byte, so a match
/// in the original text that is blank in that view sits inside one of the two —
/// and which one is settled by the first byte of the blanked run: a string
/// literal opens with its quote (or with the `r` of a raw string), a comment
/// with `/`. That is the whole discrimination, and it costs no second lexer:
/// `route_gate_rank_guard` had to grow one and read a raw string wrong for it
/// (see `parse_route_facts`, commit 00c7dd4).
fn templates_declared_in_runner_source() -> BTreeSet<String> {
    let source_path = workspace_crates().join("rg-runner/src/api.rs");
    let source = std::fs::read_to_string(&source_path)
        .unwrap_or_else(|error| panic!("read {}: {error}", source_path.display()));
    let masked = rust_code_only(&source);
    let masked = masked.as_bytes();
    let bytes = source.as_bytes();

    let mut declared = BTreeSet::new();
    for (at, _) in source.match_indices(RUNNER_API_PREFIX) {
        if masked[at] != b' ' {
            continue;
        }
        let mut start = at;
        while start > 0 && masked[start - 1] == b' ' && bytes[start - 1] != b'\n' {
            start -= 1;
        }
        let mut end = at;
        while end < masked.len() && masked[end] == b' ' && bytes[end] != b'\n' {
            end += 1;
        }
        // A line comment's blanked run opens with the `/` of `//`; a literal's
        // opens with its quote or the `r` of a raw string.
        let Some(open) = source[start..end].find('"') else {
            continue;
        };
        if bytes[start] == b'/' {
            continue;
        }
        let Some(close) = source[start..end].rfind('"') else {
            continue;
        };
        let body = &source[start + open + 1..start + close];
        let Some(url) = body.find(RUNNER_API_PREFIX).map(|from| &body[from..]) else {
            continue;
        };
        let url = url.split('?').next().unwrap_or(url);
        // `{}`, `{server}` and `{runner_id}` are one and the same hole.
        declared.insert(hole_of(url));
    }
    assert!(
        !declared.is_empty(),
        "no `{RUNNER_API_PREFIX}` URL was found in {} — the scan, not the runner, is what broke",
        source_path.display()
    );
    declared
}

/// Drive one call and hand back what the server saw it ask for.
///
/// The client's own return value is deliberately dropped: five of the calls
/// cannot report a routing failure through it (see the module header).
async fn probe<F, T>(recorder: &Recorder, name: &str, call: F) -> Vec<Seen>
where
    F: std::future::Future<Output = T>,
{
    let before = recorder.take();
    assert!(
        before.is_empty(),
        "{name} started with {} unread request(s) recorded",
        before.len()
    );
    call.await;
    let seen = recorder.take();
    assert!(
        !seen.is_empty(),
        "{name} sent no request at all — it turned its own arguments away before reaching the server"
    );
    seen
}

/// Every request this call made was matched by a route mounted for its method.
fn assert_addressed_a_mounted_route(name: &str, seen: &[Seen], mounted: &BTreeSet<String>) {
    for request in seen {
        let Seen {
            method,
            path,
            status,
        } = request;
        assert_ne!(
            *status, 405,
            "{name} addressed `{method} {path}`, a path this server mounts under other methods only"
        );
        assert_ne!(
            *status, 404,
            "{name} addressed `{method} {path}`, which no route claims"
        );
        let addressed = format!("{method} {}", template_of(path));
        assert!(
            mounted.contains(&addressed),
            "{name} addressed `{addressed}`, which this build mounts no route for"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_runner_api_call_addresses_a_route_this_server_mounts() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create the test repo root");

    let recorder = Recorder::default();
    let state = build_test_app_state(db, repo_root);
    let (app, facts) = rg_http::create_router_for_test_with_routes(state);
    // `{id}` and `{job_id}` are holes of the same shape the observed paths are
    // reduced to, so the two sides are comparable without a second spelling of
    // either list.
    let mounted: BTreeSet<String> = facts
        .iter()
        .map(|fact| format!("{} {}", fact.method, hole_of(&fact.path)))
        .collect();
    assert!(
        mounted
            .iter()
            .any(|route| route.contains(RUNNER_API_PREFIX)),
        "the route table names no `{RUNNER_API_PREFIX}` route at all"
    );
    let app = app.layer(axum::middleware::from_fn_with_state(
        recorder.clone(),
        record,
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let dir_path = dir.path().to_path_buf();
    let workspace = dir.path().join("runner-workspace");
    std::fs::create_dir_all(&workspace).expect("create the probe workspace");
    std::fs::write(workspace.join("cached.txt"), b"probe").expect("seed the cache probe");
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    let base = format!("http://{addr}");

    // Dull values throughout: the runner is not registered and the token is not
    // a token, so every call is turned away by the gate. What the handler would
    // have made of the numbers is not this sweep's business.
    let client = reqwest::Client::new();
    let (runner_id, job_id, token) = (1_i64, 1_i64, "not-a-runner-token");
    let cache_key = "probe";
    let cache_paths = vec!["cached.txt".to_string()];
    // `stage_artifact` refuses an empty archive before it sends anything, and a
    // probe that never reaches the server proves nothing about the route.
    let artifact = dir_path.join("probe-artifact.tar");
    std::fs::write(&artifact, b"probe archive").expect("seed the artifact probe");

    let mut observed = BTreeSet::new();
    let mut probed = BTreeSet::new();
    let mut record_probe = |name: &str, seen: Vec<Seen>| {
        assert_addressed_a_mounted_route(name, &seen, &mounted);
        assert_addressed_the_credited_route(name, &seen, runner_id, job_id);
        probed.insert(name.to_string());
        for request in &seen {
            observed.insert(template_of(&request.path));
        }
    };

    record_probe(
        "register_runner",
        probe(&recorder, "register_runner", async {
            rg_runner::api::register_runner(&client, &base, "owner/repository", "probe", &[], token)
                .await
        })
        .await,
    );
    record_probe(
        "poll_job",
        probe(&recorder, "poll_job", async {
            rg_runner::api::poll_job(&client, &base, runner_id, token).await
        })
        .await,
    );
    record_probe(
        "send_heartbeat",
        probe(&recorder, "send_heartbeat", async {
            rg_runner::api::send_heartbeat(&client, &base, runner_id, token).await
        })
        .await,
    );
    record_probe(
        "start_job",
        probe(&recorder, "start_job", async {
            rg_runner::api::start_job(&client, &base, runner_id, job_id, token).await
        })
        .await,
    );
    record_probe(
        "upload_log",
        probe(&recorder, "upload_log", async {
            rg_runner::api::upload_log(&client, &base, runner_id, job_id, token, "probe").await
        })
        .await,
    );
    record_probe(
        "download_workspace",
        probe(&recorder, "download_workspace", async {
            rg_runner::api::download_workspace(&client, &base, runner_id, job_id, token).await
        })
        .await,
    );
    record_probe(
        "restore_cache",
        probe(&recorder, "restore_cache", async {
            rg_runner::api::restore_cache(
                &client, &base, runner_id, job_id, token, cache_key, &workspace,
            )
            .await
        })
        .await,
    );
    record_probe(
        "save_cache",
        probe(&recorder, "save_cache", async {
            rg_runner::api::save_cache(
                &client,
                &base,
                runner_id,
                job_id,
                token,
                cache_key,
                &cache_paths,
                &workspace,
            )
            .await
        })
        .await,
    );
    record_probe(
        "stage_artifact",
        probe(&recorder, "stage_artifact", async {
            rg_runner::api::stage_artifact(&client, &base, runner_id, job_id, token, &artifact)
                .await
        })
        .await,
    );
    record_probe(
        "publish_artifact",
        probe(&recorder, "publish_artifact", async {
            rg_runner::api::publish_artifact(
                &client,
                &base,
                runner_id,
                job_id,
                token,
                "probe.tar",
                artifact.to_string_lossy().as_ref(),
            )
            .await
        })
        .await,
    );
    record_probe(
        "deregister_runner",
        probe(&recorder, "deregister_runner", async {
            rg_runner::api::deregister_runner(&client, &base, runner_id, token).await
        })
        .await,
    );
    record_probe(
        "finish_job",
        probe(&recorder, "finish_job", async {
            rg_runner::api::finish_job(&client, &base, runner_id, job_id, token, "success", 0).await
        })
        .await,
    );

    assert_eq!(
        observed,
        templates_declared_in_runner_source(),
        "the probe table and the URLs `rg-runner/src/api.rs` builds have drifted apart"
    );

    // The other direction: a row that claims a route no probe drives would
    // credit that route with coverage nothing runs.
    let credited: BTreeSet<String> = ROUTE_EXPECTATIONS
        .iter()
        .map(|row| row.probe.to_string())
        .collect();
    assert_eq!(
        credited, probed,
        "`ROUTE_EXPECTATIONS` and the probes this sweep runs name different calls"
    );

    server.abort();
}
