use super::*;
use crate::LoadOptions;
use crate::load::load_from;
use crate::test_support::{BUILD, env, write};
use std::assert_matches;

fn parse(toml: &str) -> Result<AuthnConfig, toml::de::Error> {
    toml::from_str(toml)
}

#[test]
fn omitted_section_defaults_to_inert_none() {
    let config = AuthnConfig::default();
    assert_matches!(config, AuthnConfig::None {});
    config.validate().unwrap();
}

// template:begin oidc-jwt:authn-config-jwt-tests
#[test]
fn jwt_loads_algorithm_and_audience_lists_from_variables() {
    let dir = tempfile::tempdir().unwrap();
    let config = write(
        &dir,
        "authn.toml",
        "[authn]\nmode = \"oidc-jwt\"\nissuer = \"issuer\"\naudience = [\"base\"]\nalgorithms = [\"RS256\"]\n",
    );
    write(&dir, "APP__AUTHN__AUDIENCE", "mounted");
    let options = LoadOptions {
        config: Some(config),
        secrets_dir: Some(dir.path().to_owned()),
        ..LoadOptions::default()
    };
    let environment = env(&[
        ("APP__AUTHN__ALGORITHMS", "ES256, EdDSA"),
        ("APP__AUTHN__AUDIENCES", r#"["api-a", " api-b ", "api-a"]"#),
    ]);
    let config = load_from(&options, BUILD, environment).unwrap();
    let AuthnConfig::OidcJwt {
        audience,
        algorithms,
        ..
    } = config.authn
    else {
        panic!("expected OIDC JWT configuration");
    };
    assert_eq!(audience.as_slice(), ["api-a", " api-b "]);
    assert_eq!(algorithms, [JwtAlgorithm::Es256, JwtAlgorithm::EdDsa]);
}

#[test]
fn jwt_normalizes_scalar_or_list_audiences_without_changing_values() {
    let scalar = parse(
        r#"
        mode = "oidc-jwt"
        issuer = " issuer grammar belongs to the adapter "
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
        "#,
    )
    .unwrap();

    // URL grammar is adapter-owned; only required config shape is checked here.
    scalar.validate().unwrap();

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
        "#,
    )
    .unwrap();
    assert_eq!(
        empty_algorithms.validate().unwrap_err().key,
        "authn.algorithms"
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
            "mode = \"oidc-jwt\"\nissuer = \"issuer\"\naudience = \"service\"\n{extra}"
        ))
        .unwrap()
    };
    let jwks_uri = |config: AuthnConfig| {
        let AuthnConfig::OidcJwt { jwks_uri, .. } = config else {
            panic!("expected OIDC JWT configuration");
        };
        jwks_uri
    };
    assert_eq!(jwks_uri(source("")), None);
    let explicit = source("jwks_uri = \"https://issuer.example/keys\"\n");
    explicit.validate().unwrap();
    assert_eq!(
        jwks_uri(explicit).as_deref(),
        Some("https://issuer.example/keys")
    );
    assert_eq!(
        source("jwks_uri = \"  \"\n").validate().unwrap_err().key,
        "authn.jwks_uri"
    );
}

#[test]
fn jwt_debug_redacts_trust_inputs() {
    let config = parse(
        r#"
        mode = "oidc-jwt"
        issuer = "https://private-issuer.example"
        audience = "private-audience"
        "#,
    )
    .unwrap();
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
    config.validate().unwrap();

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
    assert_eq!(
        missing_secret.validate().unwrap_err().key,
        "authn.introspection_client_secret"
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
    assert_matches!(parse("mode = \"none\"\n").unwrap(), AuthnConfig::None {});
    for source in [
        "mode = \"none\"\nissuer = \"issuer\"\n",
        "mode = \"oidc-jwt\"\nissuer = \"issuer\"\naudience = \"service\"\nintrospection_endpoint = \"endpoint\"\n",
        "mode = \"oidc-introspection\"\nissuer = \"issuer\"\naudience = \"service\"\nintrospection_endpoint = \"endpoint\"\nintrospection_client_id = \"client\"\nintrospection_client_secret = \"secret\"\nalgorithms = [\"RS256\"]\n",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
    for mode in ["none", "oidc-jwt"] {
        let base = if mode == "none" {
            "mode = \"none\"\n"
        } else {
            "mode = \"oidc-jwt\"\nissuer = \"issuer\"\naudience = \"service\"\n"
        };
        for field in [
            "cache_enabled = false",
            "cache_capacity = 256",
            "cache_ttl = \"30s\"",
        ] {
            let source = format!("{base}{field}\n");
            assert!(parse(&source).is_err(), "{source}");
        }
    }
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
        assert_eq!(
            introspection_client_secret
                .as_ref()
                .unwrap()
                .expose_secret(),
            "000042"
        );
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
    let AuthnConfig::OidcIntrospection { audience, .. } = config.authn else {
        panic!("expected OIDC introspection configuration");
    };
    assert_eq!(audience.as_slice(), [literal]);

    environment.push(("APP__AUTHN__AUDIENCES".to_owned(), literal.to_owned()));
    let error = load_from(&options, BUILD, environment.clone()).unwrap_err();
    assert_matches!(error, crate::load::Error::AmbiguousName { .. });

    environment.retain(|(name, _)| name != "APP__AUTHN__AUDIENCE");
    let config = load_from(&options, BUILD, environment.clone()).unwrap();
    let AuthnConfig::OidcIntrospection { audience, .. } = config.authn else {
        panic!("expected OIDC introspection configuration");
    };
    assert_eq!(audience.as_slice(), ["api-a", "api-b"]);

    environment.last_mut().unwrap().1 = "malformed-list".to_owned();
    let error = load_from(&options, BUILD, environment)
        .unwrap_err()
        .to_string();
    assert!(error.contains("APP__AUTHN__AUDIENCES"), "{error}");
    assert!(!error.contains("malformed-list"), "{error}");
}

#[test]
fn introspection_environment_rejects_invalid_scalar_overrides() {
    for (key, value) in [
        ("APP__AUTHN__CACHE_ENABLED", ""),
        ("APP__AUTHN__CACHE_ENABLED", "not-a-boolean"),
        ("APP__AUTHN__CACHE_CAPACITY", ""),
        ("APP__AUTHN__CACHE_CAPACITY", "-1"),
        ("APP__AUTHN__CACHE_CAPACITY", "1.5"),
        ("APP__AUTHN__PROVIDER_CONCURRENCY", ""),
        ("APP__AUTHN__PROVIDER_CONCURRENCY", "0"),
        ("APP__AUTHN__PROVIDER_CONCURRENCY", "4294967296"),
    ] {
        assert!(
            load_from(
                &LoadOptions::default(),
                BUILD,
                introspection_environment(&[(key, value)]),
            )
            .is_err(),
            "{key}={value}"
        );
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
        ..
    } = jwt.authn
    else {
        panic!("expected OIDC JWT configuration");
    };
    assert_eq!(token_profile, TokenProfile::Rfc9068);
    assert_eq!(jwks_uri.as_deref(), Some("https://issuer.example/keys"));
    assert!(load_from(&LoadOptions::default(), BUILD, jwt_environment(" ")).is_err());
}
// template:end oidc-jwt:load-jwt-token-profile-environment

// template:begin oidc-introspection:load-introspection-secret-file
#[test]
fn introspection_secret_in_a_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let leaked = write(
        &dir,
        "leaked.toml",
        "[authn]\nintrospection_client_secret = \"file-secret\"\n",
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
    assert_matches!(&err, crate::load::Error::SecretInFile { key, .. } if key == "authn.introspection_client_secret", "{err}");
}
// template:end oidc-introspection:load-introspection-secret-file

// template:begin oidc-jwt:load-jwt-token-profile-file
#[test]
fn jwt_token_profile_in_a_file_is_public() {
    let dir = tempfile::tempdir().unwrap();
    let profile = write(
        &dir,
        "profile.toml",
        "[authn]\nmode = \"oidc-jwt\"\nissuer = \"issuer\"\naudience = \"service\"\ntoken_profile = \"rfc9068\"\n",
    );
    let cfg = load_from(
        &LoadOptions {
            config: Some(profile),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[]),
    )
    .unwrap();
    let AuthnConfig::OidcJwt { token_profile, .. } = cfg.authn else {
        panic!("expected OIDC JWT configuration");
    };
    assert_eq!(token_profile, TokenProfile::Rfc9068);
}
// template:end oidc-jwt:load-jwt-token-profile-file
