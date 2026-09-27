/// Closed transport failures. This type never retains request metadata,
/// bearer material, a peer path, a certificate, or a dependency error.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("gRPC configuration is invalid")]
    InvalidConfiguration,
    #[error("gRPC registration is invalid")]
    InvalidRegistration,
}
