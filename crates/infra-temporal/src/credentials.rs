use std::fmt;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::ParsedCertificate;
use sha2::{Digest as _, Sha256};
use temporalio_client::{CertifiedKey, Connection, ResolvesClientCert, SignatureScheme};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tonic::metadata::AsciiMetadataValue;
use zeroize::{Zeroize as _, Zeroizing};

use crate::connection::ConnectError;
use crate::metrics::WorkspaceMetricsMeter;

const MAX_CREDENTIAL_FILE_BYTES: usize = 1024 * 1024;
const CREDENTIAL_READ_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) const KIND_API_KEY: &str = "api_key";
pub(crate) const KIND_MTLS: &str = "mtls";
pub(crate) const KIND_ROOT_CA: &str = "root_ca";

pub(crate) struct RotatingClientCertResolver {
    current: Arc<ArcSwap<CertifiedKey>>,
}

impl RotatingClientCertResolver {
    #[must_use]
    pub(crate) fn new(initial: CertifiedKey) -> Self {
        Self {
            current: Arc::new(ArcSwap::from_pointee(initial)),
        }
    }

    pub(crate) fn swap(&self, next: CertifiedKey) {
        self.current.store(Arc::new(next));
    }

    #[cfg(test)]
    pub(crate) fn current_cert_der(&self) -> Vec<u8> {
        self.current
            .load()
            .cert
            .first()
            .map_or_default(|cert| cert.as_ref().to_vec())
    }
}

impl fmt::Debug for RotatingClientCertResolver {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RotatingClientCertResolver { .. }")
    }
}

impl ResolvesClientCert for RotatingClientCertResolver {
    fn resolve(
        &self,
        _root_hint_subjects: &[&[u8]],
        sigschemes: &[SignatureScheme],
    ) -> Option<Arc<CertifiedKey>> {
        let certified = self.current.load_full();
        certified
            .key
            .choose_scheme(sigschemes)
            .is_some()
            .then_some(certified)
    }

    fn has_certs(&self) -> bool {
        true
    }
}

pub(crate) fn validate_inline_api_key(key: &str) -> bool {
    !key.is_empty()
        && key.trim_ascii() == key
        && key.bytes().all(|byte| (0x20..=0x7E).contains(&byte))
        && format!("Bearer {key}")
            .parse::<AsciiMetadataValue>()
            .is_ok()
}

pub(crate) fn parse_and_validate_api_key(
    mut raw_bytes: Zeroizing<Vec<u8>>,
) -> Result<(Zeroizing<String>, [u8; 32]), ()> {
    let raw = std::mem::take(&mut *raw_bytes);
    let mut text = String::from_utf8(raw).map_err(|error| {
        error.into_bytes().zeroize();
    })?;
    let without_breaks = shared_rules::secret_file::value(std::mem::take(&mut text));
    let mut normalized = Zeroizing::new(without_breaks);
    let trimmed = normalized.trim_ascii();
    if trimmed.is_empty()
        || !trimmed.bytes().all(|byte| (0x20..=0x7E).contains(&byte))
        || format!("Bearer {trimmed}")
            .parse::<AsciiMetadataValue>()
            .is_err()
    {
        return Err(());
    }
    let key = Zeroizing::new(trimmed.to_owned());
    normalized.zeroize();
    let digest = Sha256::digest(key.as_bytes()).into();
    Ok((key, digest))
}

pub(crate) fn validate_root_ca_pem(pem_bytes: &[u8]) -> Result<(), ()> {
    let mut count = 0_usize;
    for item in CertificateDer::pem_slice_iter(pem_bytes) {
        let cert = item.map_err(|_| ())?;
        ParsedCertificate::try_from(&cert).map_err(|_| ())?;
        count += 1;
    }
    if count == 0 {
        return Err(());
    }
    Ok(())
}

pub(crate) fn parse_certified_key(
    cert_pem: &[u8],
    key_pem: &[u8],
) -> Result<(CertifiedKey, [u8; 32]), ()> {
    let mut chain = Vec::new();
    for item in CertificateDer::pem_slice_iter(cert_pem) {
        let cert = item.map_err(|_| ())?;
        ParsedCertificate::try_from(&cert).map_err(|_| ())?;
        chain.push(cert);
    }
    if chain.is_empty() {
        return Err(());
    }
    let private_key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|_| ())?;
    let provider = rustls::crypto::aws_lc_rs::default_provider();
    let certified = CertifiedKey::from_der(chain, private_key, &provider).map_err(|_| ())?;
    certified.keys_match().map_err(|_| ())?;
    let digest = combined_digest(cert_pem, key_pem);
    Ok((certified, digest))
}

fn combined_digest(first: &[u8], second: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update((first.len() as u64).to_le_bytes());
    hasher.update(first);
    hasher.update((second.len() as u64).to_le_bytes());
    hasher.update(second);
    hasher.finalize().into()
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "blocking workers must read from the same descriptor they validated as a regular file"
)]
fn read_regular_file_secret(path: &Path) -> io::Result<Zeroizing<Vec<u8>>> {
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
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let capacity = usize::try_from(metadata.len())
        .unwrap_or(MAX_CREDENTIAL_FILE_BYTES)
        .min(MAX_CREDENTIAL_FILE_BYTES)
        .saturating_add(1);
    let mut bytes = Zeroizing::new(Vec::with_capacity(capacity));
    file.take(MAX_CREDENTIAL_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_CREDENTIAL_FILE_BYTES {
        return Err(io::ErrorKind::InvalidData.into());
    }
    Ok(bytes)
}

async fn read_file_bounded(
    path: PathBuf,
    reader: &Arc<Semaphore>,
) -> Result<Zeroizing<Vec<u8>>, ()> {
    let permit = reader.clone().try_acquire_owned().map_err(|_| ())?;
    tokio::task::spawn_blocking(move || {
        let _capacity = permit;
        read_regular_file_secret(&path)
    })
    .await
    .map_err(|_| ())?
    .map_err(|_| ())
}

pub(crate) async fn load_root_ca_file(
    path: PathBuf,
    reader: &Arc<Semaphore>,
) -> Result<Vec<u8>, ConnectError> {
    let raw = read_file_bounded(path, reader)
        .await
        .map_err(|()| ConnectError::CredentialRead { kind: KIND_ROOT_CA })?;
    validate_root_ca_pem(&raw)
        .map_err(|()| ConnectError::TlsSetup("invalid root ca certificate"))?;
    Ok(raw.to_vec())
}

pub(crate) async fn load_api_key_file(
    path: PathBuf,
    reader: &Arc<Semaphore>,
) -> Result<(Zeroizing<String>, [u8; 32]), ConnectError> {
    let raw = read_file_bounded(path, reader)
        .await
        .map_err(|()| ConnectError::CredentialRead { kind: KIND_API_KEY })?;
    parse_and_validate_api_key(raw)
        .map_err(|()| ConnectError::CredentialRead { kind: KIND_API_KEY })
}

pub(crate) struct LoadedMtls {
    pub(crate) cert_pem: Vec<u8>,
    pub(crate) key_pem: Zeroizing<Vec<u8>>,
    pub(crate) certified_key: CertifiedKey,
    pub(crate) digest: [u8; 32],
}

pub(crate) async fn load_mtls_files(
    cert_path: PathBuf,
    key_path: PathBuf,
    reader: &Arc<Semaphore>,
) -> Result<LoadedMtls, ConnectError> {
    let permit = reader
        .clone()
        .try_acquire_owned()
        .map_err(|_| ConnectError::CredentialRead { kind: KIND_MTLS })?;
    let (cert_raw, key_raw) = tokio::task::spawn_blocking(move || {
        let _capacity = permit;
        let cert = read_regular_file_secret(&cert_path)?;
        let key = read_regular_file_secret(&key_path)?;
        Ok::<_, io::Error>((cert, key))
    })
    .await
    .map_err(|_| ConnectError::CredentialRead { kind: KIND_MTLS })?
    .map_err(|_| ConnectError::CredentialRead { kind: KIND_MTLS })?;

    let (certified_key, digest) = parse_certified_key(&cert_raw, &key_raw)
        .map_err(|()| ConnectError::TlsSetup("invalid client identity"))?;
    Ok(LoadedMtls {
        cert_pem: cert_raw.to_vec(),
        key_pem: key_raw,
        certified_key,
        digest,
    })
}

pub(crate) struct ApiKeyWatch {
    path: PathBuf,
    last_digest: [u8; 32],
    connection: Connection,
}

impl ApiKeyWatch {
    pub(crate) const fn new(path: PathBuf, last_digest: [u8; 32], connection: Connection) -> Self {
        Self {
            path,
            last_digest,
            connection,
        }
    }
}

pub(crate) struct MtlsWatch {
    cert_path: PathBuf,
    key_path: PathBuf,
    last_digest: [u8; 32],
    resolver: Arc<RotatingClientCertResolver>,
}

impl MtlsWatch {
    pub(crate) const fn new(
        cert_path: PathBuf,
        key_path: PathBuf,
        last_digest: [u8; 32],
        resolver: Arc<RotatingClientCertResolver>,
    ) -> Self {
        Self {
            cert_path,
            key_path,
            last_digest,
            resolver,
        }
    }
}

pub(crate) struct CredentialWatcher {
    api_key: Option<ApiKeyWatch>,
    mtls: Option<MtlsWatch>,
    reader: Arc<Semaphore>,
    #[cfg(test)]
    step_tx: Option<tokio::sync::watch::Sender<u64>>,
}

impl CredentialWatcher {
    pub(crate) fn new(
        api_key: Option<ApiKeyWatch>,
        mtls: Option<MtlsWatch>,
        reader: Arc<Semaphore>,
    ) -> Self {
        Self {
            api_key,
            mtls,
            reader,
            #[cfg(test)]
            step_tx: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_step_observer(mut self) -> (Self, tokio::sync::watch::Receiver<u64>) {
        let (tx, rx) = tokio::sync::watch::channel(0_u64);
        self.step_tx = Some(tx);
        (self, rx)
    }

    pub(crate) const fn is_active(&self) -> bool {
        self.api_key.is_some() || self.mtls.is_some()
    }

    pub(crate) async fn step(&mut self) {
        if let Some(watch) = &mut self.api_key {
            let outcome = tokio::time::timeout(
                CREDENTIAL_READ_TIMEOUT,
                load_api_key_file(watch.path.clone(), &self.reader),
            )
            .await;
            if let Ok(Ok((key, digest))) = outcome {
                if digest != watch.last_digest {
                    watch.connection.set_api_key(Some(key.as_str().to_owned()));
                    watch.last_digest = digest;
                    WorkspaceMetricsMeter::record_credential_reload(KIND_API_KEY, "ok");
                    tracing::info!(kind = KIND_API_KEY, "temporal_credential_reloaded");
                }
            } else {
                WorkspaceMetricsMeter::record_credential_reload(KIND_API_KEY, "failed");
                tracing::warn!(kind = KIND_API_KEY, "temporal_credential_reload_failed");
            }
        }

        if let Some(watch) = &mut self.mtls {
            let outcome = tokio::time::timeout(
                CREDENTIAL_READ_TIMEOUT,
                load_mtls_files(
                    watch.cert_path.clone(),
                    watch.key_path.clone(),
                    &self.reader,
                ),
            )
            .await;
            if let Ok(Ok(loaded)) = outcome {
                if loaded.digest != watch.last_digest {
                    watch.resolver.swap(loaded.certified_key);
                    watch.last_digest = loaded.digest;
                    WorkspaceMetricsMeter::record_credential_reload(KIND_MTLS, "ok");
                    tracing::info!(kind = KIND_MTLS, "temporal_credential_reloaded");
                }
            } else {
                WorkspaceMetricsMeter::record_credential_reload(KIND_MTLS, "failed");
                tracing::warn!(kind = KIND_MTLS, "temporal_credential_reload_failed");
            }
        }
    }

    pub(crate) async fn run(mut self, interval: Duration, cancel: CancellationToken) {
        let _ = cancel
            .run_until_cancelled(async {
                let first_tick = tokio::time::Instant::now() + interval;
                let mut ticker = tokio::time::interval_at(first_tick, interval);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                loop {
                    ticker.tick().await;
                    self.step().await;
                    #[cfg(test)]
                    if let Some(step_tx) = &self.step_tx {
                        step_tx.send_modify(|count| *count += 1);
                    }
                }
            })
            .await;
    }
}
