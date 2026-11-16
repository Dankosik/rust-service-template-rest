//! `sqlx` takes connect options when the pool is built; this task hands it new
//! ones through `Pool::set_connect_options` when the file `postgres.password_file`
//! names is rewritten. Open connections are left alone: the server authenticates
//! a session once. [`crate::pool::MAX_CONNECTION_LIFETIME`] is an age threshold
//! for retirement, not forced interruption or a hard revocation deadline.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::postgres::PgPool;
use tokio_util::sync::CancellationToken;

use crate::dsn::{DsnError, password_from};

const REFRESHES_METRIC: &str = "postgres_password_file_refreshes_total";

/// Until changed options are installed and accepted, new connections can fail
/// with `28P01` and the pool returns that instead of retrying; this cadence does
/// not bound the cutover.
pub const PASSWORD_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// A failed read leaves the last installed options in use.
pub async fn refresh_password_periodically(
    pool: PgPool,
    password_file: PathBuf,
    cancel: CancellationToken,
) {
    metrics::describe_counter!(
        REFRESHES_METRIC,
        "Completed periodic password-file reads and future connection-options installations; not database authentication."
    );
    let _ = cancel
        .run_until_cancelled(async {
            let mut ticker = tokio::time::interval(PASSWORD_REFRESH_INTERVAL);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut refresh = Refresh::default();
            loop {
                ticker.tick().await;
                refresh.step(&pool, &password_file).await;
            }
        })
        .await;
}

#[derive(Default)]
struct Refresh {
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
                    metrics::counter!(REFRESHES_METRIC, "outcome" => "unchanged").increment(1);
                    return;
                }
                let options = (*pool.connect_options()).clone().password(&password);
                pool.set_connect_options(options);
                if self.current.replace(password).is_some() {
                    tracing::info!("postgres_password_reloaded");
                }
                metrics::counter!(REFRESHES_METRIC, "outcome" => "installed").increment(1);
            }
            Err(error) => {
                metrics::counter!(REFRESHES_METRIC, "outcome" => "read_failed").increment(1);
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
    use crate::Dsn;

    fn password(pool: &PgPool) -> Option<String> {
        pool.connect_options()
            .to_url_lossy()
            .password()
            .map(str::to_owned)
    }

    #[tokio::test]
    #[expect(
        clippy::disallowed_methods,
        reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
    )]
    async fn the_pool_follows_the_file_and_keeps_its_password_while_the_file_is_unreadable() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("password");
        std::fs::write(&file, "first\n").unwrap();
        let dsn = Dsn::admit_with(
            "postgres://app@127.0.0.1:1/app?sslmode=disable",
            Some(&file),
        )
        .unwrap();
        let pool = PgPoolOptions::new().connect_lazy_with(dsn.connect_options());
        assert_eq!(password(&pool).as_deref(), Some("first"));
        assert!(
            !recorder
                .handle()
                .render()
                .contains("postgres_password_file_refreshes_total")
        );

        let mut refresh = Refresh::default();
        refresh.step(&pool, &file).await;
        refresh.step(&pool, &file).await;
        assert_eq!(password(&pool).as_deref(), Some("first"));

        std::fs::write(&file, "second").unwrap();
        refresh.step(&pool, &file).await;
        assert_eq!(password(&pool).as_deref(), Some("second"));

        std::fs::remove_file(&file).unwrap();
        refresh.step(&pool, &file).await;
        assert!(refresh.unreadable);
        assert_eq!(password(&pool).as_deref(), Some("second"));
        refresh.step(&pool, &file).await;
        std::fs::write(&file, "").unwrap();
        refresh.step(&pool, &file).await;
        assert_eq!(password(&pool).as_deref(), Some("second"));

        std::fs::write(&file, "third\n").unwrap();
        refresh.step(&pool, &file).await;
        assert!(!refresh.unreadable);
        assert_eq!(password(&pool).as_deref(), Some("third"));
        dir.close().unwrap();
        let scrape = recorder.handle().render();
        let mut samples: Vec<_> = scrape
            .lines()
            .filter(|line| line.starts_with("postgres_password_file_refreshes_total{"))
            .collect();
        samples.sort_unstable();
        assert_eq!(
            samples,
            [
                "postgres_password_file_refreshes_total{outcome=\"installed\"} 3",
                "postgres_password_file_refreshes_total{outcome=\"read_failed\"} 3",
                "postgres_password_file_refreshes_total{outcome=\"unchanged\"} 1",
            ]
        );
        for secret in [file.to_str().unwrap(), "first", "second", "third"] {
            assert!(!scrape.contains(secret));
        }
    }
}
