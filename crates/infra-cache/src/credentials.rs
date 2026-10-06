//! Admitted password-file input. The connection supervisor owns refresh and AUTH.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::CacheError;

pub(crate) const PASSWORD_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const DEFAULT_USER: &str = "default";

pub(crate) struct PasswordFile {
    path: Arc<Path>,
    username: Arc<str>,
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
    #[allow(
        clippy::disallowed_methods,
        reason = "startup cache admission owns the initial password read; refresh uses async IO"
    )]
    pub(crate) fn admit(path: PathBuf, username: Option<&str>) -> Result<Self, CacheError> {
        let content = std::fs::read_to_string(&path)
            .map_err(|error| CacheError::PasswordFile { kind: error.kind() })?;
        password_from(content)?;
        Ok(Self {
            path: path.into(),
            username: username
                .filter(|user| !user.is_empty())
                .unwrap_or(DEFAULT_USER)
                .into(),
        })
    }

    pub(crate) fn username(&self) -> &str {
        &self.username
    }

    /// The supervisor bounds this read with setup or refresh's shared deadline.
    pub(crate) async fn read(&self) -> Result<String, CacheError> {
        let content = tokio::fs::read_to_string(&self.path)
            .await
            .map_err(|error| CacheError::PasswordFile { kind: error.kind() })?;
        password_from(content)
    }
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
