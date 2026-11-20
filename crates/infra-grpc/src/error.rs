use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyKind {
    ScopeRequirement,
    CallerRule,
}

impl fmt::Display for PolicyKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ScopeRequirement => formatter.write_str("scope requirement"),
            Self::CallerRule => formatter.write_str("caller rule"),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("decode gRPC file descriptor set")]
    DecodeDescriptorSet(#[source] prost::DecodeError),
    #[error("inspect gRPC reflection file descriptor")]
    InspectReflectionFile(#[source] prost::DecodeError),
    #[error("inspect gRPC reflection file descriptor: missing file name")]
    MissingReflectionFileName,
    #[error("build gRPC v1 reflection service")]
    BuildReflectionV1(#[source] tonic_reflection::server::Error),
    #[error("build gRPC v1alpha reflection service")]
    BuildReflectionV1Alpha(#[source] tonic_reflection::server::Error),
    #[error("gRPC descriptors disagree on the methods of service {0}")]
    ConflictingServiceDescription(String),
    #[error("gRPC reflection descriptors disagree on the contents of file {0}")]
    ConflictingFileDescription(String),
    #[error("gRPC service {0} is registered twice")]
    DuplicateService(&'static str),
    #[error("gRPC service {0} is in no described file descriptor set")]
    UndescribedService(&'static str),
    #[error("gRPC {policy} for {path} must name a described method of a registered service")]
    UnregisteredMethod { policy: PolicyKind, path: String },
    #[error("gRPC {policy} is declared twice for {path}")]
    DuplicatePolicy { policy: PolicyKind, path: String },
    #[error("gRPC method {path} has no {policy}")]
    MissingPolicy { policy: PolicyKind, path: String },
    // template:begin authn:grpc-unauthenticated-business-calls-error
    #[error("gRPC scope requirement for {path} has an invalid scope token at index {index}")]
    InvalidScopeToken { path: String, index: usize },
    #[error("gRPC business calls require {}", crate::authn::VERIFIER_SETTINGS.join(" or "))]
    UnauthenticatedBusinessCalls,
    #[error(
        "gRPC required scopes or bearer delegation cannot be enforced when service tokens replace bearer authentication"
    )]
    ScopesWithServiceTokens,
    // template:end authn:grpc-unauthenticated-business-calls-error
    // template:begin service-auth:grpc-caller-rule-errors
    #[error("gRPC caller rules require service token authentication")]
    CallerRulesRequireServiceTokens,
    #[error("gRPC caller rule for {path} is invalid")]
    InvalidCallerRule {
        path: String,
        source: infra_serviceauth::InvalidServiceName,
    },
    #[error("gRPC caller rule for {0} names the same caller twice")]
    DuplicateCallerName(String),
    // template:end service-auth:grpc-caller-rule-errors
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("gRPC service token authentication requires TLS")]
pub struct TlsRequiredError;

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TlsMaterialError {
    #[error("gRPC TLS certificate chain is invalid")]
    InvalidCertificate,
    #[error("gRPC TLS private key is invalid or does not match the certificate")]
    InvalidPrivateKey,
    #[error("gRPC TLS CA certificate is invalid")]
    InvalidCaCertificate,
}
