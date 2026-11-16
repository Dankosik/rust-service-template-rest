use std::path::{Path, PathBuf};
use std::sync::Arc;

use itertools::Itertools;
use parking_lot::Mutex;
use secrecy::{ExposeSecret as _, SecretString};
use tokio::sync::Semaphore;

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
    .map_err(|_| CredentialsFileError::Unreadable(std::io::ErrorKind::Other))??;
    parse(content.expose_secret())
}

#[expect(
    clippy::disallowed_methods,
    reason = "owned blocking read; `read` keeps at most one in flight"
)]
fn read_file(path: &Path) -> Result<SecretString, CredentialsFileError> {
    std::fs::read_to_string(path)
        .map(SecretString::from)
        .map_err(|error| CredentialsFileError::Unreadable(error.kind()))
}

fn parse(content: &str) -> Result<(String, nkeys::KeyPair), CredentialsFileError> {
    let mut values =
        content
            .lines()
            .map(str::trim)
            .tuple_windows()
            .filter_map(|(opening, value, closing)| {
                (is_marker(opening) && !value.is_empty() && !is_marker(value) && is_marker(closing))
                    .then_some(value)
            });
    let (Some(jwt), Some(seed)) = (values.next(), values.next()) else {
        return Err(CredentialsFileError::Malformed);
    };
    let key = nkeys::KeyPair::from_seed(seed).map_err(|_| CredentialsFileError::Malformed)?;
    Ok((jwt.to_owned(), key))
}

fn is_marker(line: &str) -> bool {
    line.len() >= 6 && line.starts_with("---") && line.ends_with("---")
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

        let (jwt, key) = parse(content.replace('\n', "\r\n").trim_end()).unwrap();
        assert_eq!(jwt, "eyJ0eXAiOiJKV1QifQ.eyJzdWIiOiJVIn0.c2ln");
        assert_eq!(key.public_key(), user.public_key());

        for malformed in [
            "",
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
        std::fs::write(file.path(), creds("first.jwt.value", &first)).unwrap();
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
        std::fs::write(file.path(), creds("rotated.jwt.value", &rotated)).unwrap();
        let auth = credentials.answer(b"nonce-2").await.unwrap();
        assert_eq!(auth.jwt.as_deref(), Some("rotated.jwt.value"));
        let signature = auth.signature.unwrap();
        rotated.verify(b"nonce-2", &signature).unwrap();
        assert!(first.verify(b"nonce-2", &signature).is_err());
        let scrape = recorder.handle().render();
        let samples: Vec<_> = scrape
            .lines()
            .filter(|line| line.starts_with("messaging_credentials_file_challenges_total{"))
            .collect();
        assert_eq!(
            samples,
            ["messaging_credentials_file_challenges_total{outcome=\"prepared\",reason=\"none\"} 3"]
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

    #[tokio::test(flavor = "multi_thread")]
    #[expect(
        clippy::disallowed_methods,
        clippy::disallowed_types,
        reason = "test-owned credentials fixtures; the FIFO writer runs on an owned blocking thread"
    )]
    async fn a_hung_credentials_read_is_never_joined_by_a_second() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("user.creds");
        let user = nkeys::KeyPair::new_user();
        std::fs::write(&path, creds("user.jwt.value", &user)).unwrap();
        let credentials = CredentialsFile::admit(path.clone()).await.unwrap();
        std::fs::remove_file(&path).unwrap();
        let status = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap();
        assert!(status.success());

        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                credentials.answer(b"nonce")
            )
            .await
            .is_err(),
            "the FIFO read must hang"
        );
        let second = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            credentials.answer(b"nonce"),
        )
        .await;

        let fifo = directory.path().join("stale");
        std::fs::rename(&path, &fifo).unwrap();
        std::fs::write(&path, creds("user.jwt.value", &user)).unwrap();
        tokio::task::spawn_blocking(move || {
            use std::io::Write;
            std::fs::OpenOptions::new()
                .write(true)
                .open(fifo)
                .and_then(|mut writer| writer.write_all(b"stale"))
        })
        .await
        .unwrap()
        .unwrap();
        assert_eq!(
            second.unwrap().err(),
            Some(CredentialsFileError::Unreadable(
                std::io::ErrorKind::ResourceBusy
            )),
            "a second read must fail at once instead of starting another"
        );
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        while credentials.answer(b"nonce").await.is_err() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the released read must free the slot"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}
