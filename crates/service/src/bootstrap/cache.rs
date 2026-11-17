use infra_cache::{Cache, CacheOptions, ClientCertificate};
use service_config::Config;

use super::BootstrapError;

pub(super) async fn open(
    config: &Config,
    retained: &mut Option<Cache>,
) -> Result<(), BootstrapError> {
    let Some(cache) = &config.cache else {
        return Ok(());
    };
    let options = CacheOptions {
        dsn: cache.dsn.clone(),
        password_file: cache.password_file.clone(),
        root_ca_path: cache.root_ca_path.clone(),
        client_certificate: cache.client_certificate.as_ref().map(|certificate| {
            ClientCertificate {
                cert_path: certificate.cert_path.clone(),
                key_path: certificate.key_path.clone(),
            }
        }),
        allow_plaintext: cache.allow_plaintext,
        allow_unauthenticated: cache.allow_unauthenticated,
        command_timeout: cache.command_timeout,
        max_key_bytes: usize::try_from(cache.max_key_bytes.as_u64()).unwrap_or(usize::MAX),
        max_value_bytes: usize::try_from(cache.max_value_bytes.as_u64()).unwrap_or(usize::MAX),
    };
    *retained = Some(Cache::start(options).await?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::extract::State;
    use infra_cache::CacheNamespace;
    use infra_http::RequestContext;

    #[test]
    fn a_feature_handler_can_forward_its_request_context_to_the_cache() {
        let _: axum::routing::MethodRouter<CacheNamespace> = axum::routing::get(
            async |State(cache): State<CacheNamespace>, request: RequestContext| {
                let _lookup = cache.get(request.operation(), "key").await;
            },
        );
    }
}
