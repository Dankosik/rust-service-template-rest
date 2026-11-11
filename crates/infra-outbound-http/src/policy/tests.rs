use std::time::Duration;

use http::header;

use crate::{
    BuildError, Client, Error, Limits, MAX_RESPONSE_HEADER_COUNT, policy, test_support::*,
};

#[test]
fn client_binds_one_https_origin() {
    for origin in [
        "https://provider.example",
        "https://provider.example:8443",
        "https://provider.example/v1/token?ignored=1",
        "https://8.8.8.8/",
        "https://127.0.0.1/",
        "https://[::ffff:127.0.0.1]/",
        "https://169.254.169.254/",
        "https://[fd00:ec2::254]/",
    ] {
        assert!(
            Client::try_new(&url(origin), limits()).is_ok(),
            "{origin} must be admitted"
        );
    }
    for origin in [
        "http://provider.example/",
        "https://user:secret@provider.example/",
        "https://user@provider.example/",
        "data:text/plain,provider",
    ] {
        assert!(
            matches!(
                Client::try_new(&url(origin), limits()),
                Err(BuildError::InvalidOrigin)
            ),
            "{origin} must be refused"
        );
    }
}

#[test]
fn requests_must_name_the_configured_origin() {
    let origin = policy::admit_origin(&url("https://provider.fixture.test")).expect("origin");
    for admitted in [
        "https://provider.fixture.test/items",
        "https://provider.fixture.test:443/items?q=1",
        "https://PROVIDER.fixture.test/items",
    ] {
        assert!(
            policy::admit_request(&origin, request_to(admitted)).is_ok(),
            "{admitted} must be admitted"
        );
    }
    for refused in [
        "https://other.fixture.test/items",
        "https://provider.fixture.test:8443/items",
        "http://provider.fixture.test/items",
        "https://user@provider.fixture.test/items",
        "/items",
        "*",
    ] {
        assert!(
            matches!(
                policy::admit_request(&origin, request_to(refused)),
                Err(Error::InvalidTarget)
            ),
            "{refused} must be refused"
        );
    }
    let mut with_host = request();
    with_host.headers_mut().insert(
        header::HOST,
        "other.fixture.test".parse().expect("host header"),
    );
    assert!(matches!(
        policy::admit_request(&origin, with_host),
        Err(Error::InvalidTarget)
    ));
}

#[test]
fn the_host_header_names_the_port_only_when_it_is_not_the_default() {
    for (origin, expected) in [
        ("https://provider.fixture.test", "provider.fixture.test"),
        ("https://provider.fixture.test:443", "provider.fixture.test"),
        (
            "https://provider.fixture.test:8443",
            "provider.fixture.test:8443",
        ),
        ("https://[::1]:8443", "[::1]:8443"),
    ] {
        let target = policy::admit_origin(&url(origin)).expect("origin");
        let admitted = policy::admit_request(&target, request_to(&format!("{origin}/items")))
            .expect("admitted request");
        assert_eq!(admitted.headers()[header::HOST], expected, "{origin}");
    }
}

#[test]
fn limits_must_be_positive_and_bounded() {
    let mutations: [fn(&mut Limits); 4] = [
        |limits: &mut Limits| limits.operation_timeout = Duration::ZERO,
        |limits: &mut Limits| limits.response_header_count = 0,
        |limits: &mut Limits| limits.response_header_count = MAX_RESPONSE_HEADER_COUNT + 1,
        |limits: &mut Limits| limits.response_body_bytes = 0,
    ];
    for mutate in mutations {
        let mut invalid = limits();
        mutate(&mut invalid);
        assert!(matches!(
            policy::validate_limits(&invalid),
            Err(BuildError::InvalidLimits)
        ));
    }
}
