use infra_cache::{Cache, CacheOptions, ClientCertificate};
use service_config::Config;

use super::BootstrapError;

pub(super) async fn open(
    config: &Config,
    retained: &mut Option<Cache>,
) -> Result<(), BootstrapError> {
    let Some(dsn) = &config.cache.dsn else {
        return Ok(());
    };
    let client_certificate =
        config
            .cache
            .client_certificate
            .clone()
            .map(|certificate| ClientCertificate {
                cert_path: certificate.cert_path,
                key_path: certificate.key_path,
            });
    let options = CacheOptions {
        dsn: dsn.clone(),
        password_file: config.cache.password_file.clone(),
        root_ca_path: config.cache.root_ca_path.clone(),
        client_certificate,
        allow_plaintext: config.cache.allow_plaintext,
        allow_unauthenticated: config.cache.allow_unauthenticated,
        command_timeout: config.cache.command_timeout,
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
                cache
                    .get(request.operation(), "key")
                    .await
                    .map_err(|_| axum::http::StatusCode::SERVICE_UNAVAILABLE)?
                    .ok_or(axum::http::StatusCode::NOT_FOUND)
            },
        );
    }
}
