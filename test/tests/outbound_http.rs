//! Downstream adoption of the outbound client's narrow loopback mock API.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test fixture setup"
)]

use std::time::Duration;

use infra_outbound_http::{Bytes, Client, Error, Limits, Operation, Request};
use tokio::time::Instant;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn limits() -> Limits {
    Limits {
        max_active: 1,
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

    let client = Client::new_for_test_http(&mock.uri(), limits()).expect("loopback mock client");
    let response = client
        .execute(
            Request::get("/provider/items").body(Bytes::new()).unwrap(),
            Operation {
                deadline: Instant::now() + Duration::from_secs(1),
                response_body_bytes: None,
            },
        )
        .await
        .expect("mock response through bounded client");
    assert_eq!(response.body().as_ref(), b"ok");
}

#[test]
fn production_and_mock_constructors_keep_their_separate_admission_boundaries() {
    assert!(matches!(
        Client::new("http://127.0.0.1:8080", limits()),
        Err(Error::InvalidConfiguration)
    ));
    for base in [
        "http://localhost:8080",
        "http://10.0.0.1:8080",
        "https://127.0.0.1:8080",
        "http://127.0.0.1:8080/provider",
    ] {
        assert!(matches!(
            Client::new_for_test_http(base, limits()),
            Err(Error::InvalidConfiguration)
        ));
    }
}
