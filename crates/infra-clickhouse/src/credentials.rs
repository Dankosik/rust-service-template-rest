use std::io::{self, ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use shared_rules::rfc9110::has_ascii_control;
use tokio::sync::{Notify, Semaphore};
use tokio::time::Instant;
use tokio_util::task::AbortOnDropHandle;
use zeroize::{Zeroize as _, Zeroizing};

use crate::Shared;
use crate::error::StartErrorKind;
use crate::observe::PasswordRefreshOutcome;

const PASSWORD_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const REACTIVE_REFRESH_COOLDOWN: Duration = Duration::from_secs(1);
const PASSWORD_READ_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_PASSWORD_FILE_BYTES: usize = 16 * 1024;

#[derive(derive_more::Debug)]
#[debug("ActiveCredentials {{ cred_epoch: {}, .. }}", self.cred_epoch)]
pub(crate) struct ActiveCredentials {
    pub(crate) cred_epoch: u64,
    pub(crate) read_client: clickhouse::Client,
    pub(crate) password: Option<Zeroizing<String>>,
}

#[derive(Clone, Copy)]
pub(crate) enum PasswordFileError {
    Io(ErrorKind),
    EmptyOrInvalid,
}

impl From<PasswordFileError> for StartErrorKind {
    fn from(error: PasswordFileError) -> Self {
        match error {
            PasswordFileError::Io(kind) => Self::PasswordFile { kind },
            PasswordFileError::EmptyOrInvalid => Self::PasswordFileEmpty,
        }
    }
}

#[derive(Clone, Copy)]
enum RefreshReadFailure {
    File(PasswordFileError),
    Busy,
    TimedOut,
    Panicked,
}

impl RefreshReadFailure {
    const fn reason(self) -> &'static str {
        match self {
            Self::File(PasswordFileError::Io(_)) => "io_error",
            Self::File(PasswordFileError::EmptyOrInvalid) => "empty_or_invalid",
            Self::Busy => "read_busy",
            Self::TimedOut => "timed_out",
            Self::Panicked => "panicked",
        }
    }
}

pub(crate) async fn run_password_refresh(shared: Arc<Shared>, path: PathBuf, trigger: Arc<Notify>) {
    let reading = Arc::new(Semaphore::new(1));
    let mut warned_read_failure = false;
    let mut last_reactive_refresh: Option<Instant> = None;
    let mut next_refresh = Instant::now() + PASSWORD_REFRESH_INTERVAL;

    loop {
        let reactive = tokio::select! {
            biased;
            () = shared.shutdown.cancelled() => return,
            () = trigger.notified() => true,
            () = tokio::time::sleep_until(next_refresh) => false,
        };
        if reactive
            && let Some(prev) = last_reactive_refresh
            && let Some(wait) = REACTIVE_REFRESH_COOLDOWN.checked_sub(prev.elapsed())
        {
            tokio::select! {
                biased;
                () = shared.shutdown.cancelled() => return,
                () = tokio::time::sleep(wait) => {}
            }
        }
        let now = Instant::now();
        if reactive {
            last_reactive_refresh = Some(now);
        }
        next_refresh = now + PASSWORD_REFRESH_INTERVAL;

        let result = tokio::select! {
            biased;
            () = shared.shutdown.cancelled() => return,
            result = read_once(&reading, &path) => result,
        };

        if shared.shutdown.is_cancelled() {
            return;
        }

        match result {
            Ok(new_password) => {
                warned_read_failure = false;
                let current = shared.credentials.load();
                if current.password.as_deref().map(String::as_str) == Some(new_password.as_str()) {
                    shared
                        .metrics
                        .record_password_refresh(PasswordRefreshOutcome::Unchanged);
                } else {
                    let next_epoch = current.cred_epoch.saturating_add(1);
                    let next_client = current
                        .read_client
                        .clone()
                        .with_password(new_password.as_str());
                    shared.credentials.store(Arc::new(ActiveCredentials {
                        cred_epoch: next_epoch,
                        read_client: next_client,
                        password: Some(new_password),
                    }));
                    shared
                        .metrics
                        .record_password_refresh(PasswordRefreshOutcome::Updated);
                    tracing::info!(
                        server.address = %shared.endpoint.host,
                        server.port = shared.endpoint.port,
                        cred_epoch = next_epoch,
                        "clickhouse_password_reloaded"
                    );
                }
            }
            Err(failure) => {
                shared
                    .metrics
                    .record_password_refresh(PasswordRefreshOutcome::ReadFailed);
                if !std::mem::replace(&mut warned_read_failure, true) {
                    tracing::warn!(
                        server.address = %shared.endpoint.host,
                        server.port = shared.endpoint.port,
                        reason = failure.reason(),
                        "clickhouse_password_file_unreadable"
                    );
                }
            }
        }
    }
}

async fn read_once(
    reading: &Arc<Semaphore>,
    path: &Path,
) -> Result<Zeroizing<String>, RefreshReadFailure> {
    let permit = Arc::clone(reading)
        .try_acquire_owned()
        .map_err(|_| RefreshReadFailure::Busy)?;
    let path = path.to_path_buf();
    let handle = AbortOnDropHandle::new(tokio::task::spawn_blocking(move || {
        let _permit = permit;
        read_password_file(&path)
    }));
    tokio::time::timeout(PASSWORD_READ_TIMEOUT, handle)
        .await
        .map_err(|_| RefreshReadFailure::TimedOut)?
        .map_err(|_| RefreshReadFailure::Panicked)?
        .map_err(RefreshReadFailure::File)
}

pub(crate) fn read_password_file(path: &Path) -> Result<Zeroizing<String>, PasswordFileError> {
    let mut bytes = read_secret_file(path, MAX_PASSWORD_FILE_BYTES)
        .map_err(|error| PasswordFileError::Io(error.kind()))?;
    let raw = std::mem::take(&mut *bytes);
    let mut content = String::from_utf8(raw)
        .map(Zeroizing::new)
        .map_err(|error| {
            error.into_bytes().zeroize();
            PasswordFileError::Io(ErrorKind::InvalidData)
        })?;
    let trimmed = Zeroizing::new(shared_rules::secret_file::value(std::mem::take(
        &mut *content,
    )));
    if trimmed.is_empty() || has_ascii_control(trimmed.as_bytes()) {
        return Err(PasswordFileError::EmptyOrInvalid);
    }
    Ok(trimmed)
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
        use std::os::unix::fs::OpenOptionsExt as _;

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
