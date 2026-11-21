use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use zeroize::{Zeroize as _, Zeroizing};

use crate::ServerEndpoint;
use crate::error::{ErrorType, StartErrorKind};
use crate::observe::PASSWORD_REFRESHES_METRIC;

const DEFAULT_USER: &str = "default";
const MAX_PASSWORD_FILE_BYTES: usize = 16 * 1024;
const PASSWORD_READ_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const PASSWORD_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

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
    pub(crate) fn admit(path: PathBuf, username: Option<&str>) -> Result<Self, StartErrorKind> {
        let content =
            read_password_file(&path).map_err(|kind| StartErrorKind::PasswordFile { kind })?;
        password_from(content).ok_or(StartErrorKind::PasswordFileEmpty)?;
        Ok(Self {
            path,
            username: username
                .filter(|user| !user.is_empty())
                .unwrap_or(DEFAULT_USER)
                .into(),
            reading: Arc::new(Semaphore::new(1)),
        })
    }

    pub(crate) async fn read(&self, tasks: &TaskTracker) -> Result<Zeroizing<String>, ReadFailure> {
        let permit = self
            .reading
            .clone()
            .try_acquire_owned()
            .map_err(|_| ReadFailure::Busy)?;
        let path = self.path.clone();
        let content = tasks
            .spawn_blocking(move || {
                let _permit = permit;
                read_password_file(&path)
            })
            .await
            .map_err(|_| ReadFailure::Panicked)?
            .map_err(ReadFailure::Io)?;
        password_from(content).ok_or(ReadFailure::Empty)
    }

    pub(crate) async fn connect_settings<'a>(
        &'a self,
        base: &redis::RedisConnectionInfo,
        server: &ServerEndpoint,
        tasks: &'a TaskTracker,
    ) -> Result<(redis::RedisConnectionInfo, Rotation<'a>), ErrorType> {
        let password = self.read(tasks).await.map_err(|error| {
            warn_password_file_unreadable(server, error);
            ErrorType::Other
        })?;
        let settings = base
            .clone()
            .set_username(&self.username)
            .set_password(password.as_str());
        Ok((settings, Rotation::new(self, tasks, password)))
    }
}

pub(crate) struct Rotation<'a> {
    file: &'a PasswordFile,
    tasks: &'a TaskTracker,
    authenticated_password: Zeroizing<String>,
    last_warning: Option<RefreshWarning>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RefreshWarning {
    Unreadable,
    AuthRefused,
}

enum RefreshOutcome {
    ReadFailed(ReadFailure),
    Unchanged,
    AuthAccepted,
    AuthRefused,
    Failed(ErrorType),
}

impl RefreshOutcome {
    fn outcome_label(&self) -> &'static str {
        match self {
            Self::ReadFailed(_) => "read_failed",
            Self::Unchanged => "unchanged",
            Self::AuthAccepted => "auth_accepted",
            Self::AuthRefused | Self::Failed(_) => "refresh_failed",
        }
    }

    fn error_type(&self) -> Option<ErrorType> {
        match self {
            Self::AuthRefused => Some(ErrorType::Auth),
            Self::Failed(error) => Some(*error),
            Self::ReadFailed(_) | Self::Unchanged | Self::AuthAccepted => None,
        }
    }

    fn disposition(&self) -> Disposition {
        match self {
            Self::ReadFailed(_) | Self::Unchanged | Self::AuthAccepted | Self::AuthRefused => {
                Disposition::Keep
            }
            Self::Failed(_) => Disposition::Retire,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Disposition {
    Keep,
    Retire,
}

impl<'a> Rotation<'a> {
    fn new(
        file: &'a PasswordFile,
        tasks: &'a TaskTracker,
        authenticated_password: Zeroizing<String>,
    ) -> Self {
        Self {
            file,
            tasks,
            authenticated_password,
            last_warning: None,
        }
    }

    pub(crate) async fn refresh_and_record(
        &mut self,
        server: &ServerEndpoint,
        retired: &CancellationToken,
        authenticate: impl AsyncFnOnce(&redis::Cmd) -> Result<(), ErrorType>,
    ) -> Disposition {
        let outcome = tokio::select! {
            biased;
            () = retired.cancelled() => return Disposition::Retire,
            outcome = self.refresh(retired, authenticate) => outcome,
        };
        self.record(server, &outcome);
        outcome.disposition()
    }

    async fn refresh(
        &mut self,
        retired: &CancellationToken,
        authenticate: impl AsyncFnOnce(&redis::Cmd) -> Result<(), ErrorType>,
    ) -> RefreshOutcome {
        let password = match timeout(PASSWORD_READ_TIMEOUT, self.file.read(self.tasks)).await {
            Ok(Ok(password)) => password,
            Ok(Err(error)) => return RefreshOutcome::ReadFailed(error),
            Err(_elapsed) => return RefreshOutcome::ReadFailed(ReadFailure::TimedOut),
        };
        if password.as_str() == self.authenticated_password.as_str() {
            return RefreshOutcome::Unchanged;
        }
        let mut command = redis::cmd("AUTH");
        command.arg(&self.file.username).arg(password.as_str());
        if let Err(error) = authenticate(&command).await {
            return match error {
                ErrorType::Auth => RefreshOutcome::AuthRefused,
                error => RefreshOutcome::Failed(error),
            };
        }
        if retired.is_cancelled() {
            return RefreshOutcome::Failed(ErrorType::Io);
        }
        self.authenticated_password = password;
        RefreshOutcome::AuthAccepted
    }

    fn record(&mut self, server: &ServerEndpoint, outcome: &RefreshOutcome) {
        let reason = outcome.error_type().map_or("none", ErrorType::label);
        metrics::counter!(
            PASSWORD_REFRESHES_METRIC,
            "outcome" => outcome.outcome_label(),
            "reason" => reason,
        )
        .increment(1);
        let warning = match outcome {
            RefreshOutcome::ReadFailed(_) => Some(RefreshWarning::Unreadable),
            RefreshOutcome::AuthRefused => Some(RefreshWarning::AuthRefused),
            RefreshOutcome::Unchanged
            | RefreshOutcome::AuthAccepted
            | RefreshOutcome::Failed(_) => None,
        };
        let previous = std::mem::replace(&mut self.last_warning, warning);
        if warning.is_some() && warning == previous {
            return;
        }
        match outcome {
            RefreshOutcome::ReadFailed(error) => warn_password_file_unreadable(server, *error),
            RefreshOutcome::Unchanged => {}
            RefreshOutcome::AuthAccepted => tracing::info!(
                server.address = %server.host,
                server.port = server.port,
                "cache_password_reloaded"
            ),
            RefreshOutcome::AuthRefused => warn_password_refresh_failed(server, ErrorType::Auth),
            RefreshOutcome::Failed(error) => warn_password_refresh_failed(server, *error),
        }
    }
}

fn warn_password_file_unreadable(server: &ServerEndpoint, error: ReadFailure) {
    tracing::warn!(
        server.address = %server.host,
        server.port = server.port,
        %error,
        "cache_password_file_unreadable"
    );
}

fn warn_password_refresh_failed(server: &ServerEndpoint, error: ErrorType) {
    tracing::warn!(
        server.address = %server.host,
        server.port = server.port,
        error.type = error.label(),
        "cache_password_refresh_failed"
    );
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "blocking workers must read from the same descriptor they validated as a regular file"
)]
fn read_regular_file_into(path: &Path, max_bytes: usize, bytes: &mut Vec<u8>) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits());
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(ErrorKind::InvalidInput.into());
    }
    let capacity = usize::try_from(metadata.len())
        .unwrap_or(max_bytes)
        .min(max_bytes)
        .saturating_add(1);
    bytes.reserve_exact(capacity);
    file.take(max_bytes as u64 + 1).read_to_end(bytes)?;
    if bytes.len() > max_bytes {
        return Err(ErrorKind::InvalidData.into());
    }
    Ok(())
}

pub(crate) fn read_regular_file(path: &Path, max_bytes: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    read_regular_file_into(path, max_bytes, &mut bytes)?;
    Ok(bytes)
}

pub(crate) fn read_secret_file(path: &Path, max_bytes: usize) -> io::Result<Zeroizing<Vec<u8>>> {
    let mut bytes = Zeroizing::new(Vec::new());
    read_regular_file_into(path, max_bytes, &mut bytes)?;
    Ok(bytes)
}

fn read_password_file(path: &Path) -> Result<Zeroizing<String>, ErrorKind> {
    let mut bytes =
        read_secret_file(path, MAX_PASSWORD_FILE_BYTES).map_err(|error| error.kind())?;
    let raw = std::mem::take(&mut *bytes);
    String::from_utf8(raw).map(Zeroizing::new).map_err(|error| {
        error.into_bytes().zeroize();
        ErrorKind::InvalidData
    })
}

fn password_from(mut content: Zeroizing<String>) -> Option<Zeroizing<String>> {
    let trimmed = shared_rules::secret_file::value(std::mem::take(&mut *content));
    Some(Zeroizing::new(trimmed)).filter(|password| !password.is_empty())
}
