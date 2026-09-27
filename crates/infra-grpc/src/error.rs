/// Closed transport failures.  This type never retains request metadata,
/// bearer material, a peer path, a certificate, or a dependency error.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum Error {
    #[error("gRPC configuration is invalid")]
    InvalidConfiguration,
    #[error("gRPC registration is invalid")]
    InvalidRegistration,
    #[error("gRPC transport is already stopping")]
    Stopping,
    #[error("gRPC request metadata is too large")]
    MetadataTooLarge,
    #[error("gRPC request deadline has expired")]
    DeadlineExceeded,
    #[error("gRPC transport is at capacity")]
    AtCapacity,
    #[error("gRPC transport I/O failed")]
    Transport,
    #[error("gRPC transport drain timed out")]
    DrainTimedOut,
    #[error("gRPC authentication failed")]
    Authentication,
    #[error("gRPC authentication is unavailable")]
    AuthenticationUnavailable,
    #[error("gRPC deadline is incompatible with drain budget")]
    IncompatibleDrainBudget,
    #[error("gRPC timeout is invalid")]
    InvalidTimeout,
    #[error("gRPC validation program is invalid")]
    InvalidValidation,
    #[error("gRPC retry delay is too large")]
    InvalidRetryDelay,
    #[error("gRPC operation exceeds the parent deadline")]
    ParentDeadlineExceeded,
    #[error("gRPC operation duration is invalid")]
    InvalidDuration,
}

impl Error {
    pub(crate) fn status(self) -> tonic::Status {
        match self {
            Self::MetadataTooLarge => {
                tonic::Status::resource_exhausted("request metadata is too large")
            }
            Self::DeadlineExceeded | Self::ParentDeadlineExceeded => {
                tonic::Status::deadline_exceeded("request deadline exceeded")
            }
            Self::AtCapacity => {
                tonic::Status::resource_exhausted(service_failure::AT_CAPACITY_DETAIL)
            }
            Self::Stopping | Self::DrainTimedOut => {
                tonic::Status::unavailable("service is draining")
            }
            Self::Authentication => tonic::Status::unauthenticated("authentication failed"),
            Self::AuthenticationUnavailable => {
                tonic::Status::unavailable("authentication is unavailable")
            }
            Self::InvalidConfiguration
            | Self::InvalidRegistration
            | Self::IncompatibleDrainBudget
            | Self::InvalidTimeout
            | Self::InvalidValidation
            | Self::InvalidRetryDelay
            | Self::InvalidDuration => tonic::Status::internal("request failed"),
            Self::Transport => tonic::Status::unavailable("transport unavailable"),
        }
    }
}
