use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

use sqlx::postgres::PgPool;
use tokio_util::sync::CancellationToken;
use zeroize::{Zeroize, Zeroizing};

use crate::dsn::DsnError;

const REFRESHES_METRIC: &str = "postgres_password_file_refreshes_total";
const PASSWORD_FILE_BYTES: usize = 16 * 1024;

pub const PASSWORD_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

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
    current: Option<Zeroizing<String>>,
    unreadable: bool,
}

impl Refresh {
    async fn step(&mut self, pool: &PgPool, path: &Path) {
        match read(path).await {
            Ok(password) => {
                self.unreadable = false;
                if self.current.as_ref() == Some(&password) {
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

async fn read(path: &Path) -> Result<Zeroizing<String>, DsnError> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || read_password(&path))
        .await
        .map_err(|_| DsnError::PasswordFile(io::ErrorKind::Other))?
}

pub(crate) fn read_password(path: &Path) -> Result<Zeroizing<String>, DsnError> {
    let content = read_file(path).map_err(|error| DsnError::PasswordFile(error.kind()))?;
    let password = Zeroizing::new(shared_rules::secret_file::value(content));
    if password.is_empty() {
        return Err(DsnError::PasswordFileEmpty);
    }
    Ok(password)
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "the blocking reader validates and reads the same open descriptor"
)]
fn read_file(path: &Path) -> io::Result<String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits());
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut content = io::read_to_string(file.take(PASSWORD_FILE_BYTES as u64 + 1))?;
    if content.len() > PASSWORD_FILE_BYTES {
        content.zeroize();
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(content)
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
        use metrics::Key;
        use metrics_util::debugging::DebuggingRecorder;

        let recorder = DebuggingRecorder::new();
        let snapshotter = recorder.snapshotter();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("password");
        std::fs::write(&file, "first\n").unwrap();
        assert_eq!(
            Dsn::admit_with(
                "postgres://app@127.0.0.1:1/app?sslmode=require",
                Some(&file)
            )
            .unwrap_err(),
            DsnError::ServerVerification,
        );
        let dsn = Dsn::admit_with(
            "postgres://app@127.0.0.1:1/app?sslmode=verify-full",
            Some(&file),
        )
        .unwrap();
        let pool = PgPoolOptions::new().connect_lazy_with(dsn.connect_options());
        assert_eq!(password(&pool).as_deref(), Some("first"));
        assert!(!format!("{:?}", pool.connect_options()).contains("first"));
        assert!(
            snapshotter
                .snapshot()
                .into_vec()
                .iter()
                .all(|(key, ..)| key.key().name() != REFRESHES_METRIC)
        );

        let mut refresh = Refresh::default();
        refresh.step(&pool, &file).await;
        refresh.step(&pool, &file).await;
        assert_eq!(password(&pool).as_deref(), Some("first"));

        std::fs::write(&file, "second").unwrap();
        refresh.step(&pool, &file).await;
        assert_eq!(password(&pool).as_deref(), Some("second"));
        assert!(!format!("{:#?}", pool.connect_options()).contains("second"));

        std::fs::remove_file(&file).unwrap();
        refresh.step(&pool, &file).await;
        assert!(refresh.unreadable);
        assert_eq!(password(&pool).as_deref(), Some("second"));
        std::fs::write(&file, vec![b'x'; 16 * 1024 + 1]).unwrap();
        refresh.step(&pool, &file).await;
        assert_eq!(password(&pool).as_deref(), Some("second"));
        std::fs::write(&file, "").unwrap();
        refresh.step(&pool, &file).await;
        assert_eq!(password(&pool).as_deref(), Some("second"));

        std::fs::write(&file, "third\n").unwrap();
        refresh.step(&pool, &file).await;
        assert!(!refresh.unreadable);
        assert_eq!(password(&pool).as_deref(), Some("third"));
        dir.close().unwrap();
        let snapshot = snapshotter.snapshot().into_hashmap();
        assert_eq!(snapshot.len(), 3, "{snapshot:?}");
        for (outcome, expected) in [("installed", 3), ("read_failed", 3), ("unchanged", 1)] {
            let target = Key::from_parts(REFRESHES_METRIC, &[("outcome", outcome)]);
            let actual = crate::test_metrics::counter(&snapshot, &target);
            assert_eq!(actual, Some(expected), "{target}: {snapshot:?}");
        }
        let debug = format!("{snapshot:?}");
        for secret in [file.to_str().unwrap(), "first", "second", "third"] {
            assert!(!debug.contains(secret));
        }
    }
}
