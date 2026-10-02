//! Password rotation.
//!
//! A platform that rotates the cache password (a secrets manager's agent, a
//! mounted Kubernetes secret, a sidecar that writes short-lived tokens)
//! rewrites the file `cache.password_file` names. redis-rs takes changing
//! credentials through a [`StreamingCredentialsProvider`]: every connection
//! attempt authenticates with the first item of a new subscription, and the
//! open connection then follows a subscription of its own, sending `AUTH`
//! for each item. Each subscription here yields the file's content at once
//! and then every change of it, so a connection repeats its `AUTH` once
//! after it opens and again whenever the file is rewritten.

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::Stream;
use redis::{BasicAuth, RedisError, RedisResult, StreamingCredentialsProvider};

use crate::CacheError;

/// How often a live connection's password file is read again. A token that
/// expires must be rewritten at least this long before its expiry.
const PASSWORD_REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// The ACL user `AUTH <password>` names when the DSN gives none.
const DEFAULT_USER: &str = "default";

/// The admitted password file and the user it authenticates.
pub(crate) struct PasswordFile {
    path: Arc<Path>,
    username: Arc<str>,
}

impl PasswordFile {
    /// Reads the file once so an unreadable or empty one fails startup
    /// instead of every later connection attempt.
    pub(crate) fn admit(path: PathBuf, username: Option<&str>) -> Result<Self, CacheError> {
        let content = std::fs::read_to_string(&path)
            .map_err(|error| CacheError::PasswordFile { kind: error.kind() })?;
        password_from(content)?;
        Ok(Self {
            path: path.into(),
            username: username
                .filter(|username| !username.is_empty())
                .unwrap_or(DEFAULT_USER)
                .into(),
        })
    }
}

impl StreamingCredentialsProvider for PasswordFile {
    fn subscribe(&self) -> Pin<Box<dyn Stream<Item = RedisResult<BasicAuth>> + Send + 'static>> {
        let watch = Watch {
            path: self.path.clone(),
            username: self.username.clone(),
            started: false,
            current: None,
            unreadable: false,
        };
        Box::pin(futures_util::stream::unfold(
            watch,
            |mut watch| async move {
                let credentials = watch.next().await;
                Some((credentials, watch))
            },
        ))
    }
}

/// What one subscription remembers between reads.
struct Watch {
    path: Arc<Path>,
    username: Arc<str>,
    /// Whether the first item was produced. Every later one waits first.
    started: bool,
    /// The password this stream last yielded.
    current: Option<String>,
    /// Whether the last later read failed, so one outage is one warning.
    unreadable: bool,
}

impl Watch {
    /// The file's content at once, then each change of it.
    async fn next(&mut self) -> RedisResult<BasicAuth> {
        if !std::mem::replace(&mut self.started, true) {
            // A connection cannot open without a password: the attempt fails
            // and the client's own retry subscribes and reads again.
            let password = read(&self.path).await.map_err(|error| {
                RedisError::from((
                    redis::ErrorKind::AuthenticationFailed,
                    "cache password file",
                    error.to_string(),
                ))
            })?;
            return Ok(self.yield_password(password));
        }
        loop {
            tokio::time::sleep(PASSWORD_REFRESH_INTERVAL).await;
            if let Some(credentials) = self.step().await {
                return Ok(credentials);
            }
        }
    }

    /// One later read: the credentials when the password changed.
    async fn step(&mut self) -> Option<BasicAuth> {
        match read(&self.path).await {
            Ok(password) => {
                self.unreadable = false;
                if self.current.as_deref() == Some(password.as_str()) {
                    return None;
                }
                if self.current.is_some() {
                    tracing::info!("cache_password_reloaded");
                }
                Some(self.yield_password(password))
            }
            // The connection stays authenticated with the last password.
            Err(error) => {
                if !std::mem::replace(&mut self.unreadable, true) {
                    tracing::warn!(%error, "cache_password_file_unreadable");
                }
                None
            }
        }
    }

    fn yield_password(&mut self, password: String) -> BasicAuth {
        self.current = Some(password.clone());
        BasicAuth::new(self.username.to_string(), password)
    }
}

async fn read(path: &Path) -> Result<String, CacheError> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map_err(|error| CacheError::PasswordFile { kind: error.kind() })?;
    password_from(content)
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

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        reason = "credential tests assert on a temporary password file"
    )]

    use super::*;

    fn watch(path: &Path) -> Watch {
        Watch {
            path: path.into(),
            username: DEFAULT_USER.into(),
            started: false,
            current: None,
            unreadable: false,
        }
    }

    #[tokio::test]
    async fn a_stream_yields_the_file_then_only_its_changes() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "first\n").unwrap();
        let mut watch = watch(file.path());

        let first = watch.next().await.unwrap();
        assert_eq!(first.username(), "default");
        assert!(first.password() == "first", "initial password differs");
        assert!(watch.step().await.is_none());

        std::fs::write(file.path(), "second").unwrap();
        assert!(
            watch.step().await.unwrap().password() == "second",
            "rotated password differs"
        );

        // An unreadable or empty file keeps the last password.
        std::fs::write(file.path(), "").unwrap();
        assert!(watch.step().await.is_none());
        assert!(watch.unreadable);
        assert!(
            watch.current.as_deref() == Some("second"),
            "last password changed"
        );

        std::fs::write(file.path(), "third\r\n").unwrap();
        assert!(
            watch.step().await.unwrap().password() == "third",
            "recovered password differs"
        );
        assert!(!watch.unreadable);
    }

    #[tokio::test]
    async fn a_new_connection_fails_while_the_file_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("password");
        let mut watch = watch(&path);

        let error = watch.next().await.unwrap_err();
        assert_eq!(error.kind(), redis::ErrorKind::AuthenticationFailed);
        assert_eq!(crate::observe::error_type(&error).label(), "auth");
        let rendered = error.to_string();
        assert!(!rendered.contains(path.to_str().unwrap()), "{rendered}");
    }

    /// redis-rs keeps polling a subscription after an error, so a failed
    /// first read must not turn into a read loop.
    #[tokio::test(start_paused = true)]
    async fn a_failed_first_read_waits_before_the_next_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("password");
        let mut watch = watch(&path);
        watch.next().await.unwrap_err();

        std::fs::write(&path, "late").unwrap();
        let started = tokio::time::Instant::now();
        assert!(
            watch.next().await.unwrap().password() == "late",
            "late password differs"
        );
        assert!(started.elapsed() >= PASSWORD_REFRESH_INTERVAL);
    }
}
