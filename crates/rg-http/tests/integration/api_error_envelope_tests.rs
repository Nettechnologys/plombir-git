//! Contract for refusals produced before an API handler runs.
//!
//! The API router owns one response envelope around the complete `RouteTable`.
//! These tests prove both halves of that claim: real `Json` extractor failures
//! cross the mounted layer, and every recorded API path answers an unsupported
//! method in the envelope even when a per-route middleware refuses it before
//! Axum's `MethodRouter` can produce its own `405`.

use std::collections::{BTreeMap, BTreeSet};

use reqwest::{Client, Method, Response, StatusCode};

use crate::common::{spawn_test_app, spawn_test_app_with_routes};

const API_ROUTE_FLOOR: usize = 200;

async fn api_error(response: Response, label: &str) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.starts_with("application/json")),
        Some(true),
        "{label}: refusal is not JSON"
    );
    let body: serde_json::Value = response
        .json()
        .await
        .unwrap_or_else(|error| panic!("{label}: refusal is not a JSON envelope: {error}"));
    assert!(
        body["error"]["code"]
            .as_str()
            .is_some_and(|code| !code.is_empty()),
        "{label}: body has no machine-readable code: {body}"
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .is_some_and(|message| !message.is_empty()),
        "{label}: body has no useful message: {body}"
    );
    (status, body)
}

async fn assert_api_error(response: Response, status: StatusCode, code: &str, label: &str) {
    let (actual_status, body) = api_error(response, label).await;
    assert_eq!(actual_status, status, "{label}: body: {body}");
    assert_eq!(body["error"]["code"], code, "{label}: body: {body}");
}

#[tokio::test]
async fn auth_user_refusals_use_the_api_error_envelope() {
    let base = spawn_test_app().await;
    let response = Client::new()
        .get(format!("{base}/api/v1/users/me"))
        .send()
        .await
        .expect("send an anonymous request to an AuthUser-guarded route");

    assert_api_error(
        response,
        StatusCode::UNAUTHORIZED,
        "UNAUTHORIZED",
        "AuthUser refusal",
    )
    .await;
}

#[tokio::test]
async fn json_extractor_refusals_cross_the_api_router_envelope() {
    let base = spawn_test_app().await;
    let client = Client::new();
    let endpoint = format!("{base}/api/v1/users/register");

    for (content_type, body, status, code, label) in [
        (
            "application/json",
            "{",
            StatusCode::BAD_REQUEST,
            "BAD_REQUEST",
            "malformed JSON",
        ),
        (
            "text/plain",
            "{}",
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "UNSUPPORTED_MEDIA_TYPE",
            "unsupported request content type",
        ),
    ] {
        let response = client
            .post(&endpoint)
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body)
            .send()
            .await
            .unwrap_or_else(|error| panic!("{label}: request failed: {error}"));
        assert_api_error(response, status, code, label).await;
    }

    // Unlike routes carrying a per-route credential wrapper, this public path
    // reaches MethodRouter's own fallback directly. It pins the empty 405 that
    // motivated the router half of the fix.
    let response = client.get(&endpoint).send().await.unwrap();
    assert_api_error(
        response,
        StatusCode::METHOD_NOT_ALLOWED,
        "METHOD_NOT_ALLOWED",
        "wrong method",
    )
    .await;
}

/// Replace Axum `{name}` and `{*catch_all}` parameters with one valid segment.
/// A method refusal happens before a handler reads those values, so the values
/// need only make the route pattern match; no database fixture is involved.
fn concrete_path(pattern: &str) -> String {
    let mut path = String::new();
    let mut rest = pattern;
    while let Some(open) = rest.find('{') {
        path.push_str(&rest[..open]);
        let close = rest[open..]
            .find('}')
            .map(|offset| open + offset)
            .expect("route fact contains an unclosed placeholder");
        path.push_str("envelope-probe");
        rest = &rest[close + 1..];
    }
    path.push_str(rest);
    path
}

#[tokio::test]
async fn every_recorded_api_path_wraps_an_unsupported_method_refusal() {
    let (base, facts) = spawn_test_app_with_routes().await;
    let mut paths: BTreeMap<String, BTreeSet<&'static str>> = BTreeMap::new();
    for fact in facts
        .into_iter()
        .filter(|fact| fact.path.starts_with("/api/v1/"))
    {
        paths.entry(fact.path).or_default().insert(fact.method);
    }
    assert!(
        paths.len() >= API_ROUTE_FLOOR,
        "only {} distinct API paths reached the envelope sweep; expected at least {API_ROUTE_FLOOR}",
        paths.len()
    );

    let client = Client::new();
    for (pattern, registered_methods) in paths {
        let unsupported_method = ["DELETE", "PATCH", "PUT", "POST", "GET", "OPTIONS", "TRACE"]
            .into_iter()
            .find(|method| !registered_methods.contains(method))
            .map(|method| Method::from_bytes(method.as_bytes()).expect("known HTTP method"))
            .unwrap_or_else(|| panic!("{pattern} accepts every available probe method"));
        let path = concrete_path(&pattern);
        let response = client
            .request(unsupported_method.clone(), format!("{base}{path}"))
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("{unsupported_method} {pattern}: request failed: {error}")
            });
        let label = format!("{unsupported_method} {pattern}");
        let (status, body) = api_error(response, &label).await;
        assert!(
            status.is_client_error(),
            "{label}: expected an early client refusal, got {status}: {body}"
        );
    }
}
