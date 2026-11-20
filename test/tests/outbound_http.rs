use std::time::Duration;

use infra_outbound_http::{
    BuildError, Bytes, Client, InvalidOrigin, Limits, OperationContext, Origin, Request,
};
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

    let client =
        Client::try_new_for_test_http(&mock.uri(), limits()).expect("loopback mock client");
    let request = Request::get(client.origin().uri("/provider/items").unwrap())
        .body(Bytes::new())
        .unwrap();
    let response = client
        .execute(
            &OperationContext::with_timeout(Duration::from_secs(1)),
            request,
        )
        .await
        .expect("mock response through bounded client");
    assert_eq!(response.body().as_ref(), b"ok");
}

#[test]
fn production_and_mock_constructors_keep_their_separate_admission_boundaries() {
    std::assert_matches!(
        Origin::parse_https("http://127.0.0.1:8080"),
        Err(InvalidOrigin)
    );
    for base in [
        "http://localhost:8080",
        "http://10.0.0.1:8080",
        "https://127.0.0.1:8080",
        "http://user@127.0.0.1:8080",
        "http://127.0.0.1:8080/path",
    ] {
        std::assert_matches!(
            Client::try_new_for_test_http(base, limits()),
            Err(BuildError::InvalidOrigin),
            "{base} must be refused"
        );
    }
    let http_client =
        Client::try_new_for_test_http("http://127.0.0.1:8080", limits()).expect("test http client");
    std::assert_matches!(
        Client::try_new(http_client.origin().clone(), limits()),
        Err(BuildError::InvalidOrigin)
    );
    let https_origin = Origin::parse_https("https://provider.example").unwrap();
    std::assert_matches!(
        Client::try_new_for_test_https(
            https_origin.clone(),
            limits(),
            b"not-der",
            "10.0.0.1:443".parse().unwrap(),
        ),
        Err(BuildError::InvalidOrigin)
    );
    std::assert_matches!(
        Client::try_new_for_test_https(
            https_origin,
            limits(),
            b"not-der",
            "127.0.0.1:443".parse().unwrap(),
        ),
        Err(BuildError::Tls { .. })
    );
}
