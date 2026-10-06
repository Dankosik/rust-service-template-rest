//! Two separately owned adapter compositions sharing one real PostgreSQL.
//! This observes consumer recovery, not separate OS schedulers or process exit.

use std::{net::SocketAddr, num::NonZeroU32, time::Duration};

use axum::{extract::State, http::StatusCode, response::IntoResponse as _, routing::get};
use health::{Readiness, RefreshPolicy};
use infra_http::{Drained, HardenOptions, Server, ServerOptions};
use infra_postgres::{PgPool, PostgresProbe};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    sync::watch,
    task::JoinHandle,
    time::{Instant, timeout},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tracing::instrument::WithSubscriber as _;

use super::{PoolEvents, dsn_at, dsn_for, server_address, template_pool};

// template:begin postgres-grpc-auth:recovery-auth-tls
#[path = "../../fixtures/tls.rs"]
mod tls;
// template:end postgres-grpc-auth:recovery-auth-tls

const WAIT: Duration = Duration::from_secs(25);
const CALL: Duration = Duration::from_secs(6);
const HTTP_CONNECTIONS: u32 = 4;
const POLICY: RefreshPolicy = RefreshPolicy {
    interval: Duration::from_secs(2),
    probe_budget: Duration::from_secs(4),
    failure_threshold: 3,
};

// An independently written row makes a successful response distinguish fresh
// dependency work from a cached ready verdict or a canned Echo response.
async fn value(pool: &PgPool) -> Result<String, sqlx::Error> {
    let mut connection = infra_postgres::acquire(pool, "recovery consumer").await?;
    sqlx::query_scalar("SELECT value FROM recovery_value")
        .fetch_one(&mut *connection)
        .await
}

async fn http_value(State(pool): State<PgPool>) -> axum::response::Response {
    match value(&pool).await {
        Ok(value) => value.into_response(),
        Err(_) => infra_http::Problem::new(infra_http::Code::ServiceUnavailable).into_response(),
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "non-shipped fixture operation; platform probes retain their canonical contract"
)]
fn http_router(pool: PgPool, readiness: &Readiness) -> axum::Router {
    let probes = infra_http::finalize_public(infra_http::router())
        .unwrap()
        .with_state(readiness.reader());
    let work = axum::Router::new()
        .route("/useful", get(http_value))
        .with_state(pool);
    infra_http::harden(
        probes.merge(work),
        &HardenOptions {
            max_body_bytes: 1024 * 1024,
            request_timeout: Duration::from_secs(8),
            max_in_flight: NonZeroU32::new(256),
            log_health_probes: false,
        },
    )
}

struct Instance {
    pool: PgPool,
    readiness: Readiness,
    cancel: CancellationToken,
    refresh: JoinHandle<()>,
    server: Server,
    client: reqwest::Client,
    address: SocketAddr,
    events: PoolEvents,
    // template:begin postgres-grpc-consumers:recovery-instance-grpc
    grpc: grpc::Consumer,
    // template:end postgres-grpc-consumers:recovery-instance-grpc
}

impl Instance {
    async fn start(pool: PgPool) -> Self {
        let readiness = Readiness::new(vec![Box::new(PostgresProbe::new(pool.clone()))], POLICY);
        readiness.refresh().await;
        assert!(readiness.reader().verdict().is_ok());
        let cancel = CancellationToken::new();
        let events = PoolEvents::default();
        let refresh = tokio::spawn({
            let readiness = readiness.clone();
            let cancel = cancel.clone();
            async move { readiness.refresh_until(cancel).await }.with_subscriber(events.clone())
        });
        // Diagnostics are absent. A small fixed application cap makes admission
        // pressure observable without multiplying thousands of sockets.
        let server = Server::bind(
            "127.0.0.1:0".parse().unwrap(),
            http_router(pool.clone(), &readiness),
            ServerOptions {
                header_read_timeout: Duration::from_secs(5),
                max_header_bytes: 16 * 1024,
                max_connections: NonZeroU32::new(HTTP_CONNECTIONS),
                max_connection_age: None,
            },
        )
        .await
        .unwrap();
        let address = server.local_addr();
        // template:begin postgres-grpc-consumers:recovery-start-grpc
        let grpc = grpc::Consumer::start(pool.clone(), readiness.reader()).await;
        // template:end postgres-grpc-consumers:recovery-start-grpc
        Self {
            pool,
            readiness,
            cancel,
            refresh,
            server,
            address,
            events,
            client: reqwest::Client::builder()
                .timeout(CALL)
                .pool_max_idle_per_host(0)
                .build()
                .unwrap(),
            // template:begin postgres-grpc-consumers:recovery-grpc-value
            grpc,
            // template:end postgres-grpc-consumers:recovery-grpc-value
        }
    }

    async fn get(&self, path: &str) -> reqwest::Response {
        self.client
            .get(format!("http://{}{path}", self.address))
            .send()
            .await
            .unwrap()
    }

    async fn useful(&mut self, expected: &str) {
        let response = self.get("/useful").await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), expected);
        // template:begin postgres-grpc-consumers:recovery-useful-grpc
        self.grpc.useful(expected).await;
        // template:end postgres-grpc-consumers:recovery-useful-grpc
    }

    #[expect(
        clippy::print_stdout,
        reason = "print the observed bounded failure class with --nocapture"
    )]
    async fn state(&mut self, ready: bool) {
        timeout(WAIT, async {
            loop {
                let response = self.get("/health/ready").await;
                let status = response.status();
                let _ = response.bytes().await.unwrap();
                assert!(matches!(
                    status,
                    StatusCode::OK | StatusCode::SERVICE_UNAVAILABLE
                ));
                if (status == StatusCode::OK) == ready {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("HTTP readiness reaches the expected state");
        // template:begin postgres-grpc-consumers:recovery-state-grpc
        self.grpc.state(ready).await;
        // template:end postgres-grpc-consumers:recovery-state-grpc
        if !ready {
            let verdict = self.readiness.reader().verdict();
            println!(
                "instance {}: completed dependency failure {verdict:?}",
                self.address
            );
            assert!(
                matches!(
                    verdict,
                    Err(health::NotReady::ProbeFailed {
                        probe: "postgres",
                        ..
                    })
                ),
                "{verdict:?}"
            );
        }
    }

    async fn refused_work(&mut self) {
        let response = self.get("/useful").await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let problem: serde_json::Value = response.json().await.unwrap();
        assert_eq!(problem["code"], "service_unavailable");
        // template:begin postgres-grpc-consumers:recovery-refused-grpc
        self.grpc.refused_work().await;
        // template:end postgres-grpc-consumers:recovery-refused-grpc
    }

    async fn admission_release(&mut self, expected: &str) {
        let mut connections = Vec::new();
        for _ in 0..HTTP_CONNECTIONS {
            let mut held = TcpStream::connect(self.address).await.unwrap();
            held.write_all(b"GET /health/live HTTP/1.1\r\nHost: localhost\r\n\r\n")
                .await
                .unwrap();
            let mut received = Vec::new();
            timeout(CALL, async {
                loop {
                    let mut byte = [0];
                    held.read_exact(&mut byte).await.unwrap();
                    received.push(byte[0]);
                    if received.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            assert!(
                received.starts_with(b"HTTP/1.1 200"),
                "held connection was admitted"
            );
            connections.push(held);
        }
        // An actual response on the held socket establishes admission before
        // these new sockets compete; there is no sleep-based assumption.
        for path in ["/health/live", "/health/ready", "/useful"] {
            assert!(
                self.client
                    .get(format!("http://{}{path}", self.address))
                    .timeout(Duration::from_secs(1))
                    .send()
                    .await
                    .is_err(),
                "{path} must share the exhausted application cap"
            );
        }
        drop(connections);
        // Socket teardown is asynchronous. Wait for admission, then require
        // the independently updated row through the same listener and pool.
        timeout(CALL, async {
            loop {
                if let Ok(response) = self
                    .client
                    .get(format!("http://{}/health/live", self.address))
                    .send()
                    .await
                {
                    assert_eq!(response.status(), StatusCode::OK);
                    let _ = response.bytes().await.unwrap();
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        self.useful(expected).await;
        self.state(true).await;
    }

    async fn stop(self) {
        self.readiness.start_drain();
        // template:begin postgres-grpc-consumers:recovery-stop-grpc
        self.grpc.stop().await;
        // template:end postgres-grpc-consumers:recovery-stop-grpc
        self.cancel.cancel();
        timeout(CALL, self.refresh).await.unwrap().unwrap();
        drop(self.client);
        assert_eq!(
            timeout(CALL, self.server.drain(Duration::from_secs(3)))
                .await
                .unwrap()
                .unwrap(),
            Drained::Complete
        );
        assert_eq!(self.pool.options().get_max_connections(), 1);
        timeout(CALL, self.pool.close()).await.unwrap();
    }
}

// The current commit relay owns one-shot transaction faults; this fixture needs
// a reversible interruption shared by *all* connections of both instance pools.
// It carries bytes unchanged and has no PostgreSQL protocol or outcome policy.
struct DependencyLink {
    address: SocketAddr,
    available: watch::Sender<bool>,
    cancel: CancellationToken,
    tasks: TaskTracker,
}

impl DependencyLink {
    async fn start(upstream: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (available, initial) = watch::channel(true);
        let cancel = CancellationToken::new();
        let tasks = TaskTracker::new();
        tasks.spawn({
            let cancel = cancel.clone();
            let tasks = tasks.clone();
            async move {
                loop {
                    let accepted = tokio::select! {
                        () = cancel.cancelled() => break,
                        result = listener.accept() => result.unwrap(),
                    };
                    if !*initial.borrow() { continue; }
                    let mut online = initial.clone();
                    let cancel = cancel.clone();
                    tasks.spawn(async move {
                        let (mut client, _) = accepted;
                        tokio::select! {
                            () = cancel.cancelled() => {},
                            _ = online.wait_for(|available| !*available) => {},
                            () = async {
                                if let Ok(mut server) = TcpStream::connect(upstream).await {
                                    let _ = tokio::io::copy_bidirectional(&mut client, &mut server).await;
                                }
                            } => {},
                        }
                    });
                }
            }
        });
        Self {
            address,
            available,
            cancel,
            tasks,
        }
    }

    fn interrupt(&self, interrupted: bool) {
        self.available.send_replace(!interrupted);
    }

    async fn stop(self) {
        self.cancel.cancel();
        self.tasks.close();
        timeout(CALL, self.tasks.wait()).await.unwrap();
    }
}

impl Drop for DependencyLink {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

async fn publish(pool: &PgPool, expected: &str) {
    sqlx::query("UPDATE recovery_value SET value = $1")
        .bind(expected)
        .execute(pool)
        .await
        .unwrap();
}

#[expect(
    clippy::disallowed_methods,
    clippy::print_stdout,
    reason = "hold raw pool capacity and print observed recovery timing with --nocapture"
)]
#[sqlx::test(migrations = false)]
async fn consumers_recover_from_local_pressure_shared_interruption_and_admission(pool: PgPool) {
    sqlx::query("CREATE TABLE recovery_value (value text NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO recovery_value VALUES ('initial')")
        .execute(&pool)
        .await
        .unwrap();
    let direct = dsn_for(&pool).await;
    let link = DependencyLink::start(server_address(&direct).await).await;
    let dsn = dsn_at(&pool, link.address).await;
    let mut a = Instance::start(template_pool(&dsn, 1).await).await;
    let mut b = Instance::start(template_pool(&dsn, 1).await).await;
    a.state(true).await;
    b.state(true).await;
    a.useful("initial").await;
    b.useful("initial").await;

    let mut held = a.pool.acquire().await.unwrap();
    let started = Instant::now();
    a.state(false).await;
    // The dependency itself remains responsive through A's held pool slot.
    assert_eq!(
        sqlx::query_scalar::<_, i32>("SELECT 7")
            .fetch_one(&mut *held)
            .await
            .unwrap(),
        7
    );
    a.refused_work().await;
    b.state(true).await;
    publish(&pool, "while-a-saturated").await;
    b.useful("while-a-saturated").await;
    let events = a.events.take();
    assert!(events.iter().any(|event| {
        event
            .get("message")
            .is_some_and(|message| message == "postgres_pool_acquire_timeout")
    }));
    println!(
        "A-local responsive pool pressure: A lost readiness after {:?}; B stayed ready and read new data",
        started.elapsed()
    );
    drop(held);
    publish(&pool, "after-pool-release").await;
    a.state(true).await;
    a.useful("after-pool-release").await;
    b.useful("after-pool-release").await;

    let started = Instant::now();
    link.interrupt(true);
    tokio::join!(a.state(false), b.state(false));
    tokio::join!(a.refused_work(), b.refused_work());
    // Keep failing rounds running beyond the freshness bound. The consumer
    // result must stay an ordinary completed PostgreSQL failure, never stale.
    tokio::time::sleep(POLICY.stale_after() + Duration::from_secs(1)).await;
    tokio::join!(a.state(false), b.state(false));
    println!(
        "common dependency-path interruption: both lost readiness, bounded useful calls failed, failing rounds stayed fresh for {:?}",
        started.elapsed()
    );
    publish(&pool, "after-dependency-release").await;
    link.interrupt(false);
    tokio::join!(a.state(true), b.state(true));
    a.useful("after-dependency-release").await;
    b.useful("after-dependency-release").await;

    publish(&pool, "after-connection-release").await;
    a.admission_release("after-connection-release").await;
    b.state(true).await;
    b.useful("after-connection-release").await;
    println!(
        "same compositions recovered fresh HTTP/Echo work and readiness; no-diagnostics admission recovered at connection cap={HTTP_CONNECTIONS} and pool max=1"
    );
    a.stop().await;
    b.stop().await;
    link.stop().await;
}

// template:begin postgres-grpc-consumers:recovery-grpc-module-1
mod grpc {
    use super::*;
    use grpc_contracts::example::v1::{
        BidiStreamRequest, BidiStreamResponse, ClientStreamRequest, ClientStreamResponse,
        ServerStreamRequest, ServerStreamResponse, UnaryRequest, UnaryResponse,
        echo_service_client::EchoServiceClient,
        echo_service_server::{EchoService, EchoServiceServer},
    };
    use infra_grpc::{Client, ClientSecurity, ClientTimeout, Limits, Services};
    use std::pin::Pin;
    use tonic::{Request, Response, Status};
    use tonic_health::pb::{
        HealthCheckRequest, HealthCheckResponse, health_check_response::ServingStatus,
        health_client::HealthClient,
    };

    struct DatabaseEcho(PgPool);
    type Output<T> = Pin<Box<dyn futures_util::Stream<Item = Result<T, Status>> + Send>>;

    #[tonic::async_trait]
    impl EchoService for DatabaseEcho {
        type ServerStreamStream = Output<ServerStreamResponse>;
        type BidiStreamStream = Output<BidiStreamResponse>;

        async fn unary(
            &self,
            _request: Request<UnaryRequest>,
        ) -> Result<Response<UnaryResponse>, Status> {
            let message = value(&self.0)
                .await
                .map_err(|_| Status::unavailable("dependency unavailable"))?;
            Ok(Response::new(UnaryResponse { message }))
        }
        async fn client_stream(
            &self,
            _: Request<tonic::Streaming<ClientStreamRequest>>,
        ) -> Result<Response<ClientStreamResponse>, Status> {
            Err(Status::unimplemented("unary fixture"))
        }
        async fn server_stream(
            &self,
            _: Request<ServerStreamRequest>,
        ) -> Result<Response<Self::ServerStreamStream>, Status> {
            Err(Status::unimplemented("unary fixture"))
        }
        async fn bidi_stream(
            &self,
            _: Request<tonic::Streaming<BidiStreamRequest>>,
        ) -> Result<Response<Self::BidiStreamStream>, Status> {
            Err(Status::unimplemented("unary fixture"))
        }
    }

    pub(super) struct Consumer {
        server: Server,
        echo: EchoServiceClient<Client>,
        health: HealthClient<Client>,
        watch: tonic::Streaming<HealthCheckResponse>,
        observed: Option<ServingStatus>,
        // template:end postgres-grpc-consumers:recovery-grpc-module-1
        // template:begin postgres-grpc-auth:recovery-provider-field
        provider: Server,
        provider_cancel: CancellationToken,
        // template:end postgres-grpc-auth:recovery-provider-field
        // template:begin postgres-grpc-consumers:recovery-grpc-module-2
    }

    fn health_request() -> HealthCheckRequest {
        HealthCheckRequest {
            service: "example.v1.EchoService".to_owned(),
        }
    }

    fn work_request() -> Request<UnaryRequest> {
        #[allow(unused_mut, reason = "authentication profile adds metadata")]
        let mut request = Request::new(UnaryRequest {
            message: "read fresh database value".to_owned(),
        });
        // template:end postgres-grpc-consumers:recovery-grpc-module-2
        // template:begin postgres-grpc-auth:recovery-work-token
        request
            .metadata_mut()
            .insert("authorization", "Bearer accepted".parse().unwrap());
        // template:end postgres-grpc-auth:recovery-work-token
        // template:begin postgres-grpc-consumers:recovery-grpc-module-3
        request
    }

    impl Consumer {
        pub(super) async fn start(pool: PgPool, reader: health::ReadinessReader) -> Self {
            // template:end postgres-grpc-consumers:recovery-grpc-module-3
            // template:begin postgres-grpc-auth:recovery-start-provider
            let (verifier, provider, provider_cancel) = provider().await;
            // template:end postgres-grpc-auth:recovery-start-provider
            // template:begin postgres-grpc-consumers:recovery-grpc-module-4
            let mut services = Services::new();
            services
                .describe(grpc_contracts::FILE_DESCRIPTOR_SET)
                .unwrap();
            services
                .add(EchoServiceServer::new(DatabaseEcho(pool)))
                .unwrap();
            let limits = Limits {
                request_timeout: Duration::from_secs(8),
                max_in_flight: NonZeroU32::new(256),
                max_connections: NonZeroU32::new(4096),
                max_connection_age: None,
            };
            let router = infra_grpc::router(
                services, reader,
                // template:end postgres-grpc-consumers:recovery-grpc-module-4
                // template:begin postgres-grpc-auth:recovery-router-verifier
                verifier,
                // template:end postgres-grpc-auth:recovery-router-verifier
                // template:begin postgres-grpc-consumers:recovery-grpc-router-tail
                limits,
            )
            .unwrap();
            let server = Server::bind(
                "127.0.0.1:0".parse().unwrap(),
                router,
                infra_grpc::server_options(limits),
            )
            .await
            .unwrap();
            let destination = format!("http://{}", server.local_addr());
            let echo = EchoServiceClient::new(
                Client::new(&destination, ClientSecurity::Plaintext, CALL).unwrap(),
            );
            // Watch has no whole-stream deadline: one stream must span all
            // failure/recovery phases. Each observation below stays bounded.
            let mut health = HealthClient::new(
                Client::with_timeout_policy(
                    &destination,
                    ClientSecurity::Plaintext,
                    ClientTimeout::OpeningOnly(CALL),
                )
                .unwrap(),
            );
            let watch = timeout(CALL, health.watch(health_request()))
                .await
                .unwrap()
                .unwrap()
                .into_inner();
            Self {
                server,
                echo,
                health,
                watch,
                observed: None,
                // template:end postgres-grpc-consumers:recovery-grpc-router-tail
                // template:begin postgres-grpc-auth:recovery-provider-value
                provider,
                provider_cancel,
                // template:end postgres-grpc-auth:recovery-provider-value
                // template:begin postgres-grpc-consumers:recovery-grpc-module-5
            }
        }

        pub(super) async fn useful(&mut self, expected: &str) {
            let response = timeout(CALL, self.echo.unary(work_request()))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(response.into_inner().message, expected);
        }

        pub(super) async fn refused_work(&mut self) {
            let failure = timeout(CALL, self.echo.unary(work_request()))
                .await
                .unwrap()
                .unwrap_err();
            assert_eq!(failure.code(), tonic::Code::Unavailable);
        }

        pub(super) async fn state(&mut self, ready: bool) {
            let expected = if ready {
                ServingStatus::Serving
            } else {
                ServingStatus::NotServing
            };
            let checked = timeout(CALL, self.health.check(health_request()))
                .await
                .unwrap()
                .unwrap()
                .into_inner();
            assert_eq!(checked.status, expected as i32);
            if self.observed != Some(expected) {
                let changed = timeout(WAIT, self.watch.message())
                    .await
                    .unwrap()
                    .unwrap()
                    .expect("live Watch remains open through ordinary recovery");
                assert_eq!(
                    changed.status, expected as i32,
                    "same Watch observes each transition without reconnect"
                );
                self.observed = Some(expected);
            }
        }

        pub(super) async fn stop(mut self) {
            self.state(false).await;
            assert!(
                timeout(CALL, self.watch.message())
                    .await
                    .unwrap()
                    .unwrap()
                    .is_none(),
                "drain closes Watch"
            );
            drop(self.watch);
            drop(self.health);
            drop(self.echo);
            assert_eq!(
                timeout(CALL, self.server.drain(Duration::from_secs(3)))
                    .await
                    .unwrap()
                    .unwrap(),
                Drained::Complete
            );
            // template:end postgres-grpc-consumers:recovery-grpc-module-5
            // template:begin postgres-grpc-auth:recovery-stop-provider
            self.provider_cancel.cancel();
            assert_eq!(
                timeout(CALL, self.provider.drain(Duration::from_secs(3)))
                    .await
                    .unwrap()
                    .unwrap(),
                Drained::Complete
            );
            // template:end postgres-grpc-auth:recovery-stop-provider
            // template:begin postgres-grpc-consumers:recovery-grpc-module-6
        }
    }

    // template:end postgres-grpc-consumers:recovery-grpc-module-6
    // template:begin postgres-grpc-auth:recovery-provider
    /// Reuse the current real-TLS introspection support; no fabricated Principal
    /// or weakened gRPC authentication chain is introduced for database proof.
    #[allow(
        clippy::disallowed_methods,
        reason = "local non-shipped identity provider fixture"
    )]
    async fn provider() -> (infra_bearerauthn::Verifier, Server, CancellationToken) {
        use infra_bearerauthn::{
            EndpointUrl, IntrospectionOptions, IssuerUrl,
            test_support::{FixtureTransport, prepare_introspection_with_fixture},
        };
        use std::{num::NonZeroUsize, sync::Arc};
        use tokio_rustls::rustls::{
            self,
            pki_types::{CertificateDer, PrivatePkcs8KeyDer},
        };
        let material = tls::TlsMaterial::new("provider.test");
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(material.cert)],
            PrivatePkcs8KeyDer::from(material.key).into(),
        )
        .unwrap();
        let router = axum::Router::new().route("/introspect", axum::routing::post(|| async {
            let expiry = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 3600;
            axum::Json(serde_json::json!({"active": true, "iss": "https://issuer.example", "aud": "api", "sub": "consumer", "exp": expiry}))
        }));
        let server = Server::bind_tls(
            "127.0.0.1:0".parse().unwrap(),
            router,
            ServerOptions {
                header_read_timeout: Duration::from_secs(5),
                max_header_bytes: 16 * 1024,
                max_connections: NonZeroU32::new(16),
                max_connection_age: None,
            },
            Arc::new(config),
        )
        .await
        .unwrap();
        let cancel = CancellationToken::new();
        let fixture = FixtureTransport::new(
            "provider.test",
            server.local_addr(),
            &material.root,
            cancel.clone(),
        )
        .unwrap();
        let verifier = prepare_introspection_with_fixture(
            IntrospectionOptions {
                issuer: IssuerUrl::parse("https://issuer.example").unwrap(),
                audiences: vec!["api".to_owned()],
                endpoint: EndpointUrl::parse(&format!(
                    "https://provider.test:{}/introspect",
                    server.local_addr().port()
                ))
                .unwrap(),
                client_id: "fixture".to_owned(),
                client_secret: secrecy::SecretString::from("fixture"),
                provider_concurrency: NonZeroUsize::new(16).unwrap(),
                cache: None,
            },
            fixture,
        )
        .unwrap();
        (verifier, server, cancel)
    }
    // template:end postgres-grpc-auth:recovery-provider
    // template:begin postgres-grpc-consumers:recovery-grpc-module-7
}
// template:end postgres-grpc-consumers:recovery-grpc-module-7
