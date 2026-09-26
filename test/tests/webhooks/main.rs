//! Real-PostgreSQL proof for retained durable webhook capabilities.

#![cfg(feature = "integration")]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

// template:begin inbound-webhooks:test-webhooks-inbound-modules
#[allow(
    dead_code,
    reason = "the shared transport also supplies pending-BEGIN cancellation proof to the postgres integration target"
)]
#[path = "../support/commit_proxy.rs"]
mod commit_proxy;
mod inbound;
// template:end inbound-webhooks:test-webhooks-inbound-modules
// template:begin webhooks:test-webhooks-outbound-module
mod outbound;
// template:end webhooks:test-webhooks-outbound-module

// template:begin inbound-webhooks:test-webhooks-inbound-std
use std::future::Future;
use std::num::NonZeroU32;
// template:end inbound-webhooks:test-webhooks-inbound-std
use std::time::Duration;

use infra_postgres::{Closed, PgPool};
// template:begin inbound-webhooks:test-webhooks-inbound-postgres
use infra_postgres::{Dsn, Isolation, PoolOptions};
// template:end inbound-webhooks:test-webhooks-inbound-postgres

// template:begin inbound-webhooks:test-webhooks-inbound-constants
const APP: &str = "integration-tests-webhooks";
pub(crate) const WAIT: Duration = Duration::from_secs(10);
// template:end inbound-webhooks:test-webhooks-inbound-constants
const CLOSE_BUDGET: Duration = Duration::from_secs(5);

// template:begin inbound-webhooks:test-webhooks-inbound-pool
pub(crate) async fn template_pool(dsn: &Dsn, max_connections: u32) -> PgPool {
    infra_postgres::connect(
        dsn,
        &PoolOptions {
            max_connections: NonZeroU32::new(max_connections).expect("a pool size"),
            application_name: APP,
            default_isolation: Isolation::ServerDefault,
        },
    )
    .await
    .expect("the template pool connects")
}
// template:end inbound-webhooks:test-webhooks-inbound-pool

pub(crate) async fn close(pools: &[&PgPool]) {
    for pool in pools {
        assert_eq!(
            infra_postgres::close(pool, CLOSE_BUDGET).await,
            Closed::Complete
        );
    }
}

// template:begin inbound-webhooks:test-webhooks-inbound-bounded
pub(crate) async fn bounded<F: Future>(what: &str, future: F) -> F::Output {
    tokio::time::timeout(WAIT, future).await.expect(what)
}
// template:end inbound-webhooks:test-webhooks-inbound-bounded
