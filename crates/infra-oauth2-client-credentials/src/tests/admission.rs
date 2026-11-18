use std::time::UNIX_EPOCH;

use super::*;
use crate::{ConfigurationError, PreparationError, assertion::Signer};

fn valid_options() -> Options {
    Options {
        token_url: "https://identity.example/token".to_owned(),
        client_id: "client".to_owned(),
        private_key: SecretString::from("placeholder"),
        key_id: "key-1".to_owned(),
        algorithm: Algorithm::Rs256,
        assertion_audience: "https://issuer.example".to_owned(),
        provider_concurrency: 32,
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
                key_id: String::new(),
                ..valid_options()
            },
            "key_id",
        ),
        (
            Options {
                assertion_audience: String::new(),
                ..valid_options()
            },
            "assertion_audience",
        ),
        (
            Options {
                private_key: SecretString::from(""),
                ..valid_options()
            },
            "private_key",
        ),
        (
            Options {
                token_url: "https://user:secret@identity.example/token".to_owned(),
                ..valid_options()
            },
            "token_url",
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
                key_id: "\t".to_owned(),
                ..valid_options()
            },
            "key_id",
        ),
        (
            Options {
                assertion_audience: " ".to_owned(),
                ..valid_options()
            },
            "assertion_audience",
        ),
        (
            Options {
                private_key: SecretString::from(" \n"),
                ..valid_options()
            },
            "private_key",
        ),
        (
            Options {
                provider_concurrency: 0,
                ..valid_options()
            },
            "provider_concurrency",
        ),
    ] {
        let mut options = options;
        let error = Registration::prepare(options.clone()).unwrap_err();
        std::assert_matches!(
            error,
            PreparationError::Configuration(ConfigurationError { key: actual, .. })
            if actual == key
        );
        options.private_key = SecretString::from("different-secret");
        assert!(!format!("{options:?} {error:?}").contains("different-secret"));
    }
}

#[test]
fn the_assertion_audience_must_be_an_issuer_distinct_from_the_token_endpoint() {
    for audience in [
        "billing-api",
        "http://issuer.example",
        "https://issuer.example/?tenant=x",
        "https://issuer.example/#fragment",
        "https://@issuer.example",
        "https://issuer.example/\n",
        "https://identity.example/token",
    ] {
        let error = Registration::prepare(Options {
            assertion_audience: audience.to_owned(),
            ..valid_options()
        })
        .unwrap_err();
        std::assert_matches!(
            error,
            PreparationError::Configuration(ConfigurationError {
                key: "assertion_audience",
                ..
            }),
            "{audience:?}"
        );
    }
}

#[tokio::test]
async fn only_the_exchange_source_requires_a_valid_cache_capacity() {
    let fixture = Fixture::new().await;
    let registration = prepare_registration(fixture.options());
    for capacity in [0, 65_537] {
        let error = registration
            .exchange_tokens(
                ExchangeTokenTarget::default(),
                fixture.resource_origin(),
                capacity,
            )
            .unwrap_err();
        assert_eq!(error.key, "exchange_cache_capacity");
    }
    fixture.finish().await;
}

#[test]
fn token_targets_require_valid_scope_tokens_and_a_nonempty_configured_audience() {
    assert_eq!(
        ServiceTokenTarget::try_new(&["bad scope".to_owned()])
            .unwrap_err()
            .key,
        "scopes"
    );
    for audience in ["", " "] {
        assert_eq!(
            ExchangeTokenTarget::try_new(&[], Some(audience.to_owned()))
                .unwrap_err()
                .key,
            "audience"
        );
    }
}

#[test]
fn a_token_url_with_whitespace_reports_the_character_rule() {
    let error = Registration::prepare(Options {
        token_url: "https://identity.example/token\n".to_owned(),
        ..valid_options()
    })
    .unwrap_err();
    std::assert_matches!(
        error,
        PreparationError::Configuration(ConfigurationError {
            key: "token_url",
            reason: "must not contain whitespace or controls",
        })
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
        "https://@identity.example/token",
        r"https://identity.example\@other.example/token",
    ] {
        let error = Registration::prepare(Options {
            token_url: token_url.to_owned(),
            ..valid_options()
        })
        .unwrap_err();
        std::assert_matches!(
            error,
            PreparationError::Configuration(ConfigurationError {
                key: "token_url",
                ..
            }),
            "{token_url}"
        );
    }
}

#[test]
fn a_key_mismatched_with_its_algorithm_or_not_pem_is_refused_at_construction() {
    let ec_pem = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
        .unwrap()
        .serialize_pem();
    for private_key in [ec_pem, "not a pem key".to_owned()] {
        let options = Options {
            private_key: SecretString::from(private_key),
            algorithm: Algorithm::Rs256,
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
        private_key: SecretString::from(
            KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)
                .unwrap()
                .serialize_pem(),
        ),
        algorithm: Algorithm::Es256,
        ..valid_options()
    };
    let error = Signer::try_new(&options, UNIX_EPOCH - Duration::from_secs(1))
        .err()
        .unwrap();
    assert_eq!(error.key, "system_clock");
}

#[test]
fn a_token_url_longer_than_a_request_target_may_be_is_refused_at_preparation() {
    let error = Registration::prepare(Options {
        token_url: format!("https://identity.example/{}", "a".repeat(70_000)),
        private_key: SecretString::from(
            KeyPair::generate_for(&PKCS_RSA_SHA256)
                .unwrap()
                .serialize_pem(),
        ),
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
