/// Closed transport failures. Each variant names the rejected input, never its
/// value: no request metadata, bearer material, peer path, certificate, or key.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("gRPC client destination is not a valid URI")]
    InvalidDestination,
    #[error("gRPC client destination must use https with TLS and http with plaintext")]
    DestinationSecurityMismatch,
    #[error("gRPC TLS certificate chain is invalid")]
    InvalidCertificate,
    #[error("gRPC TLS private key is invalid or does not match the certificate")]
    InvalidPrivateKey,
    #[error("gRPC TLS CA certificate is invalid")]
    InvalidCaCertificate,
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
    #[error("gRPC scope requirement must name a described method of a registered service")]
    UnregisteredMethodPath,
    #[error("gRPC scope requirement is declared twice for one method")]
    DuplicateScopeRequirement,
}
