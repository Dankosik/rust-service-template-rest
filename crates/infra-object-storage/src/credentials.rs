use std::ffi::OsStr;

use aws_config::ecs::EcsCredentialsProvider;
use aws_config::imds::credentials::ImdsCredentialsProvider;
use aws_config::meta::credentials::CredentialsProviderChain;
use aws_config::provider_config::ProviderConfig;
use aws_config::web_identity_token::WebIdentityTokenCredentialsProvider;
use aws_sdk_s3::config::{Credentials, Region, SharedCredentialsProvider, SharedHttpClient};
use secrecy::{ExposeSecret, SecretString};

use crate::error::ConfigError;
use crate::provider::{ResolvedProvider, Target};

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
        provider: &ResolvedProvider,
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
                if !matches!(provider.target, Target::AmazonS3 { .. }) {
                    return Err(ConfigError::Credentials);
                }
                Ok(SharedCredentialsProvider::new(workload_identity(
                    &provider.region,
                    http_client.clone(),
                )?))
            }
        }
    }
}

const INSTANCE_METADATA_ENDPOINT: &str = "http://169.254.169.254";

fn web_identity_configured(
    role_arn: Option<&OsStr>,
    token_file: Option<&OsStr>,
) -> Result<bool, ConfigError> {
    match (role_arn, token_file) {
        (None, None) => Ok(false),
        (Some(role), Some(file)) if [role, file].into_iter().all(nonblank_unicode) => Ok(true),
        _ => Err(ConfigError::WebIdentity),
    }
}

fn nonblank_unicode(value: &OsStr) -> bool {
    value.to_str().is_some_and(|text| !text.trim().is_empty())
}

fn workload_identity(
    region: &str,
    http_client: SharedHttpClient,
) -> Result<CredentialsProviderChain, ConfigError> {
    let config = ProviderConfig::without_region()
        .with_region(Some(Region::new(region.to_owned())))
        .with_http_client(http_client);
    if web_identity_configured(
        std::env::var_os("AWS_ROLE_ARN").as_deref(),
        std::env::var_os("AWS_WEB_IDENTITY_TOKEN_FILE").as_deref(),
    )? {
        return Ok(CredentialsProviderChain::first_try(
            "WebIdentityToken",
            WebIdentityTokenCredentialsProvider::builder()
                .configure(&config)
                .build(),
        ));
    }
    let instance_metadata = aws_config::imds::Client::builder()
        .configure(&config)
        .endpoint(INSTANCE_METADATA_ENDPOINT)
        .map_err(|_| ConfigError::Credentials)?
        .build();
    Ok(CredentialsProviderChain::first_try(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_web_identity_cannot_fall_through_to_node_credentials() {
        let role = Some(OsStr::new("arn:aws:iam::123456789012:role/service"));
        let file = Some(OsStr::new("/run/secrets/token"));
        assert_eq!(web_identity_configured(None, None), Ok(false));
        assert_eq!(web_identity_configured(role, file), Ok(true));
        for (role, file) in [
            (role, None),
            (None, file),
            (Some(OsStr::new("")), file),
            (role, Some(OsStr::new(" "))),
        ] {
            assert_eq!(
                web_identity_configured(role, file),
                Err(ConfigError::WebIdentity)
            );
        }
    }
}
