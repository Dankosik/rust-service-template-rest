//! Readiness participation.
//!
//! The probe shares the pool on purpose: readiness is refreshed by the
//! background refresher, never per request, so a saturated pool fails the
//! probe there and the instance stops receiving traffic instead of queueing
//! it. The refresher's budget bounds the whole check; the acquire budget
//! bounds the wait for a connection inside it.

use std::future::Future;
use std::pin::Pin;

use health::{Probe, ProbeError};
use sqlx::Connection;
use sqlx::postgres::PgPool;

/// `health::Probe` over a pooled ping.
#[derive(Clone, Debug)]
pub struct PostgresProbe {
    pool: PgPool,
}

impl PostgresProbe {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

impl Probe for PostgresProbe {
    fn name(&self) -> &'static str {
        "postgres"
    }

    fn check(&self) -> Pin<Box<dyn Future<Output = Result<(), ProbeError>> + Send + '_>> {
        Box::pin(async move {
            let mut conn = self.pool.acquire().await.map_err(|err| probe_error(&err))?;
            conn.ping().await.map_err(|err| probe_error(&err))
        })
    }
}

/// The verdict message names the failure class without dependency
/// internals; the driver's own text can quote server details.
fn probe_error(err: &sqlx::Error) -> ProbeError {
    ProbeError::new(match err {
        sqlx::Error::PoolTimedOut => "no connection available inside the acquire budget",
        sqlx::Error::PoolClosed => "pool is closed",
        sqlx::Error::Io(_) => "connection failed",
        sqlx::Error::Tls(_) => "tls handshake failed",
        sqlx::Error::Database(_) => "server rejected the ping",
        _ => "ping failed",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_stay_generic() {
        assert_eq!(
            probe_error(&sqlx::Error::PoolTimedOut).0,
            "no connection available inside the acquire budget"
        );
        assert_eq!(
            probe_error(&sqlx::Error::Io(std::io::Error::other(
                "host db.internal refused"
            )))
            .0,
            "connection failed"
        );
    }
}
