//! Optional cache connection.

use std::time::Duration;

use health::Probe;
use infra_cache::{Cache, CacheOptions};
use service_config::Config;

use super::BootstrapError;

const STARTUP_CHECK: Duration = Duration::from_secs(1);

/// Connect the optional cache. An outage at startup is logged, not fatal:
/// the cache is not a readiness probe, and callers fall back to the source
/// of truth.
pub(super) async fn open(config: &Config) -> Result<Option<Cache>, BootstrapError> {
    let Some(dsn) = &config.cache.dsn else {
        return Ok(None);
    };
    let cache = Cache::connect(CacheOptions {
        dsn: dsn.clone(),
        root_ca_path: config.cache.root_ca_path.clone(),
        allow_plaintext: config.cache.allow_plaintext,
        allow_unauthenticated: config.cache.allow_unauthenticated,
        command_timeout: config.cache.command_timeout,
    })?;
    let server = cache.server();
    match tokio::time::timeout(STARTUP_CHECK, cache.probe().check()).await {
        Ok(Ok(())) => {
            tracing::info!(
                server.address = %server.host,
                server.port = server.port,
                cache.tls = server.tls,
                "cache_connected"
            );
        }
        Ok(Err(error)) => {
            tracing::warn!(
                server.address = %server.host,
                server.port = server.port,
                cache.tls = server.tls,
                reason = %error,
                "cache_unavailable_at_startup"
            );
        }
        Err(_) => {
            tracing::warn!(
                server.address = %server.host,
                server.port = server.port,
                cache.tls = server.tls,
                reason = "startup check timed out",
                "cache_unavailable_at_startup"
            );
        }
    }
    Ok(Some(cache))
}
