#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TlsMaterialError {
    #[error("gRPC TLS certificate chain is invalid")]
    InvalidCertificate,
    #[error("gRPC TLS private key is invalid or does not match the certificate")]
    InvalidPrivateKey,
    #[error("gRPC TLS CA certificate is invalid")]
    InvalidCaCertificate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ClientError {
    #[error("gRPC client destination is not a valid URI")]
    InvalidDestination,
    #[error("gRPC client destination must use https with TLS and http with plaintext")]
    DestinationSecurityMismatch,
    // template:begin service-auth:grpc-service-token-client-errors
    #[error("gRPC service tokens require a TLS destination")]
    ServiceTokensRequireTls,
    #[error("gRPC destination differs from the configured service token receiver origin")]
    ServiceTokenOriginMismatch,
    // template:end service-auth:grpc-service-token-client-errors
    #[error(transparent)]
    Tls(#[from] TlsMaterialError),
    #[error(
        "gRPC client TLS setup failed: native roots, destination host name, or certificate and key pairing"
    )]
    InvalidClientTls,
}
