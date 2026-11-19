use health::{Probe, ProbeError};
use sqlx::postgres::PgPool;
use sqlx::{Connection, PgConnection};

#[derive(Clone, Debug)]
pub struct PostgresProbe {
    pool: PgPool,
    require_writable: bool,
}

impl PostgresProbe {
    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            require_writable: false,
        }
    }

    #[must_use]
    pub fn writable_primary(pool: PgPool) -> Self {
        Self {
            pool,
            require_writable: true,
        }
    }
}

#[health::async_trait]
impl Probe for PostgresProbe {
    fn name(&self) -> &'static str {
        "postgres"
    }

    async fn check(&self) -> Result<(), ProbeError> {
        let mut conn = crate::pool::acquire_probe(&self.pool)
            .await
            .map_err(|err| probe_error(&err))?;
        if !self.require_writable {
            return conn.ping().await.map_err(|err| probe_error(&err));
        }
        let session = session_properties(&mut conn)
            .await
            .map_err(|err| probe_error(&err))?;
        if !session.writable {
            return Err(ProbeError::from_static(
                "the PostgreSQL session is not writable",
            ));
        }
        Ok(())
    }
}

#[derive(Debug)]
pub struct SessionProperties {
    pub server_encoding: String,
    pub writable: bool,
    pub read_committed: bool,
}

pub async fn session_properties(conn: &mut PgConnection) -> Result<SessionProperties, sqlx::Error> {
    sqlx::query_as!(
        SessionProperties,
        "SELECT current_setting('server_encoding') AS \"server_encoding!\", \
     NOT pg_is_in_recovery() AND current_setting('transaction_read_only') = 'off' AS \"writable!\", \
     current_setting('default_transaction_isolation') = 'read committed' AS \"read_committed!\""
    )
    .fetch_one(conn)
    .await
}

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
