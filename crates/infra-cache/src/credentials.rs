//! Admitted password-file input. The connection supervisor owns refresh and AUTH.

use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::CacheError;

const DEFAULT_USER: &str = "default";

pub(crate) struct PasswordFile {
    path: PathBuf,
    username: String,
    /// One permit, held by a blocking read until it ends, including one whose
    /// caller stopped waiting.
    reading: Arc<Semaphore>,
}

impl std::fmt::Debug for PasswordFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PasswordFile")
            .finish_non_exhaustive()
    }
}

impl PasswordFile {
    /// Admission checks availability; every subsequent setup reads again.
    #[expect(
        clippy::disallowed_methods,
        reason = "startup cache admission owns the initial password read; refresh uses async IO"
    )]
    pub(crate) fn admit(path: PathBuf, username: Option<&str>) -> Result<Self, CacheError> {
        let content = std::fs::read_to_string(&path)
            .map_err(|error| CacheError::PasswordFile { kind: error.kind() })?;
        password_from(content)?;
        Ok(Self {
            path,
            username: username
                .filter(|user| !user.is_empty())
                .unwrap_or(DEFAULT_USER)
                .into(),
            reading: Arc::new(Semaphore::new(1)),
        })
    }

    pub(crate) fn username(&self) -> &str {
        &self.username
    }

    /// Dropping this future stops only the wait: the blocking read runs on, so
    /// a hung mount would pin one blocking thread per setup or refresh attempt.
    /// While that read runs, another fails at once as
    /// [`ErrorKind::ResourceBusy`] instead of starting a second.
    pub(crate) async fn read(&self) -> Result<String, CacheError> {
        let permit =
            self.reading
                .clone()
                .try_acquire_owned()
                .map_err(|_| CacheError::PasswordFile {
                    kind: ErrorKind::ResourceBusy,
                })?;
        let path = self.path.clone();
        let content = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            read_password_file(&path)
        })
        .await
        .map_err(|_| CacheError::PasswordFile {
            kind: ErrorKind::Other,
        })?
        .map_err(|error| CacheError::PasswordFile { kind: error.kind() })?;
        password_from(content)
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "owned blocking work: PasswordFile::read keeps at most one in flight"
)]
fn read_password_file(path: &std::path::Path) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

/// The password in a password file's content: the file's secret value, which
/// must not be empty.
fn password_from(content: String) -> Result<String, CacheError> {
    let password = shared_rules::secret_file::value(content);
    if password.is_empty() {
        return Err(CacheError::PasswordFileEmpty);
    }
    Ok(password)
}
