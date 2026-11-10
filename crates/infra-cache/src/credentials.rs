//! Admitted password-file input. The connection supervisor owns refresh and AUTH.

use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::CacheError;

const DEFAULT_USER: &str = "default";

pub(crate) struct PasswordFile {
    path: PathBuf,
    username: String,
    /// Set while a blocking read runs, including one whose caller stopped waiting.
    reading: Arc<AtomicBool>,
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
            reading: Arc::default(),
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
        if self.reading.swap(true, Ordering::AcqRel) {
            return Err(CacheError::PasswordFile {
                kind: ErrorKind::ResourceBusy,
            });
        }
        let finished = ReadFinished(self.reading.clone());
        let path = self.path.clone();
        let content = tokio::task::spawn_blocking(move || {
            let _finished = finished;
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

/// Clears the in-flight mark when the blocking read ends, panics included.
struct ReadFinished(Arc<AtomicBool>);

impl Drop for ReadFinished {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "owned blocking work: PasswordFile::read keeps at most one in flight"
)]
fn read_password_file(path: &std::path::Path) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

/// The password in a password file's content: the whole file, minus one
/// trailing line break that editors and `echo` add.
fn password_from(mut password: String) -> Result<String, CacheError> {
    if password.ends_with('\n') {
        password.pop();
        if password.ends_with('\r') {
            password.pop();
        }
    }
    if password.is_empty() {
        return Err(CacheError::PasswordFileEmpty);
    }
    Ok(password)
}
