use super::*;
use crate::LoadOptions;
use crate::load::{Error, load_from};
use crate::test_support::{BUILD, env, write};

/// A jwt-mode section with the required keys, then `extra` lines.
fn jwt(extra: &str) -> ServiceAuthConfig {
    toml::from_str(&format!(
        "mode = \"jwt\"\nname = \"catalog\"\njwt_type = \"example-svc+jwt\"\ntrust = \"{{}}\"\n{extra}"
    ))
    .unwrap()
}

#[test]
fn none_admits_unauthenticated_grpc_only_in_development() {
    let none = ServiceAuthConfig::default();
    for env in ["local", "development"] {
        assert_eq!(none.validate(env, true), Ok(()));
    }
    for env in ["production", "staging", "Production", "dev"] {
        let error = none.validate(env, true).unwrap_err();
        assert_eq!(error.key, "service_auth.mode", "{env}");
    }
    // Without a gRPC listener there is nothing to authenticate.
    assert_eq!(none.validate("production", false), Ok(()));
}

#[test]
fn jwt_needs_name_type_and_trust_and_a_key_with_its_id() {
    assert_eq!(jwt("").validate("production", true), Ok(()));
    let signing = jwt("key_id = \"catalog/1\"\nprivate_key = \"pem\"");
    assert_eq!(signing.validate("production", true), Ok(()));
    for (body, key) in [
        (
            "name = \" \"\njwt_type = \"t\"\ntrust = \"{}\"",
            "service_auth.name",
        ),
        (
            "name = \"catalog\"\njwt_type = \" \"\ntrust = \"{}\"",
            "service_auth.jwt_type",
        ),
        (
            "name = \"catalog\"\njwt_type = \"t\"\ntrust = \" \"",
            "service_auth.trust",
        ),
    ] {
        let config: ServiceAuthConfig = toml::from_str(&format!("mode = \"jwt\"\n{body}")).unwrap();
        assert_eq!(config.validate("production", true).unwrap_err().key, key);
    }
    let config = jwt("key_id = \"catalog/1\"");
    assert_eq!(
        config.validate("local", true).unwrap_err().key,
        "service_auth.private_key"
    );
    let config = jwt("private_key = \"pem\"\nkey_id = \" \"");
    assert_eq!(
        config.validate("local", true).unwrap_err().key,
        "service_auth.key_id"
    );
}

#[test]
fn none_accepts_no_dormant_input_and_debug_names_only_the_mode() {
    assert!(toml::from_str::<ServiceAuthConfig>("mode = \"none\"\nname = \"x\"\n").is_err());
    let config = jwt("key_id = \"catalog/1\"\nprivate_key = \"-----BEGIN PRIVATE KEY-----\"");
    assert_eq!(
        format!("{config:?}"),
        r#"ServiceAuthConfig { mode: "jwt", .. }"#
    );
    assert_eq!(config.mode(), "jwt");
}

#[test]
fn service_auth_environment_decodes_and_the_signing_key_stays_secret() {
    use secrecy::ExposeSecret as _;

    let trust = r#"{"orders": {"keys": []}}"#;
    let cfg = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            ("APP__APP__ENV", "production"),
            ("APP__GRPC__ENABLED", "true"),
            ("APP__GRPC__ADDR", "127.0.0.1:0"),
            ("APP__GRPC__SECURITY", "plaintext"),
            ("APP__SERVICE_AUTH__MODE", "jwt"),
            ("APP__SERVICE_AUTH__NAME", "catalog"),
            ("APP__SERVICE_AUTH__JWT_TYPE", "example-svc+jwt"),
            ("APP__SERVICE_AUTH__TRUST", trust),
            ("APP__SERVICE_AUTH__KEY_ID", "catalog/production/2026-10"),
            ("APP__SERVICE_AUTH__PRIVATE_KEY", "env-signing-key"),
        ]),
    )
    .unwrap();
    let crate::ServiceAuthConfig::Jwt {
        name,
        jwt_type,
        trust: loaded,
        key_id,
        private_key,
    } = &cfg.service_auth
    else {
        panic!("expected jwt service authentication");
    };
    assert_eq!(name, "catalog");
    assert_eq!(jwt_type, "example-svc+jwt");
    assert_eq!(loaded, trust);
    assert_eq!(key_id.as_deref(), Some("catalog/production/2026-10"));
    assert_eq!(
        private_key.as_ref().unwrap().expose_secret(),
        "env-signing-key"
    );
    assert!(!format!("{cfg:?}").contains("env-signing-key"));
}

#[test]
fn service_auth_signing_key_in_a_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let leaked = write(
        &dir,
        "leaked.toml",
        "[service_auth]\nmode = \"jwt\"\nprivate_key = \"file-key\"\n",
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
        matches!(&err, Error::SecretInFile { key, .. } if key == "service_auth.private_key"),
        "{err}"
    );
}

#[test]
fn unauthenticated_grpc_outside_development_refuses_startup() {
    let listener = [
        ("APP__GRPC__ENABLED", "true"),
        ("APP__GRPC__ADDR", "127.0.0.1:0"),
        ("APP__GRPC__SECURITY", "plaintext"),
    ];
    let development = [("APP__APP__ENV", "development")];
    load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[listener.as_slice(), development.as_slice()].concat()),
    )
    .unwrap();
    let production = [("APP__APP__ENV", "production")];
    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[listener.as_slice(), production.as_slice()].concat()),
    )
    .unwrap_err();
    assert!(
        matches!(&err, Error::Validate(error) if error.key == "service_auth.mode"),
        "{err}"
    );
}
