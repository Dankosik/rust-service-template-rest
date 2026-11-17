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
        max_concurrency: cache.max_concurrency,
        max_key_bytes: usize::try_from(cache.max_key_bytes.as_u64()).unwrap_or(usize::MAX),
        max_value_bytes: usize::try_from(cache.max_value_bytes.as_u64()).unwrap_or(usize::MAX),
    };
    *retained = Some(Cache::start(options).await?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::extract::State;
    use infra_cache::{CacheLookup, namespace};
    use infra_http::RequestContext;

    #[test]
    fn a_cache_aside_handler_can_use_the_retained_startup_cache_and_request_context() {
        let dependencies = super::super::shutdown::Dependencies::default();
        let namespace = dependencies
            .cache
            .as_ref()
            .map(|cache| cache.namespace(namespace!("items")));
        let cache = CacheLookup::new(namespace, Duration::from_millis(100));
        let _: axum::Router = axum::Router::new()
            .route(
                "/items",
                axum::routing::get(
                    async |State(cache): State<CacheLookup>, request: RequestContext| {
                        let _lookup = cache.lookup(request.operation(), "key").await;
                    },
                ),
            )
            .with_state(cache);
    }
}
