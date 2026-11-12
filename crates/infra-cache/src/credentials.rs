//! Admitted password-file input. The connection supervisor owns refresh and AUTH.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Semaphore;

use crate::CacheError;

const DEFAULT_USER: &str = "default";

#[derive(derive_more::Debug)]
#[debug("PasswordFile {{ .. }}")]
pub(crate) struct PasswordFile {
    path: PathBuf,
    username: String,
    /// One permit, held by a blocking read until it ends, including one whose
    /// caller stopped waiting.
    reading: Arc<Semaphore>,
}

impl PasswordFile {
    /// Admission checks availability; every subsequent setup reads again.
    pub(crate) fn admit(path: PathBuf, username: Option<&str>) -> Result<Self, CacheError> {
        let content = read_password_file(&path)?;
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

    /// A blocking read can outlive its caller's wait. While it runs, another
    /// read fails at once as [`ErrorKind::ResourceBusy`].
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
        })??;
        password_from(content)
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "startup admission and owned blocking reads; PasswordFile::read keeps at most one in flight"
)]
fn read_password_file(path: &Path) -> Result<String, CacheError> {
    std::fs::read_to_string(path).map_err(|error| CacheError::PasswordFile { kind: error.kind() })
}

fn password_from(content: String) -> Result<String, CacheError> {
    let password = shared_rules::secret_file::value(content);
    if password.is_empty() {
        return Err(CacheError::PasswordFileEmpty);
    }
    Ok(password)
}
