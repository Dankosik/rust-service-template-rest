//! Downstream adoption of the outbound client's narrow loopback mock API.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test fixture setup"
)]

use std::time::Duration;

use infra_outbound_http::{Bytes, Client, Error, Limits, Request, Url};
use tokio::time::Instant;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn limits() -> Limits {
    Limits {
        operation_timeout: Duration::from_secs(1),
        response_header_count: 8,
        response_body_bytes: 128,
    }
}

#[tokio::test]
async fn loopback_mock_constructor_uses_the_bounded_client_path() {
    let mock = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/provider/items"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"ok"))
        .expect(1)
        .mount(&mock)
        .await;

    let origin = Url::parse(&mock.uri()).unwrap();
    let client = Client::new_for_test_http(&origin, limits()).expect("loopback mock client");
    let request = Request::get(origin.join("/provider/items").unwrap().as_str())
        .body(Bytes::new())
        .unwrap();
    let response = client
        .execute(request, Instant::now() + Duration::from_secs(1))
        .await
        .expect("mock response through bounded client");
    assert_eq!(response.body().as_ref(), b"ok");
}

#[test]
fn production_and_mock_constructors_keep_their_separate_admission_boundaries() {
    assert!(matches!(
        Client::new(&Url::parse("http://127.0.0.1:8080").unwrap(), limits()),
        Err(Error::InvalidConfiguration)
    ));
    for base in [
        "http://localhost:8080",
        "http://10.0.0.1:8080",
        "https://127.0.0.1:8080",
        "http://user@127.0.0.1:8080",
    ] {
        assert!(matches!(
            Client::new_for_test_http(&Url::parse(base).unwrap(), limits()),
            Err(Error::InvalidConfiguration)
        ));
    }
}
