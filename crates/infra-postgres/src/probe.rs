//! The probe shares the request pool so readiness reflects its saturation; the
//! refresher owns the failure threshold and the whole-check bound.

use health::{Probe, ProbeError};
use sqlx::Connection;
use sqlx::postgres::PgPool;

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

/// Names the failure class without dependency internals, since the driver's text
/// can quote server details; a server rejection carries its bounded SQLSTATE.
fn probe_error(err: &sqlx::Error) -> ProbeError {
    ProbeError::new(match err {
        sqlx::Error::PoolTimedOut => "no connection available inside the acquire budget".to_owned(),
        sqlx::Error::PoolClosed => "pool is closed".to_owned(),
        sqlx::Error::Io(_) => "connection failed".to_owned(),
        sqlx::Error::Tls(_) => "tls handshake failed".to_owned(),
        sqlx::Error::Database(_) => match crate::error::sqlstate(err) {
            Some(code) => format!("server rejected the readiness check (SQLSTATE {code})"),
            None => "server rejected the readiness check".to_owned(),
        },
        _ => "readiness check failed".to_owned(),
    })
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
