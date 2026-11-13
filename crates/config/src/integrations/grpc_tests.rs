use crate::LoadOptions;
use crate::load::{Error, load_from};
use crate::test_support::{BUILD, env};
use std::assert_matches;

#[test]
fn grpc_integration_environment_builds_a_lazy_client_tuple() {
    let cfg = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            (
                "APP__INTEGRATIONS__BILLING__GRPC__DESTINATION",
                "https://billing.example:8443",
            ),
            ("APP__INTEGRATIONS__BILLING__GRPC__SECURITY", "tls"),
            (
                "APP__INTEGRATIONS__BILLING__GRPC__CA_CERTIFICATE",
                "public-ca",
            ),
            (
                "APP__INTEGRATIONS__BILLING__GRPC__CERTIFICATE",
                "public-client-cert",
            ),
            (
                "APP__INTEGRATIONS__BILLING__GRPC__PRIVATE_KEY",
                "private-client-key",
            ),
        ]),
    )
    .unwrap();
    let grpc = cfg
        .integrations
        .get("billing")
        .and_then(|integration| integration.grpc.as_ref())
        .expect("named gRPC tuple");
    assert_eq!(grpc.destination, "https://billing.example:8443");
    assert_eq!(grpc.security, crate::GrpcSecurity::Tls);
    assert!(!format!("{cfg:?}").contains("private-client-key"));
}

#[rstest::rstest]
#[case::missing_private_key(
    ("APP__INTEGRATIONS__BILLING__GRPC__CERTIFICATE", "public-client-cert"),
    "integrations.billing.grpc.private_key",
    Some("APP__INTEGRATIONS__BILLING__GRPC__PRIVATE_KEY"),
)]
#[case::missing_certificate(
    ("APP__INTEGRATIONS__BILLING__GRPC__PRIVATE_KEY", "private-client-key"),
    "integrations.billing.grpc.certificate",
    None,
)]
fn grpc_integration_names_the_missing_mtls_field(
    #[case] configured: (&str, &str),
    #[case] missing_key: &str,
    #[case] env_hint: Option<&str>,
) {
    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            (
                "APP__INTEGRATIONS__BILLING__GRPC__DESTINATION",
                "https://billing.example:8443",
            ),
            ("APP__INTEGRATIONS__BILLING__GRPC__SECURITY", "tls"),
            configured,
        ]),
    )
    .unwrap_err();
    assert_matches!(&err, Error::Validate(error) if error.key == missing_key, "{err}");
    if let Some(env_hint) = env_hint {
        assert!(err.to_string().contains(env_hint), "{err}");
    }
}
