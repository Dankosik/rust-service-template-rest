//! HTTP utility recipes use test-local routers and a loopback mock, not service endpoints.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use axum::http::{HeaderMap, HeaderValue, Uri};
use axum::{Json, Router, routing::get};
use axum_extra::TypedHeader;
use axum_extra::extract::Query;
use axum_extra::headers::{ContentLength, HeaderMapExt, UserAgent};
use axum_test::TestServer;
use serde::Deserialize;
use serde_json::json;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Filters {
    // Option<Vec<_>> does not have the desired single-value query semantics.
    #[serde(default)]
    tag: Vec<String>,
}

#[test]
fn query_lists_handle_zero_one_and_multiple_occurrences() {
    for (query, expected) in [
        ("/tags", vec![]),
        ("/tags?tag=rust", vec!["rust"]),
        ("/tags?tag=rust&tag=backend", vec!["rust", "backend"]),
    ] {
        let uri: Uri = query.parse().unwrap();
        let parsed = Query::<Filters>::try_from_uri(&uri).unwrap();
        assert_eq!(parsed.tag, expected);
    }
    let uri: Uri = "/tags?unknown=value".parse().unwrap();
    assert!(Query::<Filters>::try_from_uri(&uri).is_err());
}

// Tests extractor composition only. Real routes must map their rejection
// paths to Problem with the request ID, as the HTTP architecture requires.
async fn inspect_query(
    Query(filters): Query<Filters>,
    TypedHeader(agent): TypedHeader<UserAgent>,
) -> Json<serde_json::Value> {
    Json(json!({"tags": filters.tag, "agent": agent.as_str()}))
}

#[tokio::test]
#[allow(
    clippy::disallowed_methods,
    reason = "this test-local router exercises library extractors rather than application operations"
)]
async fn typed_header_and_query_extractors_compose_on_axum() {
    let server = TestServer::new(Router::new().route("/tags", get(inspect_query)));
    let response = server
        .get("/tags?tag=rust&tag=backend")
        .add_header("user-agent", "utility-recipes")
        .await;
    response.assert_status_ok();
    response.assert_json(&json!({"tags": ["rust", "backend"], "agent": "utility-recipes"}));
}

#[test]
fn typed_headers_remove_manual_string_parsing() {
    let mut headers = HeaderMap::new();
    headers.typed_insert(ContentLength(512));
    assert_eq!(headers.typed_get::<ContentLength>().unwrap().0, 512);
    headers.insert("content-length", HeaderValue::from_static("not-a-number"));
    assert!(headers.typed_try_get::<ContentLength>().is_err());
}

#[test]
fn url_query_encoding_does_not_use_string_concatenation() {
    let mut target = url::Url::parse("https://api.example.test/search").unwrap();
    target
        .query_pairs_mut()
        .append_pair("q", "Rust & HTTP")
        .append_pair("page", "2");
    assert_eq!(
        target.query_pairs().collect::<Vec<_>>(),
        [
            ("q".into(), "Rust & HTTP".into()),
            ("page".into(), "2".into())
        ]
    );
    assert_eq!(target.host_str(), Some("api.example.test"));
    // Parsing/joining a URL is not an origin allowlist.
    let replaced = target.join("https://other.example.test/path").unwrap();
    assert_ne!(replaced.origin(), target.origin());
}

#[tokio::test]
async fn a_reusable_http_client_has_explicit_timeouts_and_no_hidden_retry_stack() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/items/1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 1})))
        .expect(1)
        .mount(&mock)
        .await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(1))
        .timeout(Duration::from_secs(2))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .unwrap();
    let target = url::Url::parse(&mock.uri())
        .unwrap()
        .join("items/1")
        .unwrap();
    let body: serde_json::Value = client
        .get(target)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body, json!({"id": 1}));
    // The mock verifies the number of requests. Production clients additionally
    // select the project's TLS provider/roots and their provider-specific policy.
}
