/// Closed transport failures. A message may name the registration contract,
/// a service or a method path the service itself declared, but never request
/// data, bearer material, a peer path, a certificate, or a key.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("gRPC client destination is not a valid URI")]
    InvalidDestination,
    #[error("gRPC client destination must use https with TLS and http with plaintext")]
    DestinationSecurityMismatch,
    #[error(transparent)]
    Tls(#[from] TlsMaterialError),
    #[error(
        "gRPC client TLS setup failed: native roots, destination host name, or certificate and key pairing"
    )]
    InvalidClientTls,
    #[error("gRPC file descriptor set is invalid")]
    InvalidFileDescriptorSet,
    #[error("gRPC service {0} is registered twice")]
    DuplicateService(&'static str),
    #[error("gRPC service {0} is in no described file descriptor set")]
    UndescribedService(&'static str),
    #[error("gRPC scope requirement for {0} must name a described method of a registered service")]
    UnregisteredMethodPath(String),
    #[error("gRPC scope requirement is declared twice for {0}")]
    DuplicateScopeRequirement(String),
    // template:begin authn:grpc-unauthenticated-business-calls-error
    #[error("gRPC business calls need a bearer verifier unless service tokens replace it")]
    UnauthenticatedBusinessCalls,
    #[error(
        "gRPC scope requirements cannot be enforced when service tokens replace bearer authentication"
    )]
    ScopesWithServiceTokens,
    // template:end authn:grpc-unauthenticated-business-calls-error
    // template:begin service-auth:grpc-caller-rule-errors
    #[error("gRPC caller rule for {0} must name a described method of a registered service")]
    UnregisteredCallerRule(String),
    #[error("gRPC caller rule is declared twice for {0}")]
    DuplicateCallerRule(String),
    #[error("gRPC caller rule for {0} names a caller outside the name grammar or twice")]
    InvalidCallerRule(String),
    #[error("gRPC method {0} has no caller rule")]
    MissingCallerRule(String),
    // template:end service-auth:grpc-caller-rule-errors
}

/// TLS certificate material that cannot be used, the only failure of
/// [`server_tls_config`](crate::server_tls_config).
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TlsMaterialError {
    #[error("gRPC TLS certificate chain is invalid")]
    InvalidCertificate,
    #[error("gRPC TLS private key is invalid or does not match the certificate")]
    InvalidPrivateKey,
    #[error("gRPC TLS CA certificate is invalid")]
    InvalidCaCertificate,
}
