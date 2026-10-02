//! Where the client's credentials come from: a configured access key pair,
//! or the AWS identity the platform gives the workload.

use aws_config::ecs::EcsCredentialsProvider;
use aws_config::imds::credentials::ImdsCredentialsProvider;
use aws_config::meta::credentials::CredentialsProviderChain;
use aws_config::provider_config::ProviderConfig;
use aws_config::web_identity_token::WebIdentityTokenCredentialsProvider;
use aws_sdk_s3::config::{Credentials, Region, SharedCredentialsProvider, SharedHttpClient};
use secrecy::{ExposeSecret, SecretString};

use crate::provider::{ConfigError, Provider};

/// How the client authenticates. [`Debug`] prints the variant only.
pub enum CredentialSource {
    /// A long-lived access key pair.
    AccessKey {
        /// Access key id.
        access_key_id: String,
        /// Secret access key.
        secret_access_key: SecretString,
    },
    /// The AWS identity the platform gives the workload, refreshed by the
    /// SDK before it expires: a web identity token (EKS IAM roles for service
    /// accounts), the container credentials endpoint (ECS task roles, EKS
    /// Pod Identity), then the EC2 instance profile. [`Provider::AmazonS3`]
    /// only.
    WorkloadIdentity,
}

impl std::fmt::Debug for CredentialSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::AccessKey { .. } => "AccessKey",
            Self::WorkloadIdentity => "WorkloadIdentity",
        })
    }
}

impl CredentialSource {
    /// The SDK credentials provider for this source. Nothing is sent: a
    /// workload identity loads on the first call.
    pub(crate) fn provider(
        &self,
        provider: &Provider,
        region: &str,
        http_client: &SharedHttpClient,
    ) -> Result<SharedCredentialsProvider, ConfigError> {
        match self {
            Self::AccessKey {
                access_key_id,
                secret_access_key,
            } => {
                if access_key_id.trim().is_empty() {
                    return Err(ConfigError::AccessKeyId);
                }
                if secret_access_key.expose_secret().trim().is_empty() {
                    return Err(ConfigError::SecretAccessKey);
                }
                Ok(SharedCredentialsProvider::new(Credentials::new(
                    access_key_id,
                    secret_access_key.expose_secret(),
                    None,
                    None,
                    "object_storage",
                )))
            }
            Self::WorkloadIdentity => {
                if !matches!(provider, Provider::AmazonS3 { .. }) {
                    return Err(ConfigError::Credentials);
                }
                Ok(SharedCredentialsProvider::new(workload_identity(
                    region,
                    http_client.clone(),
                )?))
            }
        }
    }
}

/// The instance metadata service at its IPv4 link-local address. Left to
/// resolve its endpoint, the SDK's client would read it from an AWS profile
/// file.
const INSTANCE_METADATA_ENDPOINT: &str = "http://169.254.169.254";

/// The workload's AWS identity, in the SDK default chain's order. Unlike that
/// chain it has no environment access keys and no profile files: the
/// providers read only what the platform injects for the identity
/// (`AWS_WEB_IDENTITY_TOKEN_FILE` and `AWS_ROLE_ARN`, the
/// `AWS_CONTAINER_*` variables) and the fixed instance metadata endpoint.
fn workload_identity(
    region: &str,
    http_client: SharedHttpClient,
) -> Result<CredentialsProviderChain, ConfigError> {
    let config = ProviderConfig::without_region()
        .with_region(Some(Region::new(region.to_owned())))
        .with_http_client(http_client);
    let instance_metadata = aws_config::imds::Client::builder()
        .configure(&config)
        .endpoint(INSTANCE_METADATA_ENDPOINT)
        .map_err(|_| ConfigError::Credentials)?
        .build();
    Ok(CredentialsProviderChain::first_try(
        "WebIdentityToken",
        WebIdentityTokenCredentialsProvider::builder()
            .configure(&config)
            .build(),
    )
    .or_else(
        "EcsContainer",
        EcsCredentialsProvider::builder().configure(&config).build(),
    )
    .or_else(
        "Ec2InstanceMetadata",
        ImdsCredentialsProvider::builder()
            .configure(&config)
            .imds_client(instance_metadata)
            .build(),
    ))
}
