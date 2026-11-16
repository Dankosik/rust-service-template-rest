use super::*;
use crate::LoadOptions;
use crate::load::{Error, load_from};
use crate::test_support::{BUILD, assert_secret_in_file, env};
use crate::validate::{ValidationErrors, admit_with};
use std::assert_matches;

fn jwt(extra: &str) -> ServiceAuthInput {
    toml::from_str(&format!(
        "mode = \"jwt\"\nname = \"catalog\"\njwt_type = \"example-svc+jwt\"\ntrust = \"{{}}\"\n{extra}"
    ))
    .unwrap()
}

fn admit(
    input: ServiceAuthInput,
    app_env: &str,
    grpc_enabled: bool,
) -> Result<ServiceAuthConfig, ValidationErrors> {
    admit_with(|v| input.admit(app_env, grpc_enabled, v))
}

fn rejected_keys(input: ServiceAuthInput, app_env: &str) -> Vec<String> {
    admit(input, app_env, true)
        .unwrap_err()
        .iter()
        .map(|error| error.key.clone())
        .collect()
}

#[test]
fn none_admits_unauthenticated_grpc_only_in_development() {
    for env in ["local", "development"] {
        assert_matches!(
            admit(ServiceAuthInput::default(), env, true),
            Ok(ServiceAuthConfig::None)
        );
    }
    for env in ["production", "staging", "Production", "dev"] {
        let errors = admit(ServiceAuthInput::default(), env, true).unwrap_err();
        assert!(errors.names("service_auth.mode"), "{env}");
    }
    assert_matches!(
        admit(ServiceAuthInput::default(), "production", false),
        Ok(ServiceAuthConfig::None)
    );
}

#[test]
fn jwt_needs_name_type_and_trust_and_a_key_with_its_id() {
    assert_matches!(
        admit(jwt(""), "production", true),
        Ok(ServiceAuthConfig::Jwt {
            signing_key: None,
            ..
        })
    );
    let Ok(ServiceAuthConfig::Jwt {
        signing_key: Some(signing_key),
        ..
    }) = admit(
        jwt("key_id = \"catalog/1\"\nprivate_key = \"pem\""),
        "production",
        true,
    )
    else {
        panic!("expected a signing key");
    };
    assert_eq!(signing_key.key_id, "catalog/1");
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
        let input: ServiceAuthInput = toml::from_str(&format!("mode = \"jwt\"\n{body}")).unwrap();
        assert_eq!(rejected_keys(input, "production"), [key]);
    }
    assert_eq!(
        rejected_keys(jwt("key_id = \"catalog/1\""), "local"),
        ["service_auth.private_key"]
    );
    assert_eq!(
        rejected_keys(jwt("private_key = \"pem\"\nkey_id = \" \""), "local"),
        ["service_auth.key_id"]
    );
}

#[test]
fn every_violated_jwt_rule_is_reported_together() {
    let input: ServiceAuthInput = toml::from_str("mode = \"jwt\"\nkey_id = \"catalog/1\"").unwrap();
    assert_eq!(
        rejected_keys(input, "production"),
        [
            "service_auth.name",
            "service_auth.jwt_type",
            "service_auth.trust",
            "service_auth.private_key"
        ]
    );
}

#[test]
fn none_accepts_no_dormant_input_and_debug_names_only_the_mode() {
    assert!(toml::from_str::<ServiceAuthInput>("mode = \"none\"\nname = \"x\"\n").is_err());
    let config = admit(
        jwt("key_id = \"catalog/1\"\nprivate_key = \"-----BEGIN PRIVATE KEY-----\""),
        "production",
        true,
    )
    .unwrap();
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
        signing_key,
    } = &cfg.service_auth
    else {
        panic!("expected jwt service authentication");
    };
    assert_eq!(name, "catalog");
    assert_eq!(jwt_type, "example-svc+jwt");
    assert_eq!(loaded, trust);
    let signing_key = signing_key.as_ref().unwrap();
    assert_eq!(signing_key.key_id, "catalog/production/2026-10");
    assert_eq!(signing_key.private_key.expose_secret(), "env-signing-key");
    assert!(!format!("{cfg:?}").contains("env-signing-key"));
}

#[test]
fn service_auth_signing_key_in_a_file_is_refused() {
    assert_secret_in_file(
        "[service_auth]\nmode = \"jwt\"\nprivate_key = \"file-key\"\n",
        "service_auth.private_key",
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
    assert_matches!(&err, Error::Validate(errors) if errors.names("service_auth.mode"), "{err}");
}
