//! Password rotation.
//!
//! A platform that rotates the database password (a secrets manager's agent,
//! a mounted Kubernetes secret, a sidecar that writes short-lived tokens)
//! rewrites the file `postgres.password_file` names. `sqlx` takes its connect
//! options when the pool is built; `Pool::set_connect_options` is the
//! driver's own way to hand it later ones, and this task is what calls it.
//! Connections already open are left alone: the server authenticates a
//! session once, and the pool retires them at their maximum lifetime.

use std::path::Path;
use std::time::Duration;

use sqlx::postgres::PgPool;
use tokio_util::sync::CancellationToken;

use crate::dsn::{Dsn, DsnError, password_from};

/// How often the password file is read again. A connection opened between a
/// rotation and the next read is refused by the server and the pool retries
/// it inside the caller's acquire budget, so this is also the longest a
/// rotation without an overlap window can fail new connections.
pub const PASSWORD_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// Give the pool the password file's current content for every connection
/// it opens from then on, every [`PASSWORD_REFRESH_INTERVAL`] until `cancel`
/// fires. Returns at once for a DSN whose password came from the URL.
pub async fn refresh_password_periodically(pool: PgPool, dsn: Dsn, cancel: CancellationToken) {
    let Some(path) = dsn.password_file() else {
        return;
    };
    let _ = cancel
        .run_until_cancelled(async {
            let mut ticker = tokio::time::interval(PASSWORD_REFRESH_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut refresh = Refresh::default();
            loop {
                ticker.tick().await;
                refresh.step(&pool, path).await;
            }
        })
        .await;
}

/// What the task remembers between reads.
#[derive(Default)]
struct Refresh {
    /// The password the pool was last given by this task.
    current: Option<String>,
    /// Whether the last read failed, so one outage is one warning.
    unreadable: bool,
}

impl Refresh {
    async fn step(&mut self, pool: &PgPool, path: &Path) {
        match read(path).await {
            Ok(password) => {
                self.unreadable = false;
                if self.current.as_deref() == Some(password.as_str()) {
                    return;
                }
                let options = (*pool.connect_options()).clone().password(&password);
                pool.set_connect_options(options);
                // The first read repeats what admission already used.
                if self.current.replace(password).is_some() {
                    tracing::info!("postgres_password_reloaded");
                }
            }
            // The pool keeps the last password it was given.
            Err(error) => {
                if !std::mem::replace(&mut self.unreadable, true) {
                    tracing::warn!(%error, "postgres_password_file_unreadable");
                }
            }
        }
    }
}

async fn read(path: &Path) -> Result<String, DsnError> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|err| DsnError::PasswordFile(err.kind()))?;
    password_from(content)
}

#[cfg(test)]
mod tests {
    use sqlx::ConnectOptions;
    use sqlx::postgres::PgPoolOptions;

    use super::*;

    fn password(pool: &PgPool) -> Option<String> {
        pool.connect_options()
            .to_url_lossy()
            .password()
            .map(str::to_owned)
    }

    #[tokio::test]
    async fn the_pool_follows_the_file_and_keeps_its_password_while_the_file_is_unreadable() {
        let dir = std::env::temp_dir().join(format!("pg-password-refresh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("password");
        std::fs::write(&file, "first\n").unwrap();
        let dsn = Dsn::admit_with(
            "postgres://app@127.0.0.1:1/app?sslmode=disable",
            Some(&file),
        )
        .unwrap();
        let pool = PgPoolOptions::new().connect_lazy_with(dsn.connect_options());
        assert_eq!(password(&pool).as_deref(), Some("first"));

        let mut refresh = Refresh::default();
        refresh.step(&pool, &file).await;
        assert_eq!(password(&pool).as_deref(), Some("first"));

        std::fs::write(&file, "second").unwrap();
        refresh.step(&pool, &file).await;
        assert_eq!(password(&pool).as_deref(), Some("second"));

        std::fs::remove_file(&file).unwrap();
        refresh.step(&pool, &file).await;
        assert!(refresh.unreadable);
        assert_eq!(password(&pool).as_deref(), Some("second"));

        std::fs::write(&file, "third\n").unwrap();
        refresh.step(&pool, &file).await;
        assert!(!refresh.unreadable);
        assert_eq!(password(&pool).as_deref(), Some("third"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_password_from_the_url_needs_no_task() {
        let dsn = Dsn::admit("postgres://app:pw@127.0.0.1:1/app?sslmode=disable").unwrap();
        let pool = PgPoolOptions::new().connect_lazy_with(dsn.connect_options());
        // Returns although the token is never cancelled.
        refresh_password_periodically(pool, dsn, CancellationToken::new()).await;
    }
}
