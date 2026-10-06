//! Credential rotation.
//!
//! A platform that rotates the broker credentials (a secrets manager's agent
//! or a mounted Kubernetes secret) rewrites the file
//! `messaging.credentials_file` names. The client asks for credentials on
//! every connection, the first one and each reconnect, so each answer here
//! reads the file again, as the Go client's `UserCredentials` does. A
//! connection the broker closes for an expired user reconnects with the
//! file's current content.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use secrecy::{ExposeSecret as _, SecretString};

/// Why a credentials file gave no credentials. Neither reason carries the
/// path or any of the file's content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CredentialsFileError {
    #[error("credentials file is unreadable: {0}")]
    Unreadable(std::io::ErrorKind),
    #[error("credentials file holds no user JWT and key seed")]
    Malformed,
}

impl CredentialsFileError {
    fn observe_challenge_failure(self) {
        let reason = match self {
            Self::Unreadable(_) => "unreadable",
            Self::Malformed => "malformed",
        };
        metrics::counter!("messaging_credentials_file_challenges_total", "outcome" => "failed", "reason" => reason).increment(1);
    }

    /// The closed `error.type` of this failure.
    pub(crate) const fn error_type(self) -> &'static str {
        match self {
            Self::Unreadable(_) => "unreadable_credentials_file",
            Self::Malformed => "malformed_credentials",
        }
    }
}

/// The admitted credentials file.
#[derive(Clone)]
pub(crate) struct CredentialsFile {
    path: Arc<Path>,
    /// The user JWT of the last answer. It names the user and is not the
    /// secret; the key seed is, and it is never kept.
    last_jwt: Arc<Mutex<String>>,
}

/// Neither the path nor the user it last read is formatted.
impl std::fmt::Debug for CredentialsFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CredentialsFile")
            .finish_non_exhaustive()
    }
}

impl CredentialsFile {
    /// Reads the file once so an unreadable or malformed one fails startup
    /// instead of every later connection attempt.
    pub(crate) async fn admit(path: PathBuf) -> Result<Self, CredentialsFileError> {
        let (jwt, _) = read(&path).await?;
        Ok(Self {
            path: path.into(),
            last_jwt: Arc::new(Mutex::new(jwt)),
        })
    }

    /// Answers one connection's challenge with the file's current user.
    pub(crate) async fn answer(
        &self,
        nonce: &[u8],
    ) -> Result<async_nats::Auth, CredentialsFileError> {
        metrics::describe_counter!(
            "messaging_credentials_file_challenges_total",
            "Completed file-backed JWT/signature challenge preparations; not broker authentication."
        );
        let credentials = read(&self.path).await;
        let (jwt, key) = credentials.inspect_err(|error| {
            error.observe_challenge_failure();
            tracing::warn!(error.type = error.error_type(), "messaging_credentials_file_failed");
        })?;
        let signature = key
            .sign(nonce)
            .map_err(|_| CredentialsFileError::Malformed)
            .inspect_err(|error| error.observe_challenge_failure())?;
        let previous = std::mem::replace(
            &mut *self.last_jwt.lock().unwrap_or_else(PoisonError::into_inner),
            jwt.clone(),
        );
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

async fn read(path: &Path) -> Result<(String, nkeys::KeyPair), CredentialsFileError> {
    let content = tokio::fs::read_to_string(path)
        .await
        .map(SecretString::from)
        .map_err(|error| CredentialsFileError::Unreadable(error.kind()))?;
    parse(content.expose_secret())
}

/// The user JWT and key of a credentials file: the first and the second
/// value that stands alone on a line between two dashed marker lines, the
/// layout `nsc` writes and the NATS clients read.
pub(crate) fn parse(content: &str) -> Result<(String, nkeys::KeyPair), CredentialsFileError> {
    let lines: Vec<&str> = content.lines().map(str::trim).collect();
    let mut values = lines.windows(3).filter_map(|window| {
        let &[opening, value, closing] = window else {
            return None;
        };
        (is_marker(opening) && !value.is_empty() && !is_marker(value) && is_marker(closing))
            .then_some(value)
    });
    let (Some(jwt), Some(seed)) = (values.next(), values.next()) else {
        return Err(CredentialsFileError::Malformed);
    };
    let key = nkeys::KeyPair::from_seed(seed).map_err(|_| CredentialsFileError::Malformed)?;
    Ok((jwt.to_owned(), key))
}

/// A line such as `-----BEGIN NATS USER JWT-----`.
fn is_marker(line: &str) -> bool {
    line.len() >= 6 && line.starts_with("---") && line.ends_with("---")
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "credential tests assert on a temporary credentials file"
)]
mod tests {
    use super::*;

    /// A credentials file as `nsc generate creds` writes it. The JWT is not
    /// a real one: only the broker reads its claims.
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

        // Windows line endings and a missing final line break still parse.
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
    #[allow(
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

        // Admission and an unpolled challenge supply no completed observation.
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
    #[allow(
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

        // A file that breaks after admission fails that connection only.
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
                credentials.answer(b"nonce").await.unwrap_err(),
                CredentialsFileError::Malformed
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
}
