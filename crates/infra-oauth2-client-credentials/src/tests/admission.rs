use std::time::UNIX_EPOCH;

use super::*;
use crate::{ConfigurationError, PreparationError, assertion::Signer};

fn valid_options() -> Options {
    Options {
        token_url: TokenUrl::parse("https://identity.example/token").unwrap(),
        client_id: "client".to_owned(),
        auth: PrivateKeyJwt {
            private_key: SecretString::from("placeholder"),
            key_id: "key-1".to_owned(),
            algorithm: Algorithm::Rs256,
            audience: IssuerUrl::parse("https://issuer.example").unwrap(),
        },
        provider_concurrency: nonzero(32),
    }
}

#[test]
fn direct_construction_repeats_sensitive_option_admission() {
    for (options, key) in [
        (
            Options {
                client_id: String::new(),
                ..valid_options()
            },
            "client_id",
        ),
        (
            Options {
                auth: PrivateKeyJwt {
                    key_id: String::new(),
                    ..valid_options().auth
                },
                ..valid_options()
            },
            "key_id",
        ),
        (
            Options {
                auth: PrivateKeyJwt {
                    private_key: SecretString::from(""),
                    ..valid_options().auth
                },
                ..valid_options()
            },
            "private_key",
        ),
        (
            Options {
                client_id: " ".to_owned(),
                ..valid_options()
            },
            "client_id",
        ),
        (
            Options {
                auth: PrivateKeyJwt {
                    key_id: "\t".to_owned(),
                    ..valid_options().auth
                },
                ..valid_options()
            },
            "key_id",
        ),
        (
            Options {
                auth: PrivateKeyJwt {
                    private_key: SecretString::from(" \n"),
                    ..valid_options().auth
                },
                ..valid_options()
            },
            "private_key",
        ),
        (
            Options {
                token_url: TokenUrl::parse_for_test_http("http://identity.example/token").unwrap(),
                ..valid_options()
            },
            "token_url",
        ),
        (
            Options {
                auth: PrivateKeyJwt {
                    audience: IssuerUrl::parse_for_test_http("http://issuer.example").unwrap(),
                    ..valid_options().auth
                },
                ..valid_options()
            },
            "assertion_audience",
        ),
    ] {
        let mut options = options;
        let error = Registration::prepare(options.clone()).unwrap_err();
        std::assert_matches!(
            error,
            PreparationError::Configuration(ConfigurationError { key: actual, .. })
            if actual == key
        );
        options.auth.private_key = SecretString::from("different-secret");
        assert!(!format!("{options:?} {error:?}").contains("different-secret"));
    }
}

#[test]
fn issuer_and_token_url_types_implement_from_str_and_enforce_url_rules() {
    let issuer: IssuerUrl = "https://issuer.example".parse().unwrap();
    assert_eq!(issuer.as_str(), "https://issuer.example");
    assert_eq!(issuer.as_ref(), "https://issuer.example");
    let token: TokenUrl = "https://identity.example/token?x=1".parse().unwrap();
    assert_eq!(token.as_str(), "https://identity.example/token?x=1");
    assert_eq!(token.as_ref(), "https://identity.example/token?x=1");

    for audience in [
        "",
        " ",
        "billing-api",
        "http://issuer.example",
        "https://issuer.example/?tenant=x",
        "https://issuer.example/#fragment",
        "https://@issuer.example",
        "https://issuer.example/\n",
    ] {
        let error = IssuerUrl::parse(audience).unwrap_err();
        assert_eq!(error.key, "issuer", "{audience:?}");
    }

    let error = Registration::prepare(Options {
        auth: PrivateKeyJwt {
            audience: IssuerUrl::parse("https://identity.example/token").unwrap(),
            ..valid_options().auth
        },
        ..valid_options()
    })
    .unwrap_err();
    std::assert_matches!(
        error,
        PreparationError::Configuration(ConfigurationError {
            key: "assertion_audience",
            ..
        })
    );
}

#[tokio::test]
async fn only_the_exchange_source_requires_a_valid_cache_capacity() {
    let fixture = Fixture::new().await;
    let registration = prepare_registration(fixture.options());
    let error = registration
        .exchange_tokens(
            ExchangeTokenTarget::default(),
            fixture.resource_origin(),
            nonzero(65_537),
        )
        .unwrap_err();
    assert_eq!(error.key, "exchange_cache_capacity");
    fixture.finish().await;
}

#[test]
fn on_behalf_of_rejects_an_empty_subject_at_construction() {
    assert_eq!(
        OnBehalfOf::try_new(SecretString::from("")).unwrap_err(),
        EmptySubject
    );
    let valid = OnBehalfOf::try_new(SecretString::from("subject-secret")).unwrap();
    assert!(!format!("{valid:?}").contains("subject-secret"));
}

#[test]
fn token_targets_require_valid_scope_tokens_and_a_nonempty_configured_audience() {
    assert_eq!(
        ServiceTokenTarget::try_new(["bad scope"]).unwrap_err().key,
        "scopes"
    );
    for audience in ["", " "] {
        assert_eq!(
            ExchangeTokenTarget::try_new(std::iter::empty::<&str>(), Some(audience.to_owned()))
                .unwrap_err()
                .key,
            "audience"
        );
    }
}

#[test]
fn a_token_url_with_whitespace_reports_the_character_rule() {
    let error = TokenUrl::parse("https://identity.example/token\n").unwrap_err();
    assert_eq!(
        error,
        ConfigurationError {
            key: "token_url",
            reason: "must not contain whitespace or controls",
        }
    );
}

#[test]
fn an_unregistered_rejection_says_that_no_code_was_registered() {
    assert_eq!(
        AcquisitionError::Rejected(Rejection::Other).to_string(),
        "OAuth2 provider rejected the request: no registered error code"
    );
}

#[test]
fn a_token_url_with_an_at_sign_in_its_raw_authority_is_refused() {
    for token_url in [
        "https://user:secret@identity.example/token",
        "https://@identity.example/token",
        r"https://identity.example\@other.example/token",
    ] {
        let error = TokenUrl::parse(token_url).unwrap_err();
        assert_eq!(error.key, "token_url", "{token_url}");
    }
}

#[test]
fn a_key_mismatched_with_its_algorithm_or_not_pem_is_refused_at_construction() {
    let ec_pem = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .unwrap()
        .serialize_pem();
    for private_key in [ec_pem, "not a pem key".to_owned()] {
        let options = Options {
            auth: PrivateKeyJwt {
                private_key: SecretString::from(private_key),
                algorithm: Algorithm::Rs256,
                ..valid_options().auth
            },
            ..valid_options()
        };
        let error = Registration::prepare(options).unwrap_err();
        std::assert_matches!(
            error,
            PreparationError::Configuration(ConfigurationError {
                key: "private_key",
                ..
            })
        );
    }
}

#[test]
fn a_pre_epoch_clock_is_reported_separately_from_the_private_key() {
    let options = Options {
        auth: PrivateKeyJwt {
            private_key: SecretString::from(
                KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
                    .unwrap()
                    .serialize_pem(),
            ),
            algorithm: Algorithm::Es256,
            ..valid_options().auth
        },
        ..valid_options()
    };
    let error = Signer::try_new(options, UNIX_EPOCH - Duration::from_secs(1))
        .err()
        .unwrap();
    assert_eq!(error.key, "system_clock");
}

#[test]
fn a_token_url_longer_than_a_request_target_may_be_is_refused_at_preparation() {
    let error = Registration::prepare(Options {
        token_url: TokenUrl::parse(&format!("https://identity.example/{}", "a".repeat(70_000)))
            .unwrap(),
        auth: PrivateKeyJwt {
            private_key: SecretString::from(
                KeyPair::generate_for(&PKCS_RSA_SHA256)
                    .unwrap()
                    .serialize_pem(),
            ),
            ..valid_options().auth
        },
        ..valid_options()
    })
    .unwrap_err();
    std::assert_matches!(
        error,
        PreparationError::Configuration(ConfigurationError {
            key: "token_url",
            ..
        })
    );
}

#[test]
fn an_empty_on_behalf_of_subject_is_refused_at_construction() {
    assert_eq!(
        OnBehalfOf::try_new(SecretString::from("")).unwrap_err(),
        EmptySubject
    );
    let subject = OnBehalfOf::try_new(SecretString::from("secret-subject-token")).unwrap();
    assert!(!format!("{subject:?}").contains("secret-subject-token"));
}
