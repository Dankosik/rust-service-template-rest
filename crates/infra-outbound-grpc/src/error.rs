#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum TlsMaterialError {
    #[error("gRPC TLS certificate chain is invalid")]
    InvalidCertificate(#[source] Option<rustls::Error>),
    #[error("gRPC TLS private key is invalid")]
    InvalidPrivateKey,
    #[error("gRPC TLS CA certificate is invalid")]
    InvalidCaCertificate(#[source] Option<rustls::Error>),
}

#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum ClientError {
    #[error("gRPC client destination is not a valid URI")]
    InvalidDestination,
    #[error("gRPC client destination must use https with TLS and http with plaintext")]
    DestinationSecurityMismatch,
    #[error("gRPC client opening limit exceeds the supported maximum")]
    InvalidOpeningLimit,
    // template:begin service-auth:grpc-service-token-client-errors
    #[error("gRPC service tokens require a TLS destination")]
    ServiceTokensRequireTls,
    #[error("gRPC destination differs from the configured service token receiver origin")]
    ServiceTokenOriginMismatch,
    // template:end service-auth:grpc-service-token-client-errors
    #[error(transparent)]
    Tls(#[from] TlsMaterialError),
    #[error("gRPC client TLS private key does not match the certificate")]
    ClientIdentityMismatch(#[source] rustls::Error),
    #[error("gRPC client TLS identity installation failed")]
    ClientIdentitySetup(#[source] rustls::Error),
    #[error("gRPC client TLS platform verifier setup failed")]
    ClientVerifierSetup,
    #[error("gRPC client TLS protocol version selection failed")]
    ClientProtocolSetup(#[source] rustls::Error),
}
