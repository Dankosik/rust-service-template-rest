use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use secrecy::{ExposeSecret as _, SecretString};
use tokio::sync::Semaphore;

const CREDENTIALS_FILE_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CredentialsFileError {
    #[error("credentials file is unreadable: {0}")]
    Unreadable(std::io::ErrorKind),
    #[error("credentials file holds no user JWT and key seed")]
    Malformed,
}

pub(crate) fn describe_metrics() {
    use metrics::describe_counter;

    describe_counter!(
        "messaging_credentials_file_challenges_total",
        "Completed file-backed JWT/signature challenge preparations; not broker authentication."
    );
}

impl CredentialsFileError {
    fn observe_challenge_failure(self) {
        let reason = match self {
            Self::Unreadable(_) => "unreadable",
            Self::Malformed => "malformed",
        };
        metrics::counter!("messaging_credentials_file_challenges_total", "outcome" => "failed", "reason" => reason).increment(1);
    }

    pub(crate) const fn error_type(self) -> &'static str {
        match self {
            Self::Unreadable(_) => "unreadable_credentials_file",
            Self::Malformed => "malformed_credentials",
        }
    }

    pub(crate) fn io_error_kind(self) -> Option<tracing::field::DebugValue<std::io::ErrorKind>> {
        match self {
            Self::Unreadable(kind) => Some(tracing::field::debug(kind)),
            Self::Malformed => None,
        }
    }
}

#[derive(Clone, derive_more::Debug)]
#[debug("CredentialsFile {{ .. }}")]
pub(crate) struct CredentialsFile {
    path: Arc<Path>,
    last_jwt: Arc<Mutex<String>>,
    reading: Arc<Semaphore>,
}

impl CredentialsFile {
    pub(crate) async fn admit(path: PathBuf) -> Result<Self, CredentialsFileError> {
        let path: Arc<Path> = path.into();
        let reading = Arc::new(Semaphore::new(1));
        let (jwt, _) = read(&path, &reading).await?;
        Ok(Self {
            path,
            last_jwt: Arc::new(Mutex::new(jwt)),
            reading,
        })
    }

    pub(crate) async fn answer(
        &self,
        nonce: &[u8],
    ) -> Result<async_nats::Auth, CredentialsFileError> {
        let credentials = read(&self.path, &self.reading).await;
        let (jwt, key) = credentials.inspect_err(|error| {
            error.observe_challenge_failure();
            tracing::warn!(
                error.type = error.error_type(),
                io.error_kind = error.io_error_kind(),
                "messaging_credentials_file_failed"
            );
        })?;
        let signature = key
            .sign(nonce)
            .map_err(|_| CredentialsFileError::Malformed)
            .inspect_err(|error| error.observe_challenge_failure())?;
        let previous = std::mem::replace(&mut *self.last_jwt.lock(), jwt.clone());
        if previous != jwt {
            tracing::info!("messaging_credentials_reloaded");
        }
        let mut auth = async_nats::Auth::new();
        auth.jwt = Some(jwt);
        auth.signature = Some(signature);
        metrics::counter!("messaging_credentials_file_challenges_total", "outcome" => "prepared", "reason" => "none").increment(1);
        Ok(auth)
    }
}

async fn read(
    path: &Arc<Path>,
    reading: &Arc<Semaphore>,
) -> Result<(String, nkeys::KeyPair), CredentialsFileError> {
    let permit = reading
        .clone()
        .try_acquire_owned()
        .map_err(|_| CredentialsFileError::Unreadable(std::io::ErrorKind::ResourceBusy))?;
    let path = Arc::clone(path);
    let content = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        read_file(&path)
    })
    .await
    .map_err(|_| CredentialsFileError::Unreadable(std::io::ErrorKind::Other))?
    .map_err(|error| CredentialsFileError::Unreadable(error.kind()))?;
    parse(content.expose_secret())
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "the blocking worker validates and reads the same open descriptor"
)]
fn read_file(path: &Path) -> std::io::Result<SecretString> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits());
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::ErrorKind::InvalidInput.into());
    }
    let content = SecretString::from(std::io::read_to_string(
        file.take(CREDENTIALS_FILE_LIMIT as u64 + 1),
    )?);
    if content.expose_secret().len() > CREDENTIALS_FILE_LIMIT {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    Ok(content)
}

fn parse(content: &str) -> Result<(String, nkeys::KeyPair), CredentialsFileError> {
    async_nats::parse_jwt_and_key_from_creds(content)
        .map(|(jwt, key)| (jwt.to_owned(), key))
        .map_err(|_| CredentialsFileError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds(jwt: &str, user: &nkeys::KeyPair) -> String {
        format!(
            "-----BEGIN NATS USER JWT-----\n{jwt}\n------END NATS USER JWT------\n\n\
             ************************* IMPORTANT *************************\n\
             NKEY Seed printed below can be used to sign and prove identity.\n\
             NKEYs are sensitive and should be treated as secrets.\n\n\
             -----BEGIN USER NKEY SEED-----\n{}\n------END USER NKEY SEED------\n\n\
             *************************************************************\n",
            user.seed().unwrap()
        )
    }

    #[test]
    fn parsing_agrees_with_the_client_on_what_a_credentials_file_holds() {
        let user = nkeys::KeyPair::new_user();
        let content = creds("eyJ0eXAiOiJKV1QifQ.eyJzdWIiOiJVIn0.c2ln", &user);

        let (jwt, key) = parse(&content).unwrap();
        assert_eq!(jwt, "eyJ0eXAiOiJKV1QifQ.eyJzdWIiOiJVIn0.c2ln");
        assert_eq!(key.public_key(), user.public_key());
        assert!(
            async_nats::ConnectOptions::new()
                .credentials(&content)
                .is_ok()
        );

        let crlf = content.replace('\n', "\r\n");
        let at_eof = crlf.trim_end_matches(['\r', '\n', '*']);
        assert!(at_eof.ends_with("------END USER NKEY SEED------"));
        let (jwt, key) = parse(at_eof).unwrap();
        assert!(
            async_nats::ConnectOptions::new()
                .credentials(at_eof)
                .is_ok()
        );
        assert_eq!(jwt, "eyJ0eXAiOiJKV1QifQ.eyJzdWIiOiJVIn0.c2ln");
        assert_eq!(key.public_key(), user.public_key());

        let padded = creds(" user.jwt.value", &user);
        for malformed in [
            "",
            &padded,
            "eyJ0eXAiOiJKV1QifQ.eyJzdWIiOiJVIn0.c2ln",
            "-----BEGIN NATS USER JWT-----\njwt\n------END NATS USER JWT------\n",
            "-----BEGIN NATS USER JWT-----\njwt\n------END NATS USER JWT------\n\
             -----BEGIN USER NKEY SEED-----\nnot-a-seed\n------END USER NKEY SEED------\n",
        ] {
            assert_eq!(
                parse(malformed).unwrap_err(),
                CredentialsFileError::Malformed
            );
            assert!(
                async_nats::ConnectOptions::new()
                    .credentials(malformed)
                    .is_err()
            );
        }
    }

    #[tokio::test]
    #[expect(
        clippy::disallowed_methods,
        reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
    )]
    async fn each_connection_signs_with_the_user_the_file_holds_now() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let file = tempfile::NamedTempFile::new().unwrap();
        let first = nkeys::KeyPair::new_user();
        let mut content = creds("first.jwt.value", &first);
        content.extend(std::iter::repeat_n(
            ' ',
            CREDENTIALS_FILE_LIMIT - content.len(),
        ));
        std::fs::write(file.path(), &content).unwrap();
        let credentials = CredentialsFile::admit(file.path().to_owned())
            .await
            .unwrap();

        drop(credentials.answer(b"cancelled-nonce"));
        assert!(
            !recorder
                .handle()
                .render()
                .contains("messaging_credentials_file_challenges_total")
        );
        let auth = credentials.answer(b"nonce-1").await.unwrap();
        assert_eq!(auth.jwt.as_deref(), Some("first.jwt.value"));
        first.verify(b"nonce-1", &auth.signature.unwrap()).unwrap();
        credentials.answer(b"repeated-material").await.unwrap();

        let rotated = nkeys::KeyPair::new_user();
        content.push(' ');
        std::fs::write(file.path(), content).unwrap();
        assert_eq!(
            credentials.answer(b"oversize").await.err(),
            Some(CredentialsFileError::Unreadable(
                std::io::ErrorKind::InvalidData
            ))
        );
        std::fs::write(file.path(), creds("rotated.jwt.value", &rotated)).unwrap();
        let auth = credentials.answer(b"nonce-2").await.unwrap();
        assert_eq!(auth.jwt.as_deref(), Some("rotated.jwt.value"));
        let signature = auth.signature.unwrap();
        rotated.verify(b"nonce-2", &signature).unwrap();
        assert!(first.verify(b"nonce-2", &signature).is_err());
        let scrape = recorder.handle().render();
        let mut samples: Vec<_> = scrape
            .lines()
            .filter(|line| line.starts_with("messaging_credentials_file_challenges_total{"))
            .collect();
        samples.sort_unstable();
        assert_eq!(
            samples,
            [
                "messaging_credentials_file_challenges_total{outcome=\"failed\",reason=\"unreadable\"} 1",
                "messaging_credentials_file_challenges_total{outcome=\"prepared\",reason=\"none\"} 3"
            ]
        );
        for secret in [
            "first.jwt.value",
            "rotated.jwt.value",
            &first.seed().unwrap(),
            &rotated.seed().unwrap(),
            file.path().to_str().unwrap(),
        ] {
            assert!(!scrape.contains(secret));
        }
    }

    #[tokio::test]
    #[expect(
        clippy::disallowed_methods,
        reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
    )]
    async fn an_unreadable_or_malformed_file_fails_without_naming_its_path_or_content() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let _recorder = metrics::set_default_local_recorder(&recorder);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sentinel-path.creds");
        let error = CredentialsFile::admit(path.clone()).await.unwrap_err();
        assert_eq!(
            error,
            CredentialsFileError::Unreadable(std::io::ErrorKind::NotFound)
        );
        assert!(!error.to_string().contains("sentinel"), "{error}");

        std::fs::write(&path, "sentinel-content").unwrap();
        let error = CredentialsFile::admit(path.clone()).await.unwrap_err();
        assert_eq!(error, CredentialsFileError::Malformed);
        assert!(!error.to_string().contains("sentinel"), "{error}");

        let user = nkeys::KeyPair::new_user();
        std::fs::write(&path, creds("user.jwt.value", &user)).unwrap();
        let credentials = CredentialsFile::admit(path.clone()).await.unwrap();
        assert!(
            !recorder
                .handle()
                .render()
                .contains("messaging_credentials_file_challenges_total")
        );
        std::fs::remove_file(&path).unwrap();
        assert!(credentials.answer(b"nonce").await.is_err());
        for content in ["sentinel-content", "other-invalid-tuple"] {
            std::fs::write(&path, content).unwrap();
            assert_eq!(
                credentials.answer(b"nonce").await.err(),
                Some(CredentialsFileError::Malformed)
            );
        }
        std::fs::write(&path, creds("user.jwt.value", &user)).unwrap();
        assert!(credentials.answer(b"nonce").await.is_ok());
        let scrape = recorder.handle().render();
        let mut samples: Vec<_> = scrape
            .lines()
            .filter(|line| line.starts_with("messaging_credentials_file_challenges_total{"))
            .collect();
        samples.sort_unstable();
        assert_eq!(
            samples,
            [
                "messaging_credentials_file_challenges_total{outcome=\"failed\",reason=\"malformed\"} 2",
                "messaging_credentials_file_challenges_total{outcome=\"failed\",reason=\"unreadable\"} 1",
                "messaging_credentials_file_challenges_total{outcome=\"prepared\",reason=\"none\"} 1",
            ]
        );
        assert!(!scrape.contains("sentinel"));
        assert!(!scrape.contains("user.jwt.value"));
    }

    #[test]
    #[expect(clippy::disallowed_methods, reason = "test-owned credentials file")]
    fn cancelling_a_queued_read_keeps_the_slot_until_the_worker_finishes() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("user.creds");
            std::fs::write(&path, creds("user.jwt.value", &nkeys::KeyPair::new_user())).unwrap();
            let file = CredentialsFile::admit(path).await.unwrap();
            let (release, gate) = tokio::sync::oneshot::channel::<()>();
            let blocker = tokio::task::spawn_blocking(move || gate.blocking_recv());
            {
                let mut challenge = std::pin::pin!(file.answer(b"nonce"));
                assert!(futures_util::poll!(challenge.as_mut()).is_pending());
            }
            std::assert_matches!(
                futures_util::poll!(std::pin::pin!(file.answer(b"nonce"))).map(Result::err),
                std::task::Poll::Ready(Some(CredentialsFileError::Unreadable(
                    std::io::ErrorKind::ResourceBusy
                )))
            );
            release.send(()).unwrap();
            blocker.await.unwrap().unwrap();
            tokio::task::spawn_blocking(|| ()).await.unwrap();
            assert!(file.answer(b"nonce").await.is_ok());
        });
    }

    #[cfg(unix)]
    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "test-owned rotating credentials file"
    )]
    fn replacing_an_unserviced_fifo_restores_authentication() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        let runtime = std::mem::DropGuard::new(runtime, |runtime| {
            if std::thread::panicking() {
                runtime.shutdown_background();
            }
        });
        runtime.block_on(async {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("user.creds");
            let replacement = directory.path().join("replacement");
            let user = nkeys::KeyPair::new_user();
            std::fs::write(&path, creds("initial", &user)).unwrap();
            let file = CredentialsFile::admit(path.clone()).await.unwrap();
            nix::unistd::mkfifo(&replacement, nix::sys::stat::Mode::S_IRUSR).unwrap();
            std::fs::rename(&replacement, &path).unwrap();
            let outcome =
                tokio::time::timeout(std::time::Duration::from_secs(2), file.answer(b"nonce"))
                    .await
                    .expect("an unserviced FIFO must return within the challenge budget");
            assert_eq!(
                outcome.err(),
                Some(CredentialsFileError::Unreadable(
                    std::io::ErrorKind::InvalidInput
                ))
            );
            std::fs::write(&replacement, creds("rotated", &user)).unwrap();
            std::fs::rename(replacement, path).unwrap();
            assert_eq!(
                file.answer(b"nonce").await.unwrap().jwt.as_deref(),
                Some("rotated")
            );
        });
    }
}
