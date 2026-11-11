use super::*;
use crate::load::{Error, load_from};
use crate::test_support::{BUILD, env, write};
use crate::{Config, LoadOptions};

#[test]
fn whitespace_only_private_key_is_refused() {
    let config: OAuthConfig = toml::from_str(
        "token_url = \"https://identity.example/token\"\n\
         client_id = \"billing-service\"\n\
         private_key = \"   \"\n\
         key_id = \"key-1\"\n\
         algorithm = \"ES256\"\n\
         assertion_audience = \"https://identity.example\"\n",
    )
    .unwrap();
    let err = config.validate("integrations.billing.oauth").unwrap_err();
    assert_eq!(err.key, "integrations.billing.oauth.private_key");
    assert_eq!(
        err.message,
        "is required when integrations.billing.oauth is set; set APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY"
    );
}

#[test]
fn oauth_environment_builds_a_named_tuple_and_redacts_its_values() {
    use secrecy::ExposeSecret;

    let cfg = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "RS256"),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                "https://identity.example/oauth2/token?tenant=blue",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                "billing-service",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                "test-private-key",
            ),
            ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                "https://identity.example",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__SCOPES",
                "billing.read billing.write",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__AUDIENCE",
                "https://billing-api.example",
            ),
        ]),
    )
    .unwrap();

    let oauth = cfg
        .integrations
        .get("billing")
        .and_then(|integration| integration.oauth.as_ref())
        .expect("named OAuth tuple");
    assert_eq!(
        oauth.token_url,
        "https://identity.example/oauth2/token?tenant=blue"
    );
    assert_eq!(oauth.client_id, "billing-service");
    assert_eq!(
        oauth.private_key.as_ref().map(ExposeSecret::expose_secret),
        Some("test-private-key")
    );
    assert_eq!(oauth.key_id, "key-1");
    assert_eq!(oauth.algorithm, crate::OAuthAlgorithm::Rs256);
    assert_eq!(oauth.assertion_audience, "https://identity.example");
    assert_eq!(oauth.scopes.as_slice(), ["billing.read", "billing.write"]);
    assert_eq!(
        oauth.audience.as_deref(),
        Some("https://billing-api.example")
    );

    let debug = format!("{cfg:?}");
    for value in [
        "test-private-key",
        "billing.read",
        "https://billing-api.example",
        "identity.example/oauth2/token",
    ] {
        assert!(
            !debug.contains(value),
            "configuration debug output exposed {value}"
        );
    }
}

#[test]
fn oauth_without_an_algorithm_fails_naming_the_key() {
    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                "https://identity.example/token",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                "billing-service",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                "test-private-key",
            ),
            ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                "https://identity.example",
            ),
        ]),
    )
    .unwrap_err();
    assert!(matches!(&err, Error::Deserialize(_)), "{err}");
    assert!(
        err.to_string()
            .contains("missing configuration field \"integrations.billing.oauth.algorithm\""),
        "{err}"
    );
}

#[test]
fn oauth_exchange_cache_capacity_has_a_default_an_environment_form_and_a_range() {
    let load = |capacity: Option<&'static str>| {
        let mut variables = vec![
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                "https://identity.example/token",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                "billing-service",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                "test-private-key",
            ),
            ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
            ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "ES256"),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                "https://identity.example",
            ),
        ];
        if let Some(capacity) = capacity {
            variables.push((
                "APP__INTEGRATIONS__BILLING__OAUTH__EXCHANGE_CACHE_CAPACITY",
                capacity,
            ));
        }
        load_from(&LoadOptions::default(), BUILD, env(&variables))
    };
    let capacity = |cfg: Config| {
        cfg.integrations["billing"]
            .oauth
            .as_ref()
            .expect("named OAuth tuple")
            .exchange_cache_capacity
    };
    assert_eq!(capacity(load(None).unwrap()), 1024);
    assert_eq!(capacity(load(Some("4096")).unwrap()), 4096);

    let dir = tempfile::tempdir().unwrap();
    let file = write(
        &dir,
        "capacity.toml",
        "[integrations.billing.oauth]\nexchange_cache_capacity = 2048\n",
    );
    let from_file = load_from(
        &LoadOptions {
            config: Some(file),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                "https://identity.example/token",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                "billing-service",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                "test-private-key",
            ),
            ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
            ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "ES256"),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                "https://identity.example",
            ),
        ]),
    )
    .unwrap();
    assert_eq!(capacity(from_file), 2048);

    for rejected in ["0", "65537"] {
        let err = load(Some(rejected)).unwrap_err();
        assert!(
            matches!(&err, Error::Validate(error) if error.key == "integrations.billing.oauth.exchange_cache_capacity" && error.message == format!("must be between 1 and 65536, got {rejected}")),
            "{err}"
        );
    }
    let err = load(Some("many")).unwrap_err();
    assert!(matches!(&err, Error::Deserialize(_)), "{err}");
    let rendered = err.to_string();
    assert!(
        rendered.contains(
            "invalid value, expected an integer for key `integrations.billing.oauth.exchange_cache_capacity`"
        ),
        "{rendered}"
    );
    assert!(!rendered.contains("many"), "{rendered}");
}

fn load_oauth_provider_concurrency(
    file_value: Option<&str>,
    environment_value: Option<&str>,
) -> Result<Config, Error> {
    let dir = tempfile::tempdir().unwrap();
    let mut content = String::from(
        "[integrations.billing.oauth]\n\
         token_url = \"https://identity.example/token\"\n\
         client_id = \"billing-service\"\n\
         key_id = \"key-1\"\n\
         algorithm = \"ES256\"\n\
         assertion_audience = \"https://identity.example\"\n",
    );
    if let Some(value) = file_value {
        use std::fmt::Write as _;

        writeln!(content, "provider_concurrency = {value}").unwrap();
    }
    let file = write(&dir, "provider-concurrency.toml", &content);
    let mut variables = vec![(
        "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
        "test-private-key",
    )];
    if let Some(value) = environment_value {
        variables.push((
            "APP__INTEGRATIONS__BILLING__OAUTH__PROVIDER_CONCURRENCY",
            value,
        ));
    }
    load_from(
        &LoadOptions {
            config: Some(file),
            ..LoadOptions::default()
        },
        BUILD,
        env(&variables),
    )
}

#[test]
fn oauth_provider_concurrency_defaults_and_loads_file_and_environment_boundaries() {
    for (file, environment, expected) in [
        (None, None, 32),
        (Some("7"), None, 7),
        (Some("\"9\""), None, 9),
        (Some("7"), Some("11"), 11),
        (Some("1"), None, 1),
        (None, Some("1"), 1),
        (Some("4294967295"), None, u32::MAX),
        (None, Some("4294967295"), u32::MAX),
    ] {
        let cfg = load_oauth_provider_concurrency(file, environment).unwrap();
        assert_eq!(
            cfg.integrations["billing"]
                .oauth
                .as_ref()
                .unwrap()
                .provider_concurrency
                .get(),
            expected,
            "file={file:?}, environment={environment:?}"
        );
    }
}

#[test]
fn oauth_provider_concurrency_rejects_invalid_scalars_without_echoing_values() {
    for (file, environment) in [
        ("0", "0"),
        ("-1", "-1"),
        ("1.5", "1.5"),
        ("1.0", "1.0"),
        ("true", "true"),
        ("4294967296", "4294967296"),
        ("\"private-sentinel\"", "private-sentinel"),
        ("\"\"", ""),
        ("[1]", "[1]"),
    ] {
        for (file, environment) in [(Some(file), None), (None, Some(environment))] {
            let err = load_oauth_provider_concurrency(file, environment).unwrap_err();
            assert!(matches!(&err, Error::Deserialize(_)), "{err}");
            let rendered = err.to_string();
            assert!(
                rendered.contains(
                    "must be an integer from 1 to 4294967295 for key `integrations.billing.oauth.provider_concurrency`"
                ),
                "{rendered}"
            );
            assert!(!rendered.contains("private-sentinel"), "{rendered}");
        }
    }
}

#[test]
fn oauth_algorithm_accepts_ps256_and_es256() {
    for (value, expected) in [
        ("PS256", crate::OAuthAlgorithm::Ps256),
        ("ES256", crate::OAuthAlgorithm::Es256),
    ] {
        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                    "https://identity.example/token",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                    "billing-service",
                ),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                    "test-private-key",
                ),
                ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
                (
                    "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                    "https://identity.example",
                ),
                ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", value),
            ]),
        )
        .unwrap();
        let oauth = cfg
            .integrations
            .get("billing")
            .and_then(|integration| integration.oauth.as_ref())
            .expect("named OAuth tuple");
        assert_eq!(oauth.algorithm, expected, "algorithm {value}");
    }
}

#[test]
fn oauth_bad_algorithm_fails_without_echoing_the_value() {
    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                "https://identity.example/token",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                "billing-service",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                "test-private-key",
            ),
            ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                "https://identity.example",
            ),
            ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "HS256"),
        ]),
    )
    .unwrap_err();
    assert!(matches!(&err, Error::Deserialize(_)), "{err}");
    let rendered = err.to_string();
    assert!(
        rendered.contains(
            "must be RS256, PS256 or ES256 for key `integrations.billing.oauth.algorithm`"
        ),
        "{rendered}"
    );
    assert!(!rendered.contains("HS256"), "{rendered}");
}

#[test]
fn oauth_client_secret_is_refused_as_an_unknown_key() {
    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                "https://identity.example/token",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                "billing-service",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_SECRET",
                "test-client-secret",
            ),
        ]),
    )
    .unwrap_err();
    assert!(matches!(&err, Error::Deserialize(_)), "{err}");
    let rendered = err.to_string();
    assert!(
        rendered.contains("unknown field `client_secret`")
            && rendered.contains("for key `integrations.billing.oauth`"),
        "{rendered}"
    );
    assert!(!rendered.contains("test-client-secret"), "{rendered}");
}

#[test]
fn oauth_private_key_in_a_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let leaked = write(
        &dir,
        "leaked.toml",
        "[integrations.billing.oauth]\nprivate_key = \"test-private-key\"\n",
    );
    let err = load_from(
        &LoadOptions {
            config: Some(leaked),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[]),
    )
    .unwrap_err();
    assert!(
        matches!(&err, Error::SecretInFile { key, .. } if key == "integrations.billing.oauth.private_key"),
        "{err}"
    );
}

#[test]
fn oauth_invalid_values_fail_with_static_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let invalid = write(
        &dir,
        "invalid.toml",
        "[integrations.billing.oauth]\ntoken_url = \"http://identity.example/token?private=value\"\nclient_id = \"billing-service\"\nscopes = [\"bad scope\"]\naudience = \"private-audience\"\n",
    );
    let err = load_from(
        &LoadOptions {
            config: Some(invalid),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[
            ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "RS256"),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                "test-private-key",
            ),
            ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                "https://identity.example",
            ),
        ]),
    )
    .unwrap_err();
    assert!(
        matches!(&err, Error::Validate(error) if error.key == "integrations.billing.oauth.token_url" && error.message == "must use HTTPS"),
        "{err}"
    );
    let rendered = err.to_string();
    for value in [
        "identity.example/token",
        "private=value",
        "private-audience",
    ] {
        assert!(
            !rendered.contains(value),
            "configuration error exposed {value}"
        );
    }
}

#[test]
fn oauth_invalid_types_have_static_key_diagnostics() {
    let dir = tempfile::tempdir().unwrap();
    let wrong_type = write(
        &dir,
        "wrong-type.toml",
        "[integrations.billing.oauth]\ntoken_url = [\"private-sentinel\"]\nclient_id = \"billing-service\"\n",
    );
    let err = load_from(
        &LoadOptions {
            config: Some(wrong_type),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                "test-private-key",
            ),
            ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                "https://identity.example",
            ),
        ]),
    )
    .unwrap_err();
    assert!(matches!(&err, Error::Deserialize(_)), "{err}");
    let rendered = err.to_string();
    assert!(
        rendered.contains(
            "invalid value, expected a string for key `integrations.billing.oauth.token_url`"
        ),
        "{rendered}"
    );
    assert!(!rendered.contains("private-sentinel"), "{rendered}");
    assert!(!rendered.contains("in the environment"), "{rendered}");

    for (name, contents, expected_key) in [
        (
            "invalid-integration.toml",
            "[integrations]\nbilling = \"private-sentinel\"\n",
            "integrations.billing",
        ),
        (
            "invalid-oauth.toml",
            "[integrations.billing]\noauth = \"private-sentinel\"\n",
            "integrations.billing.oauth",
        ),
    ] {
        let malformed = write(&dir, name, contents);
        let err = load_from(
            &LoadOptions {
                config: Some(malformed),
                ..LoadOptions::default()
            },
            BUILD,
            env(&[]),
        )
        .unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains(expected_key), "{rendered}");
        assert!(!rendered.contains("private-sentinel"), "{rendered}");
    }
}

#[test]
fn oauth_scope_presence_and_client_id_validation_stays_private() {
    let dir = tempfile::tempdir().unwrap();
    let invalid_scope = write(
        &dir,
        "invalid-scope.toml",
        "[integrations.billing.oauth]\ntoken_url = \"https://identity.example/token\"\nclient_id = \"billing-service\"\nscopes = [\"bad scope\"]\n",
    );
    let err = load_from(
        &LoadOptions {
            config: Some(invalid_scope),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[
            ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "RS256"),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                "test-private-key",
            ),
            ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                "https://identity.example",
            ),
        ]),
    )
    .unwrap_err();
    assert!(
        matches!(&err, Error::Validate(error) if error.key == "integrations.billing.oauth.scopes" && error.message == "must contain RFC 6749 scope tokens"),
        "{err}"
    );
    assert!(!err.to_string().contains("bad scope"), "{err}");

    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "RS256"),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                "https://identity.example/token",
            ),
        ]),
    )
    .unwrap_err();
    assert!(
        matches!(&err, Error::Validate(error) if error.key == "integrations.billing.oauth.client_id" && error.message == "cannot be empty"),
        "{err}"
    );
}

#[test]
fn oauth_blank_new_required_keys_are_refused() {
    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "RS256"),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                "https://identity.example/token",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                "billing-service",
            ),
        ]),
    )
    .unwrap_err();
    assert!(
        matches!(&err, Error::Validate(error) if error.key == "integrations.billing.oauth.private_key" && error.message == "is required when integrations.billing.oauth is set; set APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY"),
        "{err}"
    );

    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "RS256"),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                "https://identity.example/token",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                "billing-service",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                "test-private-key",
            ),
        ]),
    )
    .unwrap_err();
    assert!(
        matches!(&err, Error::Validate(error) if error.key == "integrations.billing.oauth.key_id" && error.message == "cannot be empty"),
        "{err}"
    );

    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "RS256"),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                "https://identity.example/token",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                "billing-service",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                "test-private-key",
            ),
            ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
        ]),
    )
    .unwrap_err();
    assert!(
        matches!(&err, Error::Validate(error) if error.key == "integrations.billing.oauth.assertion_audience" && error.message == "cannot be empty"),
        "{err}"
    );
}

#[test]
fn oauth_whitespace_audience_is_refused() {
    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "RS256"),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                "https://identity.example/token",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                "billing-service",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                "test-private-key",
            ),
            ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                "https://identity.example",
            ),
            ("APP__INTEGRATIONS__BILLING__OAUTH__AUDIENCE", "   "),
        ]),
    )
    .unwrap_err();
    assert!(
        matches!(&err, Error::Validate(error) if error.key == "integrations.billing.oauth.audience"),
        "{err}"
    );
}

#[test]
fn oauth_userinfo_validation_stays_private() {
    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            ("APP__INTEGRATIONS__BILLING__OAUTH__ALGORITHM", "RS256"),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__TOKEN_URL",
                "https://@identity.example/token",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__CLIENT_ID",
                "billing-service",
            ),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__PRIVATE_KEY",
                "test-private-key",
            ),
            ("APP__INTEGRATIONS__BILLING__OAUTH__KEY_ID", "key-1"),
            (
                "APP__INTEGRATIONS__BILLING__OAUTH__ASSERTION_AUDIENCE",
                "https://identity.example",
            ),
        ]),
    )
    .unwrap_err();
    assert!(
        matches!(&err, Error::Validate(error) if error.key == "integrations.billing.oauth.token_url" && error.message == "must not include userinfo"),
        "{err}"
    );
}
