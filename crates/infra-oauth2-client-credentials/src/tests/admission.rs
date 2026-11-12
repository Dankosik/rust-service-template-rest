use super::*;

fn valid_options() -> Options {
    Options {
        token_url: "https://identity.example/token".to_owned(),
        client_id: "client".to_owned(),
        private_key: SecretString::from("placeholder"),
        key_id: "key-1".to_owned(),
        algorithm: Algorithm::Rs256,
        assertion_audience: "https://issuer.example".to_owned(),
        scopes: Vec::new(),
        audience: None,
        exchange_cache_capacity: 1024,
        provider_concurrency: 32,
    }
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one admission table retains the independent options contract"
)]
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
                scopes: vec!["bad scope".to_owned()],
                ..valid_options()
            },
            "scopes",
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
                audience: Some(String::new()),
                ..valid_options()
            },
            "audience",
        ),
        // Whitespace alone is empty, as the typed configuration section rules.
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
                audience: Some(" ".to_owned()),
                ..valid_options()
            },
            "audience",
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
        (
            Options {
                exchange_cache_capacity: 0,
                ..valid_options()
            },
            "exchange_cache_capacity",
        ),
        (
            Options {
                exchange_cache_capacity: 65_537,
                ..valid_options()
            },
            "exchange_cache_capacity",
        ),
    ] {
        let mut options = options;
        let error = Credentials::prepare(options.clone()).unwrap_err();
        assert_eq!(error.key, key);
        options.private_key = SecretString::from("different-secret");
        assert!(!format!("{options:?} {error:?}").contains("different-secret"));
    }
}

#[test]
fn a_token_url_with_whitespace_reports_the_character_rule() {
    let error = Credentials::prepare(Options {
        token_url: "https://identity.example/token\n".to_owned(),
        ..valid_options()
    })
    .unwrap_err();
    assert_eq!(error.key, "token_url");
    assert_eq!(error.reason, "must not contain whitespace or controls");
}

#[test]
fn an_unregistered_rejection_says_that_no_code_was_registered() {
    assert_eq!(
        AcquisitionError::Rejected(Rejection::Other).to_string(),
        "OAuth2 provider rejected the request: no registered error code"
    );
}

/// `Url` parses these to a URL without userinfo; the typed configuration
/// section refuses them, so direct construction does too.
#[test]
fn a_token_url_with_an_at_sign_in_its_raw_authority_is_refused() {
    for token_url in [
        "https://@identity.example/token",
        r"https://identity.example\@other.example/token",
    ] {
        let error = Credentials::prepare(Options {
            token_url: token_url.to_owned(),
            ..valid_options()
        })
        .unwrap_err();
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
            private_key: SecretString::from(private_key),
            algorithm: Algorithm::Rs256,
            ..valid_options()
        };
        let error = Credentials::prepare(options).unwrap_err();
        assert_eq!(error.key, "private_key");
    }
}
