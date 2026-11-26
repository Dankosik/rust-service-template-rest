use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use operation_context::{BoundedStop, OperationContext, Stopped};
#[cfg(test)]
use temporalio_client::callback_based::CallbackBasedGrpcService;
use temporalio_client::errors::ClientConnectError;
use temporalio_client::{
    Client, ClientKeepAliveOptions, ClientOptions, ClientTlsOptions, Connection, ConnectionOptions,
    DnsLoadBalancingOptions, HttpConnectProxyOptions, TlsOptions,
};
#[cfg(feature = "worker")]
use temporalio_common::data_converters::DataConverter;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use url::Url;
use zeroize::Zeroizing;

use crate::client::TemporalClient;
use crate::credentials::{
    ApiKeyWatch, CredentialWatcher, MtlsWatch, RotatingClientCertResolver, load_api_key_file,
    load_mtls_files, load_root_ca_file,
};
use crate::metrics::{METRIC_PREFIX, WorkspaceMetricsMeter, describe_metrics};
use crate::options::{TemporalAuth, TemporalOptions, TlsRotationMode};
use crate::payload::build_data_converter;
use crate::probe::TemporalProbe;
use crate::trace::TraceClientInterceptor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CloseOutcome {
    Complete,
    TimedOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ConnectError {
    #[error("invalid temporal configuration: {0}")]
    InvalidConfig(&'static str),
    #[error("failed to read temporal {kind} credential")]
    CredentialRead { kind: &'static str },
    #[error("temporal tls setup failed: {0}")]
    TlsSetup(&'static str),
    #[error("temporal transport connection failed")]
    Transport,
    #[error("temporal connection timed out")]
    TimedOut,
    #[error("temporal connection cancelled")]
    Cancelled,
}

struct WatcherHandle {
    cancel: CancellationToken,
    task: JoinHandle<()>,
    #[cfg(test)]
    steps: tokio::sync::watch::Receiver<u64>,
}

impl Drop for WatcherHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

enum StartupState {
    Prepared(Box<TemporalOptions>),
    Admitted {
        connection: TemporalConnection,
        watcher: Option<WatcherHandle>,
        #[cfg(test)]
        mtls_resolver: Option<Arc<RotatingClientCertResolver>>,
    },
}

pub struct TemporalStartup {
    state: StartupState,
    startup_context: OperationContext,
    #[cfg(test)]
    service_override: Option<CallbackBasedGrpcService>,
}

impl fmt::Debug for TemporalStartup {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let phase = match &self.state {
            StartupState::Prepared(_) => "prepared",
            StartupState::Admitted { .. } => "admitted",
        };
        formatter
            .debug_struct("TemporalStartup")
            .field("state", &phase)
            .finish_non_exhaustive()
    }
}

pub(crate) struct InitialApiKey {
    value: Zeroizing<String>,
    watch: Option<(PathBuf, [u8; 32])>,
}

pub(crate) struct MtlsWatchSeed {
    cert_path: PathBuf,
    key_path: PathBuf,
    digest: [u8; 32],
    resolver: Arc<RotatingClientCertResolver>,
}

pub(crate) async fn load_initial_api_key(
    auth: &TemporalAuth,
    reader: &Arc<Semaphore>,
    connect_budget: &operation_context::BoundedContext,
    cancel: &CancellationToken,
) -> Result<Option<InitialApiKey>, ConnectError> {
    match auth {
        TemporalAuth::None => Ok(None),
        TemporalAuth::ApiKey(key) => Ok(Some(InitialApiKey {
            value: key.clone(),
            watch: None,
        })),
        TemporalAuth::ApiKeyFile(path) => {
            let (value, digest) = run_connect_stage(
                load_api_key_file(path.clone(), reader),
                connect_budget,
                cancel,
            )
            .await??;
            Ok(Some(InitialApiKey {
                value,
                watch: Some((path.clone(), digest)),
            }))
        }
    }
}

pub(crate) async fn build_tls_setup(
    options: &TemporalOptions,
    reader: &Arc<Semaphore>,
    connect_budget: &operation_context::BoundedContext,
    cancel: &CancellationToken,
) -> Result<(Option<TlsOptions>, Option<MtlsWatchSeed>), ConnectError> {
    if !options.tls_active() {
        return Ok((None, None));
    }

    let server_root_ca_cert = match options
        .tls
        .as_ref()
        .and_then(|tls| tls.server_root_ca_path.as_ref())
    {
        Some(path) => Some(
            run_connect_stage(
                load_root_ca_file(path.clone(), reader),
                connect_budget,
                cancel,
            )
            .await??,
        ),
        None => None,
    };

    let domain = options.tls.as_ref().and_then(|tls| tls.domain.clone());
    let mut client_tls_options = None;
    let mut client_cert_resolver = None;
    let mut mtls_watch_seed = None;

    if let Some(mtls) = options.tls.as_ref().and_then(|tls| tls.mtls.as_ref()) {
        let loaded = run_connect_stage(
            load_mtls_files(
                mtls.client_cert_path.clone(),
                mtls.client_key_path.clone(),
                reader,
            ),
            connect_budget,
            cancel,
        )
        .await??;
        match mtls.rotation {
            TlsRotationMode::Static => {
                client_tls_options = Some(
                    ClientTlsOptions::builder()
                        .client_cert(loaded.cert_pem)
                        .client_private_key(loaded.key_pem.to_vec())
                        .build(),
                );
            }
            TlsRotationMode::Dynamic => {
                let resolver = Arc::new(RotatingClientCertResolver::new(loaded.certified_key));
                mtls_watch_seed = Some(MtlsWatchSeed {
                    cert_path: mtls.client_cert_path.clone(),
                    key_path: mtls.client_key_path.clone(),
                    digest: loaded.digest,
                    resolver: Arc::clone(&resolver),
                });
                client_cert_resolver =
                    Some(resolver as Arc<dyn temporalio_client::ResolvesClientCert>);
            }
        }
    }

    let tls = TlsOptions::builder()
        .maybe_server_root_ca_cert(server_root_ca_cert)
        .maybe_domain(domain)
        .maybe_client_tls_options(client_tls_options)
        .maybe_client_cert_resolver(client_cert_resolver)
        .build();
    Ok((Some(tls), mtls_watch_seed))
}

pub(crate) fn build_connection_options(
    options: &TemporalOptions,
    meter: &WorkspaceMetricsMeter,
    remaining_connect: Duration,
    tls_options: Option<TlsOptions>,
    initial_api_key: Option<&InitialApiKey>,
    proxy_options: Option<HttpConnectProxyOptions>,
) -> ConnectionOptions {
    ConnectionOptions::new(options.endpoint.clone())
        .identity(options.identity.clone())
        .metrics_meter(meter.temporal_meter())
        .connect_timeout(remaining_connect)
        .keep_alive(Some(
            ClientKeepAliveOptions::builder()
                .interval(options.keep_alive_interval)
                .timeout(options.keep_alive_timeout)
                .build(),
        ))
        .dns_load_balancing(
            options
                .dns_load_balancing
                .then(DnsLoadBalancingOptions::default),
        )
        .maybe_tls_options(tls_options)
        .maybe_api_key(initial_api_key.map(|key| key.value.as_str().to_owned()))
        .maybe_http_connect_proxy(proxy_options)
        .build()
}

impl TemporalStartup {
    #[must_use]
    pub fn prepare(options: TemporalOptions, context: &OperationContext) -> Self {
        Self {
            state: StartupState::Prepared(Box::new(options)),
            startup_context: context.clone(),
            #[cfg(test)]
            service_override: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_service_override(mut self, service: CallbackBasedGrpcService) -> Self {
        self.service_override = Some(service);
        self
    }

    #[cfg(test)]
    pub(crate) fn watcher_steps(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        match &self.state {
            StartupState::Admitted {
                watcher: Some(handle),
                ..
            } => Some(handle.steps.clone()),
            _ => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn mtls_resolver(&self) -> Option<Arc<RotatingClientCertResolver>> {
        match &self.state {
            StartupState::Admitted { mtls_resolver, .. } => mtls_resolver.clone(),
            StartupState::Prepared(_) => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn replace_watcher_task(&mut self, task: JoinHandle<()>) {
        if let StartupState::Admitted {
            watcher: Some(handle),
            ..
        } = &mut self.state
        {
            handle.cancel.cancel();
            handle.task.abort();
            handle.task = task;
        }
    }

    pub async fn admit(
        &mut self,
        cancel: CancellationToken,
    ) -> Result<TemporalConnection, ConnectError> {
        describe_metrics();

        if let StartupState::Admitted { connection, .. } = &self.state {
            return Ok(connection.clone());
        }

        check_admission_gate(&self.startup_context, &cancel)?;

        let StartupState::Prepared(options) = &self.state else {
            return Err(ConnectError::InvalidConfig("invalid startup state"));
        };
        let options = (**options).clone();
        options.validate()?;

        let reader = Arc::new(Semaphore::new(1));
        let connect_budget = self.startup_context.bounded(options.connect_timeout);
        if let Err(stop) = connect_budget.check() {
            return Err(map_bounded_connect_stop(stop));
        }

        let initial_api_key =
            load_initial_api_key(&options.auth, &reader, &connect_budget, &cancel).await?;
        let (tls_options, mtls_watch_seed) =
            build_tls_setup(&options, &reader, &connect_budget, &cancel).await?;

        let proxy_options = options
            .http_proxy_url
            .as_ref()
            .map(proxy_options_from_url)
            .transpose()?;

        let meter = WorkspaceMetricsMeter::new(METRIC_PREFIX);
        let remaining_connect = connect_budget.remaining();
        if remaining_connect.is_zero() {
            return Err(ConnectError::TimedOut);
        }

        let conn_options = build_connection_options(
            &options,
            &meter,
            remaining_connect,
            tls_options,
            initial_api_key.as_ref(),
            proxy_options,
        );

        #[cfg(test)]
        let conn_options = {
            let mut conn_options = conn_options;
            if let Some(service_override) = self.service_override.clone() {
                conn_options.dns_load_balancing = None;
                conn_options.service_override = Some(service_override);
            }
            conn_options
        };
        let connection = run_connect_stage(
            async {
                tokio::time::timeout(remaining_connect, Connection::connect(conn_options))
                    .await
                    .map_err(|_| ConnectError::TimedOut)?
                    .map_err(map_client_connect_error)
            },
            &connect_budget,
            &cancel,
        )
        .await??;

        let (temporal_connection, state) = build_admitted_state(
            options,
            &connection,
            #[cfg(feature = "worker")]
            meter,
            initial_api_key,
            mtls_watch_seed,
            reader,
        )?;
        self.state = state;
        Ok(temporal_connection)
    }

    pub async fn close(self, deadline: Instant) -> CloseOutcome {
        #[allow(
            clippy::unnecessary_rest_pattern,
            reason = "mtls_resolver field exists only under cfg(test)"
        )]
        let StartupState::Admitted {
            connection,
            watcher,
            ..
        } = self.state
        else {
            return CloseOutcome::Complete;
        };
        connection.inner.draining.store(true, Ordering::Release);
        let Some(mut watcher) = watcher else {
            return CloseOutcome::Complete;
        };
        watcher.cancel.cancel();
        match tokio::time::timeout_at(deadline, &mut watcher.task).await {
            Ok(Ok(())) => CloseOutcome::Complete,
            Ok(Err(error)) => {
                let reason = if error.is_panic() {
                    "panicked"
                } else if error.is_cancelled() {
                    "cancelled"
                } else {
                    "join_error"
                };
                tracing::error!(reason, "temporal_credential_watcher_failed");
                CloseOutcome::Complete
            }
            Err(_) => {
                watcher.task.abort();
                CloseOutcome::TimedOut
            }
        }
    }
}

fn build_admitted_state(
    options: TemporalOptions,
    connection: &Connection,
    #[cfg(feature = "worker")] meter: WorkspaceMetricsMeter,
    initial_api_key: Option<InitialApiKey>,
    mtls_watch_seed: Option<MtlsWatchSeed>,
    reader: Arc<Semaphore>,
) -> Result<(TemporalConnection, StartupState), ConnectError> {
    let data_converter =
        build_data_converter(options.max_payload_bytes, options.payload_codec.clone());
    let trace_interceptor = TraceClientInterceptor::new(data_converter.clone());
    #[cfg(feature = "worker")]
    let worker_data_converter = data_converter.clone();
    let client_options = ClientOptions::new(options.namespace.clone())
        .data_converter(data_converter)
        .client_interceptors(vec![Arc::new(trace_interceptor)])
        .build();
    let raw_client = Client::new(connection.clone(), client_options)
        .map_err(|_| ConnectError::InvalidConfig("failed to construct temporal client"))?;

    let api_key_watch = initial_api_key.and_then(|key| {
        key.watch
            .map(|(path, digest)| ApiKeyWatch::new(path, digest, connection.clone()))
    });
    #[cfg(test)]
    let mtls_resolver = mtls_watch_seed
        .as_ref()
        .map(|seed| Arc::clone(&seed.resolver));
    let mtls_watch = mtls_watch_seed
        .map(|seed| MtlsWatch::new(seed.cert_path, seed.key_path, seed.digest, seed.resolver));
    let watcher = spawn_credential_watcher(
        CredentialWatcher::new(api_key_watch, mtls_watch, reader),
        options.credential_refresh_interval,
    );

    let temporal_connection = TemporalConnection {
        inner: Arc::new(ConnectionShared {
            raw_client,
            #[cfg(feature = "worker")]
            meter,
            #[cfg(feature = "worker")]
            data_converter: worker_data_converter,
            namespace: options.namespace,
            rpc_timeout: options.rpc_timeout,
            probe_budget: options.probe_budget,
            draining: Arc::new(AtomicBool::new(false)),
        }),
    };
    let state = StartupState::Admitted {
        connection: temporal_connection.clone(),
        watcher,
        #[cfg(test)]
        mtls_resolver,
    };
    Ok((temporal_connection, state))
}

fn spawn_credential_watcher(
    watcher: CredentialWatcher,
    interval: Duration,
) -> Option<WatcherHandle> {
    if !watcher.is_active() {
        return None;
    }
    let cancel = CancellationToken::new();
    #[cfg(test)]
    let (watcher, steps) = watcher.with_step_observer();
    let task = tokio::spawn(watcher.run(interval, cancel.clone()));
    Some(WatcherHandle {
        cancel,
        task,
        #[cfg(test)]
        steps,
    })
}

struct ConnectionShared {
    raw_client: Client,
    #[cfg(feature = "worker")]
    meter: WorkspaceMetricsMeter,
    #[cfg(feature = "worker")]
    data_converter: DataConverter,
    namespace: String,
    rpc_timeout: Duration,
    probe_budget: Duration,
    draining: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct TemporalConnection {
    inner: Arc<ConnectionShared>,
}

impl fmt::Debug for TemporalConnection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TemporalConnection")
            .field("namespace", &self.inner.namespace)
            .field("draining", &self.inner.draining.load(Ordering::Acquire))
            .finish()
    }
}

impl TemporalConnection {
    #[must_use]
    pub fn client(&self) -> TemporalClient {
        TemporalClient::new(self.inner.raw_client.clone(), self.inner.rpc_timeout)
    }

    #[must_use]
    pub fn probe(&self) -> TemporalProbe {
        TemporalProbe::new(
            self.inner.raw_client.clone(),
            self.inner.namespace.clone(),
            self.inner.probe_budget,
            Arc::clone(&self.inner.draining),
            None,
        )
    }

    #[cfg(feature = "worker")]
    #[must_use]
    pub(crate) fn raw_client(&self) -> Client {
        self.inner.raw_client.clone()
    }

    #[cfg(feature = "worker")]
    #[must_use]
    pub(crate) fn meter(&self) -> &WorkspaceMetricsMeter {
        &self.inner.meter
    }

    #[cfg(feature = "worker")]
    #[must_use]
    pub(crate) fn data_converter(&self) -> &DataConverter {
        &self.inner.data_converter
    }

    #[cfg(feature = "worker")]
    #[must_use]
    pub(crate) fn namespace(&self) -> &str {
        &self.inner.namespace
    }

    #[cfg(feature = "worker")]
    pub(crate) fn mark_draining(&self) {
        self.inner.draining.store(true, Ordering::Release);
    }

    #[cfg(feature = "worker")]
    #[must_use]
    pub(crate) fn probe_with_failure(&self, failed: Arc<AtomicBool>) -> TemporalProbe {
        TemporalProbe::new(
            self.inner.raw_client.clone(),
            self.inner.namespace.clone(),
            self.inner.probe_budget,
            Arc::clone(&self.inner.draining),
            Some(failed),
        )
    }
}

fn check_admission_gate(
    context: &OperationContext,
    cancel: &CancellationToken,
) -> Result<(), ConnectError> {
    if let Err(stopped) = context.check() {
        return Err(match stopped {
            Stopped::Deadline => ConnectError::TimedOut,
            Stopped::Cancelled => ConnectError::Cancelled,
        });
    }
    if cancel.is_cancelled() {
        return Err(ConnectError::Cancelled);
    }
    Ok(())
}

const fn map_bounded_connect_stop(stop: BoundedStop) -> ConnectError {
    match stop {
        BoundedStop::Parent(Stopped::Cancelled) => ConnectError::Cancelled,
        BoundedStop::Parent(Stopped::Deadline) | BoundedStop::Timeout => ConnectError::TimedOut,
    }
}

async fn run_connect_stage<F: Future>(
    future: F,
    bounded: &operation_context::BoundedContext,
    cancel: &CancellationToken,
) -> Result<F::Output, ConnectError> {
    tokio::select! {
        biased;
        stop = bounded.wait_stopped() => Err(map_bounded_connect_stop(stop)),
        () = cancel.cancelled() => Err(ConnectError::Cancelled),
        output = future => Ok(output),
    }
}

pub(crate) fn proxy_options_from_url(url: &Url) -> Result<HttpConnectProxyOptions, ConnectError> {
    if url.scheme() != "http" {
        return Err(ConnectError::InvalidConfig(
            "http_proxy_url scheme must be http",
        ));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || shared_rules::uri::authority_has_userinfo(url.as_str())
    {
        return Err(ConnectError::InvalidConfig(
            "http_proxy_url must not contain userinfo",
        ));
    }
    let host = url
        .host_str()
        .ok_or(ConnectError::InvalidConfig("http_proxy_url missing host"))?;
    let port = url
        .port_or_known_default()
        .ok_or(ConnectError::InvalidConfig("http_proxy_url missing port"))?;
    Ok(HttpConnectProxyOptions::new(format!("{host}:{port}")).build())
}

fn map_client_connect_error(error: ClientConnectError) -> ConnectError {
    match error {
        ClientConnectError::InvalidUri(_) | ClientConnectError::InvalidHeaders(_) => {
            tracing::warn!(reason = "invalid_config", "temporal_connect_failed");
            ConnectError::InvalidConfig("invalid client connection options")
        }
        ClientConnectError::InvalidConfig(_) => {
            tracing::warn!(reason = "tls_or_config", "temporal_connect_failed");
            ConnectError::TlsSetup("client tls or configuration rejected")
        }
        ClientConnectError::SystemInfoCallError(status) => {
            tracing::warn!(
                reason = "system_info_failed",
                grpc_code = ?status.code(),
                "temporal_connect_failed"
            );
            ConnectError::Transport
        }
        ClientConnectError::TonicTransportError(_)
        | ClientConnectError::DnsResolutionError { .. } => {
            tracing::warn!(reason = "transport", "temporal_connect_failed");
            ConnectError::Transport
        }
        _ => {
            tracing::warn!(reason = "unknown", "temporal_connect_failed");
            ConnectError::Transport
        }
    }
}
