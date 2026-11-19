use std::time::Duration;

use infra_postgres::{Pool, observed};

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StartupError {
    #[error("the jobs store requires UTF8 server encoding")]
    UnsupportedEncoding,
    #[error("the PostgreSQL session is not writable")]
    NotWritable,
    #[error("the jobs store requires READ COMMITTED session isolation")]
    UnsupportedIsolation,
    #[error("the jobs store is unavailable")]
    Unavailable,
}

pub(crate) const STARTUP_CHECK_BUDGET: Duration = Duration::from_secs(5);

pub(crate) async fn check_startup(pool: &Pool) -> Result<(), StartupError> {
    let read = async {
        let mut connection = infra_postgres::acquire(pool, "check jobs startup").await?;
        observed!(
            "check jobs session",
            infra_postgres::session_properties(&mut connection),
        )
        .await
    };
    let session = match tokio::time::timeout(STARTUP_CHECK_BUDGET, read).await {
        Ok(Ok(session)) => session,
        Ok(Err(err)) => {
            tracing::warn!(
                sqlstate = infra_postgres::sqlstate(&err).as_deref(),
                cause = infra_postgres::failure_cause(&err),
                "jobs_startup_check_failed"
            );
            return Err(StartupError::Unavailable);
        }
        Err(_elapsed) => {
            tracing::warn!(cause = "timeout", "jobs_startup_check_failed");
            return Err(StartupError::Unavailable);
        }
    };
    if session.server_encoding != "UTF8" {
        return Err(StartupError::UnsupportedEncoding);
    }
    if !session.writable {
        return Err(StartupError::NotWritable);
    }
    if !session.read_committed {
        return Err(StartupError::UnsupportedIsolation);
    }
    Ok(())
}
