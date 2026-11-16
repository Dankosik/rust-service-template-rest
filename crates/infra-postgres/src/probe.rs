//! The probe shares the pool so readiness can reflect saturation in the
//! pool that serves requests. The background refresher applies its failure
//! threshold and bounds the whole check. The acquire budget bounds the wait
//! for a connection inside it.

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

#[async_trait::async_trait]
impl Probe for PostgresProbe {
    fn name(&self) -> &'static str {
        "postgres"
    }

    async fn check(&self) -> Result<(), ProbeError> {
        let mut conn = crate::acquire(&self.pool, "readiness")
            .await
            .map_err(|err| probe_error(&err))?;
        conn.ping().await.map_err(|err| probe_error(&err))
    }
}

/// The verdict message names the failure class without dependency
/// internals; the driver's own text can quote server details. A server
/// rejection carries its SQLSTATE, a bounded code that tells an
/// authentication failure (`28P01`) from other rejections.
fn probe_error(err: &sqlx::Error) -> ProbeError {
    match err {
        sqlx::Error::PoolTimedOut => {
            ProbeError::from_static("no connection available inside the acquire budget")
        }
        sqlx::Error::PoolClosed => ProbeError::from_static("pool is closed"),
        sqlx::Error::Io(_) => ProbeError::from_static("connection failed"),
        sqlx::Error::Tls(_) => ProbeError::from_static("tls handshake failed"),
        sqlx::Error::Database(_) => match crate::error::sqlstate(err) {
            Some(code) => ProbeError::format(format_args!(
                "server rejected the readiness check (SQLSTATE {code})"
            )),
            None => ProbeError::from_static("server rejected the readiness check"),
        },
        _ => ProbeError::from_static("readiness check failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_stay_generic() {
        assert_eq!(
            probe_error(&sqlx::Error::PoolTimedOut).to_string(),
            "no connection available inside the acquire budget"
        );
        assert_eq!(
            probe_error(&sqlx::Error::Io(std::io::Error::other(
                "host db.internal refused"
            )))
            .to_string(),
            "connection failed"
        );
    }
}
