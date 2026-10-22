/// Closed transport failures. Each variant names the rejected input, never its
/// value: no request metadata, bearer material, peer path, certificate, or key.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    /// The client destination is not a valid URI.
    #[error("gRPC client destination is not a valid URI")]
    InvalidDestination,
    /// The destination scheme does not match the selected security (`https` with TLS, `http` with plaintext).
    #[error("gRPC client destination must use https with TLS and http with plaintext")]
    DestinationSecurityMismatch,
    /// The TLS certificate chain is invalid.
    #[error("gRPC TLS certificate chain is invalid")]
    InvalidCertificate,
    /// The TLS private key is invalid or does not match the certificate.
    #[error("gRPC TLS private key is invalid or does not match the certificate")]
    InvalidPrivateKey,
    /// The TLS CA certificate is invalid.
    #[error("gRPC TLS CA certificate is invalid")]
    InvalidCaCertificate,
    /// Client TLS setup failed (native roots, host name, or certificate and key pairing).
    #[error(
        "gRPC client TLS setup failed: native roots, destination host name, or certificate and key pairing"
    )]
    InvalidClientTls,
    /// A file descriptor set could not be decoded.
    #[error("gRPC file descriptor set is invalid")]
    InvalidFileDescriptorSet,
    /// A service was registered more than once.
    #[error("gRPC service {0} is registered twice")]
    DuplicateService(&'static str),
    /// A registered service appears in no described file descriptor set.
    #[error("gRPC service {0} is in no described file descriptor set")]
    UndescribedService(&'static str),
    /// A scope requirement names a method that is not described or not of a registered service.
    #[error("gRPC scope requirement must name a described method of a registered service")]
    UnregisteredMethodPath,
    /// A scope requirement was declared twice for one method.
    #[error("gRPC scope requirement is declared twice for one method")]
    DuplicateScopeRequirement,
    // template:begin authn:grpc-unauthenticated-business-calls-error
    /// Business calls have neither a bearer verifier nor service tokens replacing it.
    #[error("gRPC business calls need a bearer verifier unless service tokens replace it")]
    UnauthenticatedBusinessCalls,
    // template:end authn:grpc-unauthenticated-business-calls-error
    // template:begin service-auth:grpc-caller-rule-errors
    /// A caller rule names a method that is not described or not of a registered service.
    #[error("gRPC caller rule must name a described method of a registered service")]
    UnregisteredCallerRule,
    /// A caller rule was declared twice for one method.
    #[error("gRPC caller rule is declared twice for one method")]
    DuplicateCallerRule,
    /// A caller rule names a caller outside the name grammar, or the same caller twice.
    #[error("gRPC caller rule names a caller outside the name grammar or twice")]
    InvalidCallerRule,
    /// Names the method: a registered service's own contract, not request data.
    #[error("gRPC method {0} has no caller rule")]
    MissingCallerRule(String),
    // template:end service-auth:grpc-caller-rule-errors
}
