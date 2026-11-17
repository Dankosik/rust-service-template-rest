use super::*;
use crate::LoadOptions;
use crate::load::load_from;
use crate::test_support::{BUILD, env};
// template:begin oidc-jwt:authn-config-test-write-import
use crate::test_support::write;
// template:end oidc-jwt:authn-config-test-write-import
use crate::validate::admit_with;
use std::assert_matches;

fn parse(toml: &str) -> Result<AuthnInput, toml::de::Error> {
    toml::from_str(toml)
}

#[test]
fn omitted_section_defaults_to_inert_none() {
    let config = AuthnInput::default();
    assert_matches!(admit_with(|v| config.admit(v)), Ok(AuthnConfig::None));
}

// template:begin oidc-jwt:authn-config-jwt-tests
#[test]
fn jwt_loads_algorithm_and_audience_lists_from_variables() {
    let dir = tempfile::tempdir().unwrap();
    let config = write(
        &dir,
        "authn.toml",
        "[authn]\nmode = \"oidc-jwt\"\nissuer = \"issuer\"\nalgorithms = [\"RS256\"]\n",
    );
    write(&dir, "APP__AUTHN__AUDIENCES", r#"["mounted"]"#);
    let options = LoadOptions {
        config: Some(config),
        secrets_dir: Some(dir.path().to_owned()),
        ..LoadOptions::default()
    };
    let environment = env(&[
        ("APP__AUTHN__TOKEN_PROFILE", "resource-server"),
        ("APP__AUTHN__JWKS_MAX_AGE", "1h"),
        ("APP__AUTHN__ALGORITHMS", "ES256, EdDSA"),
        ("APP__AUTHN__AUDIENCES", r#"["api-a", " api-b ", "api-a"]"#),
    ]);
    let loaded = load_from(&options, BUILD, environment.clone()).unwrap();
    let AuthnConfig::OidcJwt {
        audience,
        algorithms,
        ..
    } = &loaded.authn
    else {
        panic!("expected OIDC JWT configuration");
    };
    assert_eq!(audience.as_slice(), ["api-a", " api-b "]);
    assert_eq!(
        algorithms.as_slice(),
        [JwtAlgorithm::Es256, JwtAlgorithm::EdDsa]
    );

    write(&dir, "APP__AUTHN__AUDIENCE", "conflicting");
    let conflict = load_from(&options, BUILD, environment).unwrap_err();
    assert_matches!(
        &conflict,
        crate::Error::Validate(errors) if errors.names("authn.audiences"),
        "{conflict}"
    );
}

#[test]
fn jwt_load_errors_name_the_rejected_field() {
    for (audience, algorithms, expected) in [
        (
            " ",
            "ES256",
            "expected a nonempty string or list of nonblank exact values for key `authn.audience`",
        ),
        (
            "service",
            "ES256,invalid-jwt-algorithm",
            "expected a list of supported JWT algorithms or a comma-separated string for key `authn.algorithms`",
        ),
    ] {
        let error = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[
                ("APP__AUTHN__MODE", "oidc-jwt"),
                ("APP__AUTHN__ISSUER", "issuer"),
                ("APP__AUTHN__AUDIENCE", audience),
                ("APP__AUTHN__ALGORITHMS", algorithms),
            ]),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains("invalid-jwt-algorithm"), "{error}");
    }
}

#[test]
fn jwt_load_errors_describe_the_expected_field_shape() {
    let dir = tempfile::tempdir().unwrap();
    for (audience, algorithms, expected) in [
        (
            r#"["rejected-value", 7]"#,
            r#"["RS256"]"#,
            "expected a string or a list of strings for key `authn.audience`",
        ),
        (
            "2026-11-13t12:00:00z",
            r#"["RS256"]"#,
            "key `authn.audience`",
        ),
        (
            r#"["service", 2026-11-13]"#,
            r#"["RS256"]"#,
            "key `authn.audience`",
        ),
        (
            r#""service""#,
            r#"["rejected-value"]"#,
            "expected a list of supported JWT algorithms or a comma-separated string for key `authn.algorithms`",
        ),
    ] {
        let config = write(
            &dir,
            "authn.toml",
            &format!(
                "[authn]\nmode = \"oidc-jwt\"\nissuer = \"issuer\"\naudience = {audience}\nalgorithms = {algorithms}\n"
            ),
        );
        let options = LoadOptions {
            config: Some(config),
            ..LoadOptions::default()
        };
        let error = load_from(&options, BUILD, env(&[]))
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains("rejected-value"), "{error}");
    }
}

#[test]
fn jwt_load_accepts_a_quoted_date_or_list_from_a_later_file() {
    let dir = tempfile::tempdir().unwrap();
    let base = write(
        &dir,
        "base.toml",
        "[authn]\nmode = \"oidc-jwt\"\nissuer = \"issuer\"\naudience = 2026-11-13\ntoken_profile = \"resource-server\"\njwks_max_age = \"1h\"\n",
    );
    for (input, expected) in [
        (r#""2026-11-13""#, &["2026-11-13"][..]),
        (
            r#"["2026-11-13", "service"]"#,
            &["2026-11-13", "service"][..],
        ),
    ] {
        let options = LoadOptions {
            config: Some(base.clone()),
            config_overlay: vec![write(
                &dir,
                "overlay.toml",
                &format!("authn.audience = {input}\n"),
            )],
            ..LoadOptions::default()
        };
        let cfg = load_from(&options, BUILD, env(&[])).unwrap();
        let AuthnConfig::OidcJwt { audience, .. } = &cfg.authn else {
            panic!("expected OIDC JWT configuration");
        };
        assert_eq!(audience.as_slice(), expected);
    }
}

#[test]
fn jwt_normalizes_scalar_or_list_audiences_without_changing_values() {
    let scalar = parse(
        r#"
        mode = "oidc-jwt"
        issuer = " issuer grammar belongs to the adapter "
        token_profile = "resource-server"
        jwks_max_age = "1h"
        audience = "service"
        "#,
    )
    .unwrap();
    let list = parse(
        r#"
        mode = "oidc-jwt"
        issuer = "issuer"
        audience = ["service", " api ", "service"]
        algorithms = ["ES256", "EdDSA"]
        token_profile = "rfc9068"
        jwks_max_age = "1h"
        "#,
    )
    .unwrap();

    let scalar = admit_with(|v| scalar.admit(v)).unwrap();
    let list = admit_with(|v| list.admit(v)).unwrap();

    let AuthnConfig::OidcJwt {
        audience,
        token_profile,
        algorithms,
        ..
    } = scalar
    else {
        panic!("expected OIDC JWT configuration");
    };
    assert_eq!(audience.as_slice(), ["service"]);
    assert_eq!(token_profile, TokenProfile::ResourceServer);
    assert_eq!(algorithms, [JwtAlgorithm::Rs256]);

    let AuthnConfig::OidcJwt {
        audience,
        token_profile,
        algorithms,
        ..
    } = list
    else {
        panic!("expected OIDC JWT configuration");
    };
    assert_eq!(audience.as_slice(), ["service", " api "]);
    assert_eq!(token_profile, TokenProfile::Rfc9068);
    assert_eq!(algorithms, [JwtAlgorithm::Es256, JwtAlgorithm::EdDsa]);
}

#[test]
fn jwt_rejects_empty_algorithms_and_invalid_audience_shape() {
    let empty_algorithms = parse(
        r#"
        mode = "oidc-jwt"
        issuer = "issuer"
        audience = "service"
        algorithms = []
        token_profile = "resource-server"
        jwks_max_age = "1h"
        "#,
    )
    .unwrap();
    assert!(
        admit_with(|v| empty_algorithms.admit(v))
            .unwrap_err()
            .names("authn.algorithms")
    );

    for audience in ["[]", "[\"service\", \"  \"]", "\"  \""] {
        let source = format!("mode = \"oidc-jwt\"\nissuer = \"issuer\"\naudience = {audience}\n");
        assert!(parse(&source).is_err(), "{source}");
    }
}

#[test]
fn jwt_accepts_an_optional_nonblank_jwks_uri() {
    let source = |extra: &str| {
        parse(&format!(
            "mode = \"oidc-jwt\"\nissuer = \"issuer\"\naudience = \"service\"\ntoken_profile = \"rfc9068\"\njwks_max_age = \"1h\"\n{extra}"
        ))
        .unwrap()
    };
    let jwks_uri = |config: AuthnConfig| {
        let AuthnConfig::OidcJwt { jwks_uri, .. } = config else {
            panic!("expected OIDC JWT configuration");
        };
        jwks_uri
    };
    assert_eq!(jwks_uri(admit_with(|v| source("").admit(v)).unwrap()), None);
    let explicit = source("jwks_uri = \"https://issuer.example/keys\"\n");
    let explicit = admit_with(|v| explicit.admit(v)).unwrap();
    assert_eq!(
        jwks_uri(explicit).as_deref(),
        Some("https://issuer.example/keys")
    );
    let blank = source("jwks_uri = \"  \"\n");
    assert!(
        admit_with(|v| blank.admit(v))
            .unwrap_err()
            .names("authn.jwks_uri")
    );
}

#[test]
fn jwt_debug_redacts_trust_inputs() {
    let config = parse(
        r#"
        mode = "oidc-jwt"
        issuer = "https://private-issuer.example"
        audience = "private-audience"
        token_profile = "resource-server"
        jwks_max_age = "1h"
        "#,
    )
    .unwrap();
    let config = admit_with(|v| config.admit(v)).unwrap();
    let diagnostic = format!("{config:?}");
    assert!(diagnostic.contains("oidc-jwt"));
    assert!(!diagnostic.contains("private-issuer"));
    assert!(!diagnostic.contains("private-audience"));
}
// template:end oidc-jwt:authn-config-jwt-tests

// template:begin oidc-introspection:authn-config-introspection-tests
#[test]
fn introspection_requires_structural_inputs_and_uses_nonzero_default_capacity() {
    let config = parse(
        r#"
        mode = "oidc-introspection"
        issuer = "issuer"
        audience = ["service"]
        introspection_endpoint = "endpoint"
        introspection_client_id = "client"
        introspection_client_secret = "secret"
        "#,
    )
    .unwrap();
    let config = admit_with(|v| config.admit(v)).unwrap();
    let AuthnConfig::OidcIntrospection {
        provider_concurrency,
        cache_enabled,
        cache_capacity,
        cache_ttl,
        ..
    } = &config
    else {
        panic!("expected OIDC introspection configuration");
    };
    assert_eq!(provider_concurrency.get(), 32);
    assert!(!cache_enabled);
    assert_eq!(*cache_capacity, 256);
    assert_eq!(*cache_ttl, Duration::from_secs(30));

    let missing_secret = parse(
        r#"
        mode = "oidc-introspection"
        issuer = "issuer"
        audience = "service"
        introspection_endpoint = "endpoint"
        introspection_client_id = "client"
        "#,
    )
    .unwrap();
    assert!(
        admit_with(|v| missing_secret.admit(v))
            .unwrap_err()
            .names("authn.introspection_client_secret")
    );
    assert!(
        parse(
            r#"
        mode = "oidc-introspection"
        issuer = "issuer"
        audience = "service"
        introspection_endpoint = "endpoint"
        introspection_client_id = "client"
        introspection_client_secret = "secret"
        provider_concurrency = 0
        "#,
        )
        .is_err()
    );
}

#[test]
fn introspection_debug_redacts_trust_inputs_and_credentials() {
    let config = parse(
        r#"
        mode = "oidc-introspection"
        issuer = "https://private-issuer.example"
        audience = "private-audience"
        introspection_endpoint = "https://private-endpoint.example/check?secret=query-value"
        introspection_client_id = "private-client"
        introspection_client_secret = "private-secret"
        "#,
    )
    .unwrap();
    let config = admit_with(|v| config.admit(v)).unwrap();
    let diagnostic = format!("{config:?}");
    assert!(diagnostic.contains("oidc-introspection"));
    for private_value in [
        "private-issuer",
        "private-audience",
        "private-endpoint",
        "query-value",
        "private-client",
        "private-secret",
    ] {
        assert!(!diagnostic.contains(private_value));
    }
}
// template:end oidc-introspection:authn-config-introspection-tests

#[test]
fn modes_reject_foreign_provider_fields() {
    let none = parse("mode = \"none\"\n").unwrap();
    assert_matches!(admit_with(|v| none.admit(v)), Ok(AuthnConfig::None));

    let assert_rejected_field = |source: &str, expected_key: &str| match parse(source) {
        Ok(input) => {
            let errors = admit_with(|v| input.admit(v)).unwrap_err();
            assert!(errors.names(expected_key), "{source}: {errors}");
        }
        Err(err) => {
            let rendered = err.to_string();
            let field = expected_key.strip_prefix("authn.").unwrap_or(expected_key);
            assert!(rendered.contains(field), "{source}: {rendered}");
        }
    };

    for (source, expected_key) in [
        ("mode = \"none\"\nissuer = \"issuer\"\n", "authn.issuer"),
        // template:begin oidc-jwt:authn-foreign-fields-jwt-case
        (
            "mode = \"none\"\njwks_max_age = \"1h\"\n",
            "authn.jwks_max_age",
        ),
        (
            "mode = \"oidc-jwt\"\nissuer = \"issuer\"\naudience = \"service\"\nintrospection_endpoint = \"endpoint\"\n",
            "authn.introspection_endpoint",
        ),
        // template:end oidc-jwt:authn-foreign-fields-jwt-case
        // template:begin oidc-introspection:authn-foreign-fields-introspection-case
        (
            "mode = \"oidc-introspection\"\nissuer = \"issuer\"\naudience = \"service\"\nintrospection_endpoint = \"endpoint\"\nintrospection_client_id = \"client\"\nintrospection_client_secret = \"secret\"\nalgorithms = [\"RS256\"]\n",
            "authn.algorithms",
        ),
        // template:end oidc-introspection:authn-foreign-fields-introspection-case
    ] {
        assert_rejected_field(source, expected_key);
    }
    let assert_rejected_cache_fields = |base: &str| {
        for (field, expected_key) in [
            ("cache_enabled = false", "authn.cache_enabled"),
            ("cache_capacity = 256", "authn.cache_capacity"),
            ("cache_ttl = \"30s\"", "authn.cache_ttl"),
        ] {
            let source = format!("{base}{field}\n");
            assert_rejected_field(&source, expected_key);
        }
    };
    assert_rejected_cache_fields("mode = \"none\"\n");
    // template:begin oidc-jwt:authn-foreign-cache-fields-jwt-case
    assert_rejected_cache_fields(
        "mode = \"oidc-jwt\"\nissuer = \"issuer\"\naudience = \"service\"\n",
    );
    // template:end oidc-jwt:authn-foreign-cache-fields-jwt-case
}

// template:begin oidc-introspection:load-introspection-environment
fn introspection_environment(extra: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut values = env(&[
        ("APP__AUTHN__MODE", "oidc-introspection"),
        ("APP__AUTHN__ISSUER", "https://issuer.example/tenant"),
        ("APP__AUTHN__AUDIENCE", "00123"),
        (
            "APP__AUTHN__INTROSPECTION_ENDPOINT",
            "https://issuer.example/introspect",
        ),
        ("APP__AUTHN__INTROSPECTION_CLIENT_ID", "false"),
        ("APP__AUTHN__INTROSPECTION_CLIENT_SECRET", "000042"),
    ]);
    values.extend(env(extra));
    values
}

#[test]
fn introspection_environment_decodes_and_redacts_the_secret() {
    use secrecy::ExposeSecret;

    for (enabled, expected) in [("true", true), ("false", false)] {
        let cfg = load_from(
            &LoadOptions::default(),
            BUILD,
            introspection_environment(&[
                ("APP__AUTHN__PROVIDER_CONCURRENCY", "7"),
                ("APP__AUTHN__CACHE_ENABLED", enabled),
                ("APP__AUTHN__CACHE_CAPACITY", "64"),
                ("APP__AUTHN__CACHE_TTL", "1m 30s"),
            ]),
        )
        .unwrap();
        let AuthnConfig::OidcIntrospection {
            audience,
            introspection_client_id,
            introspection_client_secret,
            provider_concurrency,
            cache_enabled,
            cache_capacity,
            cache_ttl,
            ..
        } = &cfg.authn
        else {
            panic!("expected OIDC introspection configuration");
        };
        assert_eq!(*cache_enabled, expected);
        assert_eq!(*cache_capacity, 64);
        assert_eq!(*cache_ttl, Duration::from_secs(90));
        assert_eq!(provider_concurrency.get(), 7);
        assert_eq!(audience.as_slice(), ["00123"]);
        assert_eq!(introspection_client_id, "false");
        assert_eq!(introspection_client_secret.expose_secret(), "000042");
        assert!(!format!("{cfg:?}").contains("000042"));
    }
}

#[test]
fn audience_list_is_explicit_and_refuses_conflicting_or_invalid_input() {
    let options = LoadOptions::default();
    let literal = r#"["api-a", "api-b"]"#;
    let mut environment = introspection_environment(&[]);
    let single = environment
        .iter_mut()
        .find(|(name, _)| name == "APP__AUTHN__AUDIENCE")
        .unwrap();
    single.1 = literal.to_owned();
    let config = load_from(&options, BUILD, environment.clone()).unwrap();
    let AuthnConfig::OidcIntrospection { audience, .. } = &config.authn else {
        panic!("expected OIDC introspection configuration");
    };
    assert_eq!(audience.as_slice(), [literal]);

    environment.push(("APP__AUTHN__AUDIENCES".to_owned(), literal.to_owned()));
    let error = load_from(&options, BUILD, environment.clone()).unwrap_err();
    assert_matches!(
        &error,
        crate::Error::Validate(errors) if errors.names("authn.audiences"),
        "{error}"
    );

    environment.retain(|(name, _)| name != "APP__AUTHN__AUDIENCE");
    let config = load_from(&options, BUILD, environment.clone()).unwrap();
    let AuthnConfig::OidcIntrospection { audience, .. } = &config.authn else {
        panic!("expected OIDC introspection configuration");
    };
    assert_eq!(audience.as_slice(), ["api-a", "api-b"]);

    environment.last_mut().unwrap().1 = "malformed-list".to_owned();
    let error = load_from(&options, BUILD, environment)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains(
            "expected a TOML array of nonblank strings for key `authn.audiences` in the environment"
        ),
        "{error}"
    );
    assert!(!error.contains("malformed-list"), "{error}");
}

#[test]
fn introspection_environment_rejects_out_of_range_cache_settings_even_when_disabled() {
    for (variable, value, expected_key) in [
        ("APP__AUTHN__CACHE_CAPACITY", "0", "authn.cache_capacity"),
        ("APP__AUTHN__CACHE_CAPACITY", "1025", "authn.cache_capacity"),
        ("APP__AUTHN__CACHE_TTL", "999ms", "authn.cache_ttl"),
        ("APP__AUTHN__CACHE_TTL", "301s", "authn.cache_ttl"),
    ] {
        let error = load_from(
            &LoadOptions::default(),
            BUILD,
            introspection_environment(&[("APP__AUTHN__CACHE_ENABLED", "false"), (variable, value)]),
        )
        .unwrap_err();
        assert_matches!(
            &error,
            crate::Error::Validate(error) if error.names(expected_key),
            "{error}"
        );
    }
}

#[test]
fn introspection_environment_rejects_invalid_scalar_overrides() {
    for (key, value, expected) in [
        (
            "APP__AUTHN__CACHE_ENABLED",
            "",
            "expected a boolean for key `authn.cache_enabled` in the environment",
        ),
        (
            "APP__AUTHN__CACHE_ENABLED",
            "not-a-boolean",
            "expected a boolean for key `authn.cache_enabled` in the environment",
        ),
        (
            "APP__AUTHN__CACHE_CAPACITY",
            "",
            "expected an integer for key `authn.cache_capacity` in the environment",
        ),
        (
            "APP__AUTHN__CACHE_CAPACITY",
            "-1",
            "expected an integer for key `authn.cache_capacity` in the environment",
        ),
        (
            "APP__AUTHN__CACHE_CAPACITY",
            "1.5",
            "expected an integer for key `authn.cache_capacity` in the environment",
        ),
        (
            "APP__AUTHN__PROVIDER_CONCURRENCY",
            "",
            "expected an integer for key `authn.provider_concurrency` in the environment",
        ),
        (
            "APP__AUTHN__PROVIDER_CONCURRENCY",
            "0",
            "expected an integer for key `authn.provider_concurrency` in the environment",
        ),
        (
            "APP__AUTHN__PROVIDER_CONCURRENCY",
            "4294967296",
            "expected an integer for key `authn.provider_concurrency` in the environment",
        ),
    ] {
        let rendered = load_from(
            &LoadOptions::default(),
            BUILD,
            introspection_environment(&[(key, value)]),
        )
        .unwrap_err()
        .to_string();
        assert!(rendered.contains(expected), "{key}={value}: {rendered}");
        assert!(!rendered.contains("not-a-boolean"), "{rendered}");
    }
}
// template:end oidc-introspection:load-introspection-environment

// template:begin oidc-jwt:load-jwt-token-profile-environment
#[test]
fn jwt_environment_preserves_a_supplied_token_profile_and_jwks_uri() {
    let jwt_environment = |jwks_uri: &'static str| {
        env(&[
            ("APP__AUTHN__MODE", "oidc-jwt"),
            ("APP__AUTHN__ISSUER", "https://issuer.example/tenant"),
            ("APP__AUTHN__AUDIENCE", "service"),
            ("APP__AUTHN__TOKEN_PROFILE", "rfc9068"),
            ("APP__AUTHN__JWKS_MAX_AGE", "1m"),
            ("APP__AUTHN__JWKS_URI", jwks_uri),
        ])
    };
    let jwt = load_from(
        &LoadOptions::default(),
        BUILD,
        jwt_environment("https://issuer.example/keys"),
    )
    .unwrap();
    let AuthnConfig::OidcJwt {
        token_profile,
        jwks_uri,
        jwks_max_age,
        ..
    } = &jwt.authn
    else {
        panic!("expected OIDC JWT configuration");
    };
    assert_eq!(*token_profile, TokenProfile::Rfc9068);
    assert_eq!(jwks_uri.as_deref(), Some("https://issuer.example/keys"));
    assert_eq!(*jwks_max_age, std::time::Duration::from_mins(1));
    assert!(load_from(&LoadOptions::default(), BUILD, jwt_environment(" ")).is_err());
}
// template:end oidc-jwt:load-jwt-token-profile-environment

// template:begin oidc-introspection:load-introspection-secret-file
#[test]
fn introspection_secret_in_a_file_is_refused() {
    crate::test_support::assert_secret_in_file(
        "[authn]\nintrospection_client_secret = \"file-secret\"\n",
        "authn.introspection_client_secret",
    );
}
// template:end oidc-introspection:load-introspection-secret-file

// template:begin oidc-jwt:load-jwt-token-profile-file
#[test]
fn the_projected_jwt_overlay_selects_rfc9068_with_service_supplied_trust_inputs() {
    let configs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../env/config");
    let cfg = load_from(
        &LoadOptions {
            config: Some(configs.join("local.toml")),
            config_overlay: vec![configs.join("oidc-jwt.toml")],
            ..LoadOptions::default()
        },
        BUILD,
        env(&[
            ("APP__AUTHN__ISSUER", "https://issuer.example"),
            ("APP__AUTHN__AUDIENCE", "service"),
            ("APP__AUTHN__JWKS_MAX_AGE", "1h"),
        ]),
    )
    .unwrap();
    let AuthnConfig::OidcJwt { token_profile, .. } = &cfg.authn else {
        panic!("expected OIDC JWT configuration");
    };
    assert_eq!(*token_profile, TokenProfile::Rfc9068);
}

#[test]
fn enabling_jwt_requires_an_explicit_profile_and_supported_maximum_key_age() {
    let base = "mode = \"oidc-jwt\"\nissuer = \"issuer\"\naudience = \"service\"\n";
    for (extra, key) in [
        ("jwks_max_age = \"1h\"", "authn.token_profile"),
        ("token_profile = \"resource-server\"", "authn.jwks_max_age"),
        (
            "token_profile = \"rfc9068\"\njwks_max_age = \"0s\"",
            "authn.jwks_max_age",
        ),
        (
            "token_profile = \"rfc9068\"\njwks_max_age = \"59999ms\"",
            "authn.jwks_max_age",
        ),
    ] {
        let input = parse(&format!("{base}{extra}")).unwrap();
        assert!(admit_with(|v| input.admit(v)).unwrap_err().names(key));
    }
}
// template:end oidc-jwt:load-jwt-token-profile-file
