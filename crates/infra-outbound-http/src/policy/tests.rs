use std::time::Duration;

use http::{Uri, header};

use crate::{
    BuildError, Client, Error, InvalidOrigin, Limits, MAX_RESPONSE_HEADER_COUNT, Origin, policy,
    test_support::*,
};

#[test]
fn client_binds_one_https_origin() {
    for raw in [
        "https://provider.example",
        "https://provider.example/",
        "https://provider.example:8443",
        "https://8.8.8.8/",
        "https://127.0.0.1/",
        "https://[::ffff:127.0.0.1]/",
        "https://169.254.169.254/",
        "https://[fd00:ec2::254]/",
    ] {
        let origin = Origin::parse_https(raw).unwrap_or_else(|_| panic!("{raw} must be admitted"));
        let client = Client::try_new(origin.clone(), limits())
            .unwrap_or_else(|error| panic!("{raw} client must build: {error}"));
        assert_eq!(client.host(), origin.host());
        assert_eq!(client.port(), origin.port());
        assert!(client.matches_origin(origin.as_uri()));
        assert_eq!(
            client.uri("/items").expect("client uri"),
            origin.uri("/items").expect("origin uri")
        );
    }
    for raw in [
        "https://provider.example/v1/token?ignored=1",
        "https://provider.example/#fragment",
        "http://provider.example/",
        "https://user:secret@provider.example/",
        "https://user@provider.example/",
        "data:text/plain,provider",
        "https://provider.example/a\nb",
    ] {
        std::assert_matches!(
            Origin::parse_https(raw),
            Err(InvalidOrigin),
            "{raw} must be refused"
        );
    }

    let extracted = Origin::from_https_uri(&Uri::from_static(
        "https://PROVIDER.example:443/v1/token?q=1",
    ))
    .expect("origin from full HTTPS URI");
    let parsed: Origin = "https://provider.example".parse().expect("FromStr origin");
    assert_eq!(extracted, parsed);
    assert_eq!(extracted.host(), "provider.example");
    assert_eq!(extracted.port(), 443);
    assert_eq!(extracted.to_string(), "https://provider.example");
    assert_eq!(&Uri::from(&extracted), extracted.as_uri());
    assert_eq!(
        extracted.uri("/items?q=1").expect("origin uri"),
        Uri::from_static("https://provider.example/items?q=1")
    );
    for raw in [
        "http://provider.example/v1",
        "https://user@provider.example/v1",
        "/relative",
    ] {
        let uri: Uri = raw.parse().expect("valid URI");
        std::assert_matches!(
            Origin::from_https_uri(&uri),
            Err(InvalidOrigin),
            "{raw} must be refused"
        );
    }
    std::assert_matches!(BuildError::from(InvalidOrigin), BuildError::InvalidOrigin);
}

#[test]
fn requests_must_name_the_configured_origin() {
    let origin = policy::TargetOrigin::Https(
        Origin::parse_https("https://provider.fixture.test").expect("origin"),
    );
    for admitted in [
        "https://provider.fixture.test/items",
        "https://provider.fixture.test:/items",
        "https://provider.fixture.test:443/items?q=1",
        "https://provider.fixture.test:000443/items",
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
        "https://provider.fixture.test:65536/items",
        "https://provider.fixture.test:+443/items",
        "http://provider.fixture.test/items",
        "https://user@provider.fixture.test/items",
        "/items",
        "*",
    ] {
        std::assert_matches!(
            policy::admit_request(&origin, request_to(refused)),
            Err(Error::InvalidTarget),
            "{refused} must be refused"
        );
    }
    let mut with_host = request();
    with_host.headers_mut().insert(
        header::HOST,
        "other.fixture.test".parse().expect("host header"),
    );
    std::assert_matches!(
        policy::admit_request(&origin, with_host),
        Err(Error::InvalidTarget)
    );
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
        let target = policy::TargetOrigin::Https(Origin::parse_https(origin).expect("origin"));
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
        std::assert_matches!(invalid.validate(), Err(BuildError::InvalidLimits));
    }
}
