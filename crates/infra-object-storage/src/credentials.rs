use aws_config::ecs::EcsCredentialsProvider;
use aws_config::imds::credentials::ImdsCredentialsProvider;
use aws_config::meta::credentials::CredentialsProviderChain;
use aws_config::provider_config::ProviderConfig;
use aws_config::web_identity_token::WebIdentityTokenCredentialsProvider;
use aws_sdk_s3::config::{Credentials, Region, SharedCredentialsProvider, SharedHttpClient};
use secrecy::{ExposeSecret, SecretString};

use crate::error::ConfigError;
use crate::provider::Admitted;

#[derive(derive_more::Debug)]
pub enum CredentialSource {
    #[debug("AccessKey")]
    AccessKey {
        access_key_id: String,
        secret_access_key: SecretString,
    },
    WorkloadIdentity,
}

impl CredentialSource {
    pub(crate) fn credentials_provider(
        &self,
        admitted: &Admitted,
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
                if !admitted.workload_identity {
                    return Err(ConfigError::Credentials);
                }
                Ok(SharedCredentialsProvider::new(workload_identity(
                    &admitted.region,
                    http_client.clone(),
                )?))
            }
        }
    }
}

const INSTANCE_METADATA_ENDPOINT: &str = "http://169.254.169.254";

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
