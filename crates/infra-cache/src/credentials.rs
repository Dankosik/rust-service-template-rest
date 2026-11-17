use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Semaphore;
use zeroize::Zeroizing;

use crate::CacheError;

const DEFAULT_USER: &str = "default";

#[derive(derive_more::Debug)]
#[debug("PasswordFile {{ .. }}")]
pub(crate) struct PasswordFile {
    path: PathBuf,
    username: String,
    reading: Arc<Semaphore>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ReadFailure {
    #[error("cache password file could not be read ({0})")]
    Io(ErrorKind),
    #[error("cache password file is empty")]
    Empty,
    #[error("cache password file read is refused while an earlier read is still running")]
    Busy,
    #[error("cache password file read did not finish within its bound")]
    TimedOut,
    #[error("cache password file read panicked")]
    Panicked,
}

impl PasswordFile {
    pub(crate) fn admit(path: PathBuf, username: Option<&str>) -> Result<Self, CacheError> {
        let content =
            read_password_file(&path).map_err(|kind| CacheError::PasswordFile { kind })?;
        password_from(content).ok_or(CacheError::PasswordFileEmpty)?;
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

    pub(crate) async fn read(&self) -> Result<Zeroizing<String>, ReadFailure> {
        let permit = self
            .reading
            .clone()
            .try_acquire_owned()
            .map_err(|_| ReadFailure::Busy)?;
        let path = self.path.clone();
        let content = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            read_password_file(&path)
        })
        .await
        .map_err(|_| ReadFailure::Panicked)?
        .map_err(ReadFailure::Io)?;
        password_from(content).ok_or(ReadFailure::Empty)
    }
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    clippy::verbose_file_reads,
    reason = "blocking workers must read from the same descriptor they validated as a regular file"
)]
pub(crate) fn read_regular_file(path: &Path) -> io::Result<Vec<u8>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits());
    }
    let mut file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(ErrorKind::InvalidInput.into());
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn read_password_file(path: &Path) -> Result<String, ErrorKind> {
    let bytes = read_regular_file(path).map_err(|error| error.kind())?;
    String::from_utf8(bytes).map_err(|_| ErrorKind::InvalidData)
}

fn password_from(content: String) -> Option<Zeroizing<String>> {
    Some(Zeroizing::new(shared_rules::secret_file::value(content)))
        .filter(|password| !password.is_empty())
}
