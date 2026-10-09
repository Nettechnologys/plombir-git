//! card_aa8cb50c61e8: every download handler built `Content-Disposition` with
//! `format!("attachment; filename=\"{}\"", name)`, and `HeaderValue` accepts
//! only visible ASCII. A file whose name is not ASCII — which every one of
//! these endpoints happily *accepts* on upload — therefore broke the download,
//! and broke it differently on each endpoint: the package download turned the
//! rejected header into a `500`, so a package that published successfully could
//! never be fetched again, while the release asset dropped the header behind an
//! `if let Ok(..)` and answered `200` with the browser left to guess a name
//! from the URL.
//!
//! These tests go through the real publish/upload path, so they also cover the
//! reading half: the name has to survive the client's `filename*=UTF-8''…`
//! into storage before the download can spell it back.

use crate::common::{create_initialised_repo, register_full, spawn_test_app};

/// A name that is legal everywhere and representable in no ASCII header.
const CYRILLIC: &str = "пакет-1.0.tgz";

/// RFC 5987 `filename*` for `CYRILLIC`, which is how any client sends a
/// non-ASCII name — a raw one cannot go in a header at all.
const CYRILLIC_ENCODED: &str = "%D0%BF%D0%B0%D0%BA%D0%B5%D1%82-1.0.tgz";

fn disposition(encoded: &str) -> String {
    format!("attachment; filename*=UTF-8''{encoded}")
}

/// The `Content-Disposition` of a response, or the fact that there is none —
/// which is the silent-failure half of this card and must be told apart from a
/// header carrying the wrong name.
fn content_disposition(response: &reqwest::Response) -> String {
    response
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .map(|value| {
            value
                .to_str()
                .expect("a header this server emits is visible ASCII")
                .to_string()
        })
        .unwrap_or_else(|| "<no Content-Disposition header>".to_string())
}

#[tokio::test]
async fn a_non_ascii_package_file_can_be_downloaded_after_it_is_published() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "cyrillic-pkg", "cyrillic-pkg@example.com").await;
    create_initialised_repo(&base, &token, "goods").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!(
            "{base}/api/v1/repos/cyrillic-pkg/goods/packages/generic/publish\
             ?name=sample&version=1.0.0"
        ))
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            disposition(CYRILLIC_ENCODED),
        )
        .body("bytes")
        .send()
        .await
        .expect("publish request");
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    assert!(
        status == 201 || status == 200,
        "the publish half already worked; it is the download that broke: {status} {body}"
    );

    let resp = client
        .get(format!(
            "{base}/api/v1/repos/cyrillic-pkg/goods/packages/generic/sample/1.0.0/{CYRILLIC}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("download request");
    let status = resp.status();
    let disposition = content_disposition(&resp);
    assert_eq!(
        status.as_u16(),
        200,
        "a published package must stay downloadable; got {status} \
         (this endpoint answered 500 for exactly this name)"
    );
    assert!(
        disposition.contains(&format!("filename*=UTF-8''{CYRILLIC_ENCODED}")),
        "the download must name the file the publisher chose: {disposition}"
    );
    assert!(
        disposition.contains("filename=\""),
        "the ASCII form has to stay for clients that never learned RFC 5987: {disposition}"
    );
    assert_eq!(resp.text().await.unwrap_or_default(), "bytes");
}

#[tokio::test]
async fn a_non_ascii_release_asset_is_served_with_the_name_it_was_uploaded_under() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "cyrillic-rel", "cyrillic-rel@example.com").await;
    create_initialised_repo(&base, &token, "shipments").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!(
            "{base}/api/v1/repos/cyrillic-rel/shipments/releases"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "tag_name": "v1.0.0", "title": "v1.0.0" }))
        .send()
        .await
        .expect("create release");
    assert_eq!(resp.status().as_u16(), 201);
    let release_id = resp.json::<serde_json::Value>().await.expect("json")["id"]
        .as_i64()
        .expect("release id");

    let resp = client
        .post(format!(
            "{base}/api/v1/repos/cyrillic-rel/shipments/releases/{release_id}/assets"
        ))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            disposition(CYRILLIC_ENCODED),
        )
        .body("release bytes")
        .send()
        .await
        .expect("upload asset");
    let status = resp.status();
    let asset = resp.json::<serde_json::Value>().await.expect("json");
    assert_eq!(status.as_u16(), 201, "upload failed: {asset}");
    assert_eq!(
        asset["filename"].as_str(),
        Some(CYRILLIC),
        "the upload must store the extended-form name, not a fallback: {asset}"
    );
    let asset_id = asset["id"].as_i64().expect("asset id");

    let resp = client
        .get(format!(
            "{base}/api/v1/repos/cyrillic-rel/shipments/releases/assets/{asset_id}/download"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("download asset");
    let status = resp.status();
    let disposition = content_disposition(&resp);
    assert_eq!(status.as_u16(), 200);
    assert!(
        disposition.contains(&format!("filename*=UTF-8''{CYRILLIC_ENCODED}")),
        "the header used to be dropped entirely for this name: {disposition}"
    );
}

/// The quoting half. `"` closes the quoted string and `;` starts the next
/// parameter, so a name carrying either used to be able to rewrite the header
/// around it — and on the package endpoint the sanitizing was simply absent.
#[tokio::test]
async fn a_release_asset_name_cannot_rewrite_the_header_around_it() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "quoting-rel", "quoting-rel@example.com").await;
    create_initialised_repo(&base, &token, "shipments").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!(
            "{base}/api/v1/repos/quoting-rel/shipments/releases"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "tag_name": "v1.0.0", "title": "v1.0.0" }))
        .send()
        .await
        .expect("create release");
    assert_eq!(resp.status().as_u16(), 201);
    let release_id = resp.json::<serde_json::Value>().await.expect("json")["id"]
        .as_i64()
        .expect("release id");

    // `a";filename="evil.exe` — a name that, pasted into a quoted string,
    // finishes it and appends a second `filename` parameter of the attacker's
    // choosing.
    let hostile = "a\";filename=\"evil.exe";
    let hostile_encoded = "a%22%3Bfilename%3D%22evil.exe";

    let resp = client
        .post(format!(
            "{base}/api/v1/repos/quoting-rel/shipments/releases/{release_id}/assets"
        ))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            disposition(hostile_encoded),
        )
        .body("release bytes")
        .send()
        .await
        .expect("upload asset");
    let status = resp.status();
    let asset = resp.json::<serde_json::Value>().await.expect("json");
    assert_eq!(status.as_u16(), 201, "upload failed: {asset}");
    assert_eq!(asset["filename"].as_str(), Some(hostile));
    let asset_id = asset["id"].as_i64().expect("asset id");

    let resp = client
        .get(format!(
            "{base}/api/v1/repos/quoting-rel/shipments/releases/assets/{asset_id}/download"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("download asset");
    assert_eq!(resp.status().as_u16(), 200);
    let disposition = content_disposition(&resp);
    // Spelled as an exact value: the quoting characters are neutralised in the
    // plain form, the extended form still carries the name byte for byte, and
    // nothing the uploader wrote became syntax.
    assert_eq!(
        disposition,
        format!(
            "attachment; filename=\"a__filename=_evil.exe\"; filename*=UTF-8''{hostile_encoded}"
        ),
        "a hostile asset name must not reshape the header"
    );
    assert_eq!(
        disposition.split(';').count(),
        3,
        "the name may not add a parameter: {disposition}"
    );
}

/// card_edd23bf9dbca, the reading half of the same quoting problem. A client
/// that sends only the plain `filename` — curl, a hand-rolled CI job, the npm
/// and NuGet clients — is the one that can put a `;` inside a quoted name, and
/// RFC 6266 §4.1 says that `;` is part of the name. The header used to be cut
/// on every `;` before the quotes were read, so the asset landed under
/// `release` and the `201` said nothing about the rest of the name.
#[tokio::test]
async fn a_plain_asset_name_may_contain_the_parameter_separator() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "semicolon-rel", "semicolon-rel@example.com").await;
    create_initialised_repo(&base, &token, "shipments").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!(
            "{base}/api/v1/repos/semicolon-rel/shipments/releases"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "tag_name": "v1.0.0", "title": "v1.0.0" }))
        .send()
        .await
        .expect("create release");
    assert_eq!(resp.status().as_u16(), 201);
    let release_id = resp.json::<serde_json::Value>().await.expect("json")["id"]
        .as_i64()
        .expect("release id");

    let resp = client
        .post(format!(
            "{base}/api/v1/repos/semicolon-rel/shipments/releases/{release_id}/assets"
        ))
        .bearer_auth(&token)
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        // No `filename*` at all: the plain form is the whole message, exactly
        // as a client that never learned RFC 5987 sends it.
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            r#"attachment; filename="release;notes.txt""#,
        )
        .body("release bytes")
        .send()
        .await
        .expect("upload asset");
    let status = resp.status();
    let asset = resp.json::<serde_json::Value>().await.expect("json");
    assert_eq!(status.as_u16(), 201, "upload failed: {asset}");
    assert_eq!(
        asset["filename"].as_str(),
        Some("release;notes.txt"),
        "the `;` is inside the quoted name, so it is part of it: {asset}"
    );
    let asset_id = asset["id"].as_i64().expect("asset id");

    let resp = client
        .get(format!(
            "{base}/api/v1/repos/semicolon-rel/shipments/releases/assets/{asset_id}/download"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("download asset");
    assert_eq!(resp.status().as_u16(), 200);
    let disposition = content_disposition(&resp);
    assert_eq!(
        disposition,
        "attachment; filename=\"release_notes.txt\"; filename*=UTF-8''release%3Bnotes.txt",
        "the download must spell back the whole name, with the `;` neutralised \
         only in the half that cannot carry it"
    );
    assert_eq!(resp.text().await.unwrap_or_default(), "release bytes");
}
