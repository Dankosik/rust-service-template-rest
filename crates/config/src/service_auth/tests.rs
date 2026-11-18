use super::*;
use crate::LoadOptions;
use crate::load::{Error, load_from};
use crate::test_support::{BUILD, assert_secret_in_file, env, load_env};
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
    insecure_development: bool,
    grpc_enabled: bool,
) -> Result<ServiceAuthConfig, ValidationErrors> {
    admit_with(|v| input.admit(insecure_development, grpc_enabled, v))
}

fn rejected_keys(input: ServiceAuthInput, insecure_development: bool) -> Vec<String> {
    admit(input, insecure_development, true)
        .unwrap_err()
        .iter()
        .map(|error| error.key.clone())
        .collect()
}

#[test]
fn none_admits_unauthenticated_grpc_only_in_development() {
    assert_matches!(
        admit(ServiceAuthInput::default(), true, true),
        Ok(ServiceAuthConfig::None)
    );
    let errors = admit(ServiceAuthInput::default(), false, true).unwrap_err();
    assert!(errors.names("service_auth.mode"));
    assert_matches!(
        admit(ServiceAuthInput::default(), false, false),
        Ok(ServiceAuthConfig::None)
    );
}

#[test]
fn jwt_needs_name_type_and_trust_and_a_key_with_its_id() {
    assert_matches!(
        admit(jwt(""), false, true),
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
        false,
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
        assert_eq!(rejected_keys(input, false), [key]);
    }
    assert_eq!(
        rejected_keys(jwt("key_id = \"catalog/1\""), true),
        ["service_auth.private_key"]
    );
    assert_eq!(
        rejected_keys(jwt("private_key = \"pem\"\nkey_id = \" \""), true),
        ["service_auth.key_id"]
    );
}

#[test]
fn every_violated_jwt_rule_is_reported_together() {
    let input: ServiceAuthInput = toml::from_str("mode = \"jwt\"\nkey_id = \"catalog/1\"").unwrap();
    assert_eq!(
        rejected_keys(input, false),
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
    let dormant: ServiceAuthInput = toml::from_str("mode = \"none\"\nname = \"x\"\n").unwrap();
    assert_eq!(rejected_keys(dormant, true), ["service_auth.name"]);
    let all_dormant: ServiceAuthInput = toml::from_str(
        "mode = \"none\"\nname = \"x\"\njwt_type = \"t\"\ntrust = \"{}\"\nkey_id = \"k\"\nprivate_key = \"p\"\n",
    )
    .unwrap();
    assert_eq!(
        rejected_keys(all_dormant, true),
        [
            "service_auth.name",
            "service_auth.jwt_type",
            "service_auth.trust",
            "service_auth.key_id",
            "service_auth.private_key",
        ]
    );
    let config = admit(
        jwt("key_id = \"catalog/1\"\nprivate_key = \"-----BEGIN PRIVATE KEY-----\""),
        false,
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
    let cfg = load_env(&[
        ("APP__APP__ENV", "production"),
        ("APP__GRPC__ENABLED", "true"),
        ("APP__GRPC__ADDR", "127.0.0.1:0"),
        ("APP__GRPC__SECURITY", "tls"),
        ("APP__GRPC__CERTIFICATE", "server certificate"),
        ("APP__GRPC__PRIVATE_KEY", "server private key"),
        ("APP__SERVICE_AUTH__MODE", "jwt"),
        ("APP__SERVICE_AUTH__NAME", "catalog"),
        ("APP__SERVICE_AUTH__JWT_TYPE", "example-svc+jwt"),
        ("APP__SERVICE_AUTH__TRUST", trust),
        ("APP__SERVICE_AUTH__KEY_ID", "catalog/production/2026-10"),
        ("APP__SERVICE_AUTH__PRIVATE_KEY", "env-signing-key"),
    ]);
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
    let development = [
        ("APP__APP__ENV", "development"),
        ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
    ];
    let jwt = [
        ("APP__SERVICE_AUTH__MODE", "jwt"),
        ("APP__SERVICE_AUTH__NAME", "catalog"),
        ("APP__SERVICE_AUTH__JWT_TYPE", "example-svc+jwt"),
        ("APP__SERVICE_AUTH__TRUST", "{}"),
    ];
    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[listener.as_slice(), development.as_slice(), jwt.as_slice()].concat()),
    )
    .unwrap_err();
    assert_matches!(&err, Error::Validate(errors) if errors.names("grpc.security"), "{err}");
    load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[listener.as_slice(), development.as_slice()].concat()),
    )
    .unwrap();
    for environment in [
        None,
        Some(("APP__APP__ENV", "development")),
        Some(("APP__APP__ENV", "private-environment-value")),
    ] {
        let err = load_from(
            &LoadOptions::default(),
            BUILD,
            env(&[listener.as_slice(), environment.as_slice()].concat()),
        )
        .unwrap_err();
        assert_matches!(&err, Error::Validate(errors) if errors.names("service_auth.mode"), "{err}");
        assert!(!err.to_string().contains("private-environment-value"));
    }
}
