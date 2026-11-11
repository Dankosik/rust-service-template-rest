use crate::LoadOptions;
use crate::load::{Error, load_from};
use crate::test_support::{BUILD, env};

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

#[test]
fn grpc_integration_refuses_unpaired_mtls_material() {
    let err = load_from(
        &LoadOptions::default(),
        BUILD,
        env(&[
            (
                "APP__INTEGRATIONS__BILLING__GRPC__DESTINATION",
                "https://billing.example:8443",
            ),
            ("APP__INTEGRATIONS__BILLING__GRPC__SECURITY", "tls"),
            (
                "APP__INTEGRATIONS__BILLING__GRPC__CERTIFICATE",
                "public-client-cert",
            ),
        ]),
    )
    .unwrap_err();
    assert!(
        matches!(&err, Error::Validate(error) if error.key == "integrations.billing.grpc.certificate/integrations.billing.grpc.private_key"),
        "{err}"
    );
}
