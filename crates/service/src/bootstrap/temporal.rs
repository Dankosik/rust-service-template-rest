use std::path::Path;
use std::time::Duration;

use infra_temporal::{
    ConnectError, TemporalAuth, TemporalConnection, TemporalMtlsOptions, TemporalOptions,
    TemporalStartup, TemporalTlsOptions, TlsRotationMode, Zeroizing,
};
use operation_context::{Deadline, OperationContext};
use secrecy::ExposeSecret as _;
use service_config::{Config, TemporalClientConfig, TemporalTlsRotation};

use super::background::Background;

pub(super) fn options(temporal: &TemporalClientConfig, probe_budget: Duration) -> TemporalOptions {
    let auth = if let Some(api_key) = temporal.api_key() {
        TemporalAuth::ApiKey(Zeroizing::new(api_key.expose_secret().to_owned()))
    } else if let Some(path) = temporal.api_key_file() {
        TemporalAuth::ApiKeyFile(path.to_path_buf())
    } else {
        TemporalAuth::None
    };
    let mtls = match (temporal.client_cert_path(), temporal.client_key_path()) {
        (Some(client_cert_path), Some(client_key_path)) => Some(TemporalMtlsOptions {
            client_cert_path: client_cert_path.to_path_buf(),
            client_key_path: client_key_path.to_path_buf(),
            rotation: match temporal.mtls_rotation() {
                TemporalTlsRotation::Static => TlsRotationMode::Static,
                TemporalTlsRotation::Dynamic => TlsRotationMode::Dynamic,
            },
        }),
        _ => None,
    };
    let server_root_ca_path = temporal.root_ca_path().map(Path::to_path_buf);
    let tls = (server_root_ca_path.is_some() || mtls.is_some()).then_some(TemporalTlsOptions {
        server_root_ca_path,
        domain: None,
        mtls,
    });
    TemporalOptions {
        endpoint: temporal.endpoint().clone(),
        namespace: temporal.namespace().to_owned(),
        identity: temporal.identity().to_owned(),
        auth,
        tls,
        allow_plaintext: temporal.allow_plaintext(),
        allow_unauthenticated: temporal.allow_unauthenticated(),
        trusted_network: temporal.trusted_network(),
        credential_refresh_interval: temporal.credential_refresh_interval(),
        connect_timeout: temporal.connect_timeout(),
        rpc_timeout: temporal.rpc_timeout(),
        keep_alive_interval: temporal.keep_alive_interval(),
        keep_alive_timeout: temporal.keep_alive_timeout(),
        dns_load_balancing: temporal.dns_load_balancing(),
        http_proxy_url: temporal.http_proxy_url().cloned(),
        max_payload_bytes: temporal.max_payload_bytes(),
        probe_budget,
        payload_codec: None,
    }
}

pub(super) async fn open(
    temporal: &TemporalClientConfig,
    config: &Config,
    background: &Background,
    retained: &mut Option<TemporalStartup>,
) -> Result<TemporalConnection, ConnectError> {
    let options = options(temporal, config.health().probe_budget);
    let cancel = background.child_token();
    let context = OperationContext::new(
        Some(Deadline::after(options.connect_timeout)),
        cancel.clone(),
    );
    let connection = retained
        .insert(TemporalStartup::prepare(options, &context))
        .admit(cancel)
        .await?;
    tracing::info!(
        temporal.namespace = temporal.namespace(),
        temporal.readiness_critical = temporal.readiness_critical(),
        "temporal_connected"
    );
    Ok(connection)
}

#[cfg(test)]
#[utoipa::path(
    get,
    path = "/_test/temporal",
    operation_id = "testTemporalState",
    security(),
    extensions(("x-security-decision" = json!({
        "exposure": "public",
        "rationale": "test-only route verifying TemporalClient AppState wiring"
    }))),
    responses(
        (status = 200, description = "Temporal client configured"),
        (status = 204, description = "Temporal client inert")
    )
)]
async fn temporal_state(
    axum::extract::State(temporal): axum::extract::State<Option<infra_temporal::TemporalClient>>,
) -> axum::http::StatusCode {
    if temporal.is_some() {
        axum::http::StatusCode::OK
    } else {
        axum::http::StatusCode::NO_CONTENT
    }
}

#[cfg(test)]
pub(crate) fn test_route()
-> Result<infra_http::HttpContract<crate::AppState>, infra_http::FinalizeError> {
    infra_http::http_route!(temporal_state)
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::body::Body;
    use axum::http::Response;
    use health::Probe as _;
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::time::Instant;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::bootstrap::{
        BootstrapError, StartupOutcome, finish_startup, shutdown, signals::Signals, start,
        test_config, tests::shared_metrics,
    };

    struct LoopbackTemporalServer {
        addr: SocketAddr,
        describe_calls: Arc<AtomicUsize>,
        stop: CancellationToken,
        task: tokio::task::JoinHandle<()>,
    }

    impl LoopbackTemporalServer {
        async fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let describe_calls = Arc::new(AtomicUsize::new(0));
            let calls = Arc::clone(&describe_calls);
            let stop = CancellationToken::new();
            let server_stop = stop.clone();

            #[expect(
                clippy::disallowed_methods,
                reason = "loopback gRPC fixture serves Temporal WorkflowService RPCs directly over axum"
            )]
            let router = axum::Router::new()
                .route(
                    "/temporal.api.workflowservice.v1.WorkflowService/GetSystemInfo",
                    axum::routing::post(async || {
                        Response::builder()
                            .status(200)
                            .header("content-type", "application/grpc")
                            .header("grpc-status", "12")
                            .header("grpc-message", "unknown method GetSystemInfo")
                            .body(Body::empty())
                            .unwrap()
                    }),
                )
                .route(
                    "/temporal.api.workflowservice.v1.WorkflowService/DescribeNamespace",
                    axum::routing::post(move || {
                        let calls = Arc::clone(&calls);
                        async move {
                            calls.fetch_add(1, Ordering::Relaxed);
                            Response::builder()
                                .status(200)
                                .header("content-type", "application/grpc")
                                .header("grpc-status", "14")
                                .header("grpc-message", "namespace unavailable")
                                .body(Body::empty())
                                .unwrap()
                        }
                    }),
                );

            let task = tokio::spawn(async move {
                axum::serve(listener, router)
                    .with_graceful_shutdown(server_stop.cancelled_owned())
                    .await
                    .unwrap();
            });

            Self {
                addr,
                describe_calls,
                stop,
                task,
            }
        }

        fn endpoint(&self) -> String {
            format!("http://{}", self.addr)
        }

        fn describe_calls(&self) -> usize {
            self.describe_calls.load(Ordering::Relaxed)
        }

        async fn stop(self) {
            self.stop.cancel();
            self.task.await.unwrap();
        }
    }

    async fn http_status_line(address: SocketAddr, path: &str) -> String {
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut stream = TcpStream::connect(address).await.unwrap();
            let request =
                format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut status = String::new();
            BufReader::new(stream).read_line(&mut status).await.unwrap();
            status
        })
        .await
        .unwrap()
    }

    #[test]
    fn options_translation_preserves_plaintext_proxy_and_budgets() {
        let plaintext = test_config(&[
            ("APP__APP__ENV", "local"),
            ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
            ("APP__HEALTH__PROBE_BUDGET", "750ms"),
            ("APP__TEMPORAL__ENABLED", "true"),
            ("APP__TEMPORAL__ENDPOINT", "http://127.0.0.1:7233"),
            ("APP__TEMPORAL__ALLOW_PLAINTEXT", "true"),
            ("APP__TEMPORAL__NAMESPACE", "orders"),
            ("APP__TEMPORAL__IDENTITY", "orders-api@1"),
            ("APP__TEMPORAL__CONNECT_TIMEOUT", "3s"),
            ("APP__TEMPORAL__RPC_TIMEOUT", "7s"),
            ("APP__TEMPORAL__KEEP_ALIVE_INTERVAL", "20s"),
            ("APP__TEMPORAL__KEEP_ALIVE_TIMEOUT", "5s"),
            ("APP__TEMPORAL__CREDENTIAL_REFRESH_INTERVAL", "15s"),
            ("APP__TEMPORAL__DNS_LOAD_BALANCING", "false"),
            ("APP__TEMPORAL__HTTP_PROXY_URL", "http://127.0.0.1:8080"),
            ("APP__TEMPORAL__MAX_PAYLOAD_BYTES", "16KiB"),
            ("APP__TEMPORAL__READINESS_CRITICAL", "true"),
        ]);
        let cfg = plaintext.temporal().unwrap();
        assert!(cfg.readiness_critical());
        let opts = options(cfg, plaintext.health().probe_budget);
        assert_eq!(opts.endpoint.as_str(), "http://127.0.0.1:7233/");
        assert_eq!(opts.namespace, "orders");
        assert_eq!(opts.identity, "orders-api@1");
        std::assert_matches!(opts.auth, TemporalAuth::None);
        assert!(opts.tls.is_none());
        assert!(opts.allow_plaintext);
        assert!(!opts.allow_unauthenticated);
        assert!(!opts.trusted_network);
        assert_eq!(opts.credential_refresh_interval, Duration::from_secs(15));
        assert_eq!(opts.connect_timeout, Duration::from_secs(3));
        assert_eq!(opts.rpc_timeout, Duration::from_secs(7));
        assert_eq!(opts.keep_alive_interval, Duration::from_secs(20));
        assert_eq!(opts.keep_alive_timeout, Duration::from_secs(5));
        assert!(!opts.dns_load_balancing);
        assert_eq!(
            opts.http_proxy_url.as_ref().map(AsRef::as_ref),
            Some("http://127.0.0.1:8080/")
        );
        assert_eq!(opts.max_payload_bytes, 16 * 1024);
        assert_eq!(opts.probe_budget, Duration::from_millis(750));
        assert!(opts.payload_codec.is_none());
    }

    #[test]
    fn options_translation_maps_https_auth_variants_and_tls_rotation_modes() {
        let https_api_key = test_config(&[
            ("APP__TEMPORAL__ENABLED", "true"),
            (
                "APP__TEMPORAL__ENDPOINT",
                "https://temporal.example.com:7233",
            ),
            ("APP__TEMPORAL__API_KEY", "inline-bearer-token"),
        ]);
        let cfg = https_api_key.temporal().unwrap();
        assert!(!cfg.readiness_critical());
        let opts = options(cfg, https_api_key.health().probe_budget);
        assert!(opts.tls.is_none());
        std::assert_matches!(
            opts.auth,
            TemporalAuth::ApiKey(ref secret) if secret.as_str() == "inline-bearer-token"
        );

        let root_ca_only = test_config(&[
            ("APP__TEMPORAL__ENABLED", "true"),
            (
                "APP__TEMPORAL__ENDPOINT",
                "https://temporal.example.com:7233",
            ),
            ("APP__TEMPORAL__API_KEY_FILE", "/run/secrets/temporal-key"),
            ("APP__TEMPORAL__ROOT_CA_PATH", "/etc/ssl/temporal-ca.pem"),
        ]);
        let opts = options(
            root_ca_only.temporal().unwrap(),
            root_ca_only.health().probe_budget,
        );
        std::assert_matches!(
            opts.auth,
            TemporalAuth::ApiKeyFile(ref path) if path == Path::new("/run/secrets/temporal-key")
        );
        let tls = opts
            .tls
            .expect("root_ca_path configures TemporalTlsOptions");
        assert_eq!(
            tls.server_root_ca_path.as_deref(),
            Some(Path::new("/etc/ssl/temporal-ca.pem"))
        );
        assert!(tls.domain.is_none());
        assert!(tls.mtls.is_none());

        let static_mtls = test_config(&[
            ("APP__TEMPORAL__ENABLED", "true"),
            (
                "APP__TEMPORAL__ENDPOINT",
                "https://temporal.example.com:7233",
            ),
            ("APP__TEMPORAL__CLIENT_CERT_PATH", "/run/certs/client.pem"),
            ("APP__TEMPORAL__CLIENT_KEY_PATH", "/run/certs/client.key"),
            ("APP__TEMPORAL__MTLS_ROTATION", "static"),
        ]);
        let opts = options(
            static_mtls.temporal().unwrap(),
            static_mtls.health().probe_budget,
        );
        std::assert_matches!(opts.auth, TemporalAuth::None);
        let mtls = opts
            .tls
            .expect("static mTLS configures TemporalTlsOptions")
            .mtls
            .expect("mTLS options");
        assert_eq!(mtls.client_cert_path, Path::new("/run/certs/client.pem"));
        assert_eq!(mtls.client_key_path, Path::new("/run/certs/client.key"));
        assert_eq!(mtls.rotation, TlsRotationMode::Static);

        let dynamic_mtls = test_config(&[
            ("APP__TEMPORAL__ENABLED", "true"),
            (
                "APP__TEMPORAL__ENDPOINT",
                "https://temporal.example.com:7233",
            ),
            ("APP__TEMPORAL__CLIENT_CERT_PATH", "/run/certs/client.pem"),
            ("APP__TEMPORAL__CLIENT_KEY_PATH", "/run/certs/client.key"),
            ("APP__TEMPORAL__MTLS_ROTATION", "dynamic"),
            ("APP__TEMPORAL__DNS_LOAD_BALANCING", "false"),
        ]);
        let opts = options(
            dynamic_mtls.temporal().unwrap(),
            dynamic_mtls.health().probe_budget,
        );
        let mtls = opts
            .tls
            .expect("dynamic mTLS configures TemporalTlsOptions")
            .mtls
            .expect("mTLS options");
        assert_eq!(mtls.rotation, TlsRotationMode::Dynamic);
    }

    #[tokio::test]
    async fn inert_temporal_disabled_performs_zero_dials_and_exposes_none_state_with_ready_200() {
        let canary = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let canary_endpoint = format!("http://{}", canary.local_addr().unwrap());
        let config = test_config(&[
            ("APP__APP__ENV", "local"),
            ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
            ("APP__HTTP__ADDR", "127.0.0.1:0"),
            ("APP__HTTP__READINESS_PROPAGATION_DELAY", "0s"),
            ("APP__OBSERVABILITY__METRICS__ADDR", ""),
            ("APP__TEMPORAL__ENABLED", "false"),
            ("APP__TEMPORAL__ENDPOINT", &canary_endpoint),
            ("APP__TEMPORAL__ALLOW_PLAINTEXT", "true"),
            ("APP__TEMPORAL__READINESS_CRITICAL", "true"),
        ]);
        assert!(config.temporal().is_none());

        let mut background = Background::new(CancellationToken::new());
        let mut dependencies = shutdown::Dependencies::default();
        let mut serving = shutdown::Serving::default();
        let prepared = start(
            &config,
            crate::ServiceRegistration::default(),
            &shared_metrics(),
            &mut background,
            &mut dependencies,
            &mut serving,
        )
        .await
        .unwrap();
        assert!(dependencies.temporal.is_none());

        let mut signals = Signals::install().unwrap();
        assert_eq!(
            finish_startup(
                Ok(Some(prepared)),
                &mut signals,
                &mut background,
                &mut serving,
            )
            .unwrap(),
            StartupOutcome::Admitted
        );
        let address = serving.traffic.http.as_ref().unwrap().local_addr();
        assert!(
            http_status_line(address, "/health/ready")
                .await
                .starts_with("HTTP/1.1 200 ")
        );
        assert!(
            http_status_line(address, "/_test/temporal")
                .await
                .starts_with("HTTP/1.1 204 ")
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), canary.accept())
                .await
                .is_err(),
            "inert Temporal configuration must never dial its endpoint"
        );

        let outcome = shutdown::run(shutdown::Plan {
            http_config: config.http(),
            signals: &mut signals,
            deadline: Instant::now() + config.http().grace_period,
            serving,
            background,
            dependencies,
            telemetry: shutdown::Telemetry::default(),
        })
        .await;
        assert_eq!(outcome, shutdown::Outcome::Graceful);
    }

    #[tokio::test]
    async fn non_critical_temporal_wires_client_skips_readiness_probe_and_drains_on_shutdown() {
        let server = LoopbackTemporalServer::start().await;
        let endpoint = server.endpoint();
        let config = test_config(&[
            ("APP__APP__ENV", "local"),
            ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
            ("APP__HTTP__ADDR", "127.0.0.1:0"),
            ("APP__HTTP__READINESS_PROPAGATION_DELAY", "0s"),
            ("APP__OBSERVABILITY__METRICS__ADDR", ""),
            ("APP__TEMPORAL__ENABLED", "true"),
            ("APP__TEMPORAL__ENDPOINT", &endpoint),
            ("APP__TEMPORAL__ALLOW_PLAINTEXT", "true"),
            ("APP__TEMPORAL__READINESS_CRITICAL", "false"),
        ]);
        let metrics = shared_metrics();
        let mut background = Background::new(CancellationToken::new());
        let mut dependencies = shutdown::Dependencies::default();
        let mut serving = shutdown::Serving::default();
        let prepared = start(
            &config,
            crate::ServiceRegistration::default(),
            &metrics,
            &mut background,
            &mut dependencies,
            &mut serving,
        )
        .await
        .unwrap();
        assert!(dependencies.temporal.is_some());
        assert_eq!(server.describe_calls(), 0);

        let mut signals = Signals::install().unwrap();
        assert_eq!(
            finish_startup(
                Ok(Some(prepared)),
                &mut signals,
                &mut background,
                &mut serving,
            )
            .unwrap(),
            StartupOutcome::Admitted
        );
        let address = serving.traffic.http.as_ref().unwrap().local_addr();
        assert!(
            http_status_line(address, "/health/ready")
                .await
                .starts_with("HTTP/1.1 200 ")
        );
        assert!(
            http_status_line(address, "/_test/temporal")
                .await
                .starts_with("HTTP/1.1 200 ")
        );
        assert_eq!(server.describe_calls(), 0);
        assert!(
            metrics.render().contains("temporal_request_latency_count"),
            "Temporal client metrics must emit through the shared Prometheus recorder"
        );

        let outcome = shutdown::run(shutdown::Plan {
            http_config: config.http(),
            signals: &mut signals,
            deadline: Instant::now() + config.http().grace_period,
            serving,
            background,
            dependencies,
            telemetry: shutdown::Telemetry::default(),
        })
        .await;
        assert_eq!(outcome, shutdown::Outcome::Graceful);
        server.stop().await;
    }

    #[tokio::test]
    async fn readiness_critical_temporal_registers_probe_and_blocks_startup_on_probe_failure() {
        let server = LoopbackTemporalServer::start().await;
        let endpoint = server.endpoint();
        let config = test_config(&[
            ("APP__APP__ENV", "local"),
            ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
            ("APP__HTTP__ADDR", "127.0.0.1:0"),
            ("APP__HTTP__READINESS_PROPAGATION_DELAY", "0s"),
            ("APP__OBSERVABILITY__METRICS__ADDR", ""),
            ("APP__TEMPORAL__ENABLED", "true"),
            ("APP__TEMPORAL__ENDPOINT", &endpoint),
            ("APP__TEMPORAL__ALLOW_PLAINTEXT", "true"),
            ("APP__TEMPORAL__READINESS_CRITICAL", "true"),
        ]);
        let mut background = Background::new(CancellationToken::new());
        let mut dependencies = shutdown::Dependencies::default();
        let mut serving = shutdown::Serving::default();
        let err = start(
            &config,
            crate::ServiceRegistration::default(),
            &shared_metrics(),
            &mut background,
            &mut dependencies,
            &mut serving,
        )
        .await
        .unwrap_err();
        std::assert_matches!(
            err,
            BootstrapError::Admission(health::NotReady::ProbeFailed {
                probe: "temporal",
                ..
            })
        );
        assert!(server.describe_calls() >= 1);
        assert!(dependencies.temporal.is_some());
        assert!(serving.traffic.http.is_none());

        let mut signals = Signals::install().unwrap();
        let outcome = shutdown::run(shutdown::Plan {
            http_config: config.http(),
            signals: &mut signals,
            deadline: Instant::now() + config.http().grace_period,
            serving,
            background,
            dependencies,
            telemetry: shutdown::Telemetry::default(),
        })
        .await;
        assert_eq!(outcome, shutdown::Outcome::Graceful);
        server.stop().await;
    }

    #[tokio::test]
    async fn open_inserts_startup_before_admit_and_shutdown_closes_admitted_and_cancelled_handles()
    {
        let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let silent_endpoint = format!("http://{}", silent.local_addr().unwrap());
        let silent_config = test_config(&[
            ("APP__APP__ENV", "local"),
            ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
            ("APP__TEMPORAL__ENABLED", "true"),
            ("APP__TEMPORAL__ENDPOINT", &silent_endpoint),
            ("APP__TEMPORAL__ALLOW_PLAINTEXT", "true"),
            ("APP__TEMPORAL__CONNECT_TIMEOUT", "10s"),
        ]);
        let background = Background::new(CancellationToken::new());
        let mut dependencies = shutdown::Dependencies::default();
        let peer = {
            let admit = open(
                silent_config.temporal().unwrap(),
                &silent_config,
                &background,
                &mut dependencies.temporal,
            );
            tokio::select! {
                result = admit => panic!("silent peer must not complete Temporal admission: {result:?}"),
                accepted = tokio::time::timeout(Duration::from_secs(5), silent.accept()) => {
                    accepted.unwrap().unwrap().0
                }
            }
        };
        assert!(
            dependencies.temporal.is_some(),
            "TemporalStartup must be stored in Dependencies before admit completes"
        );
        let mut signals = Signals::install().unwrap();
        let outcome = shutdown::run(shutdown::Plan {
            http_config: silent_config.http(),
            signals: &mut signals,
            deadline: Instant::now() + silent_config.http().grace_period,
            serving: shutdown::Serving::default(),
            background,
            dependencies,
            telemetry: shutdown::Telemetry::default(),
        })
        .await;
        assert_eq!(outcome, shutdown::Outcome::Graceful);
        drop(peer);
        drop(silent);

        let server = LoopbackTemporalServer::start().await;
        let endpoint = server.endpoint();
        let admitted_config = test_config(&[
            ("APP__APP__ENV", "local"),
            ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
            ("APP__TEMPORAL__ENABLED", "true"),
            ("APP__TEMPORAL__ENDPOINT", &endpoint),
            ("APP__TEMPORAL__ALLOW_PLAINTEXT", "true"),
        ]);
        let background = Background::new(CancellationToken::new());
        let mut dependencies = shutdown::Dependencies::default();
        let connection = open(
            admitted_config.temporal().unwrap(),
            &admitted_config,
            &background,
            &mut dependencies.temporal,
        )
        .await
        .unwrap();
        let probe = connection.probe();
        assert!(dependencies.temporal.is_some());

        let outcome = shutdown::run(shutdown::Plan {
            http_config: admitted_config.http(),
            signals: &mut signals,
            deadline: Instant::now() + admitted_config.http().grace_period,
            serving: shutdown::Serving::default(),
            background,
            dependencies,
            telemetry: shutdown::Telemetry::default(),
        })
        .await;
        assert_eq!(outcome, shutdown::Outcome::Graceful);
        let probe_err = probe.check().await.unwrap_err().to_string();
        assert!(
            probe_err.contains("draining"),
            "TemporalStartup::close must mark the connection draining: {probe_err}"
        );
        assert_eq!(server.describe_calls(), 0);
        server.stop().await;
    }

    #[tokio::test]
    async fn temporal_connect_timeout_fails_startup_before_binding_listeners_and_retains_handle() {
        let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", silent.local_addr().unwrap());
        let config = test_config(&[
            ("APP__APP__ENV", "local"),
            ("APP__APP__ALLOW_INSECURE_DEVELOPMENT", "true"),
            ("APP__HTTP__ADDR", "127.0.0.1:0"),
            ("APP__HTTP__READINESS_PROPAGATION_DELAY", "0s"),
            ("APP__OBSERVABILITY__METRICS__ADDR", ""),
            ("APP__TEMPORAL__ENABLED", "true"),
            ("APP__TEMPORAL__ENDPOINT", &endpoint),
            ("APP__TEMPORAL__ALLOW_PLAINTEXT", "true"),
            ("APP__TEMPORAL__CONNECT_TIMEOUT", "100ms"),
        ]);
        let accept = tokio::spawn(async move {
            let _peer = silent.accept().await.ok();
            tokio::time::sleep(Duration::from_secs(5)).await;
        });
        let mut background = Background::new(CancellationToken::new());
        let mut dependencies = shutdown::Dependencies::default();
        let mut serving = shutdown::Serving::default();
        let err = start(
            &config,
            crate::ServiceRegistration::default(),
            &shared_metrics(),
            &mut background,
            &mut dependencies,
            &mut serving,
        )
        .await
        .unwrap_err();
        std::assert_matches!(err, BootstrapError::Temporal(ConnectError::TimedOut));
        assert!(dependencies.temporal.is_some());
        assert!(serving.traffic.http.is_none());

        let mut signals = Signals::install().unwrap();
        let outcome = shutdown::run(shutdown::Plan {
            http_config: config.http(),
            signals: &mut signals,
            deadline: Instant::now() + config.http().grace_period,
            serving,
            background,
            dependencies,
            telemetry: shutdown::Telemetry::default(),
        })
        .await;
        assert_eq!(outcome, shutdown::Outcome::Graceful);
        accept.abort();
    }
}
