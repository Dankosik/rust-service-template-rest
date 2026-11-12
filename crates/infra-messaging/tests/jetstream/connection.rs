//! Connection admission, TLS trust, reconnect, and close.

use super::*;

/// Disconnects one real broker connection and holds replacement connections
/// until the test explicitly restores the network path.
struct OutageRelay {
    url: String,
    pause: CancellationToken,
    replacement_held: CancellationToken,
    resume: CancellationToken,
    stop: CancellationToken,
    task: JoinHandle<()>,
}

impl OutageRelay {
    async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("nats://{}", listener.local_addr().unwrap());
        let target = relay_target(&nats_url());
        let pause = CancellationToken::new();
        let replacement_held = CancellationToken::new();
        let resume = CancellationToken::new();
        let stop = CancellationToken::new();
        let (paused, held, restored, stopped) = (
            pause.clone(),
            replacement_held.clone(),
            resume.clone(),
            stop.clone(),
        );
        let task = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    () = stopped.cancelled() => break,
                    accepted = listener.accept() => accepted,
                };
                let (mut client, _) = accepted.unwrap();
                if paused.is_cancelled() {
                    held.cancel();
                    tokio::select! {
                        () = stopped.cancelled() => break,
                        () = restored.cancelled() => {},
                    }
                }
                let mut broker = TcpStream::connect(&target).await.unwrap();
                tokio::select! {
                    () = stopped.cancelled() => break,
                    () = paused.cancelled(), if !restored.is_cancelled() => {},
                    _ = tokio::io::copy_bidirectional(&mut client, &mut broker) => {},
                }
            }
        });
        Self {
            url,
            pause,
            replacement_held,
            resume,
            stop,
            task,
        }
    }

    async fn finish(self) {
        self.stop.cancel();
        timeout(Duration::from_secs(3), self.task)
            .await
            .unwrap()
            .unwrap();
    }
}

/// Terminates TLS for one reconnecting client while preserving the real broker
/// behind it. The first fixture owns IPv4; the replacement owns IPv6 on the
/// same port, so `localhost` must make a new native dial to reach it.
struct TlsRotationRelay {
    stop: CancellationToken,
    accepted: Arc<AtomicUsize>,
    rejected: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl TlsRotationRelay {
    async fn start_v4(acceptor: tokio_rustls::TlsAcceptor) -> (Self, u16) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("IPv4 TLS relay binds");
        let port = listener
            .local_addr()
            .expect("IPv4 TLS relay address")
            .port();
        (Self::start(listener, acceptor), port)
    }

    async fn start_v6(port: u16, acceptor: tokio_rustls::TlsAcceptor) -> Self {
        let listener = TcpListener::bind(format!("[::1]:{port}"))
            .await
            .expect("IPv6 TLS relay binds on the replacement port");
        Self::start(listener, acceptor)
    }

    fn start(listener: TcpListener, acceptor: tokio_rustls::TlsAcceptor) -> Self {
        let stop = CancellationToken::new();
        let accepted = Arc::new(AtomicUsize::new(0));
        let rejected = Arc::new(AtomicUsize::new(0));
        let (stopped, accepted_by_relay, rejected_by_relay) =
            (stop.clone(), Arc::clone(&accepted), Arc::clone(&rejected));
        let target = relay_target(&nats_url());
        let task = tokio::spawn(async move {
            loop {
                let accepted_socket = tokio::select! {
                    () = stopped.cancelled() => break,
                    accepted_socket = listener.accept() => accepted_socket,
                };
                let (socket, _) = accepted_socket.expect("TLS relay accepts a connection");
                let Ok(client) = acceptor.accept(socket).await else {
                    rejected_by_relay.fetch_add(1, Ordering::SeqCst);
                    continue;
                };
                accepted_by_relay.fetch_add(1, Ordering::SeqCst);
                let mut broker = TcpStream::connect(&target)
                    .await
                    .expect("TLS relay reaches the fixture broker");
                let mut client = client;
                tokio::select! {
                    () = stopped.cancelled() => break,
                    _ = tokio::io::copy_bidirectional(&mut client, &mut broker) => {},
                }
            }
        });
        Self {
            stop,
            accepted,
            rejected,
            task,
        }
    }

    async fn wait_for_rejected_handshake(&self) {
        timeout(Duration::from_secs(10), async {
            while self.rejected.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("old root rejects the replacement TLS identity");
    }

    async fn wait_for_authenticated_connection(&self) {
        timeout(Duration::from_secs(10), async {
            while self.accepted.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("replacement root admits a fresh TLS connection");
    }

    async fn finish(self) {
        self.stop.cancel();
        timeout(Duration::from_secs(3), self.task)
            .await
            .expect("TLS relay stops")
            .expect("TLS relay task completes");
    }
}

fn trust_options(servers: Vec<String>) -> MessagingOptions {
    MessagingOptions {
        connection_name: "messaging-trust-test".to_owned(),
        servers,
        credentials: None,
        root_ca_path: None,
        allow_plaintext: true,
        tls_first: false,
        source_stream: "events".to_owned(),
        max_payload_bytes: 1024,
        consumer: None,
    }
}

fn tls_identity(name: &str) -> (String, tokio_rustls::TlsAcceptor) {
    use async_nats::rustls::{ServerConfig, pki_types::PrivatePkcs8KeyDer};
    use base64::Engine as _;
    use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, IsCa, KeyPair};

    let mut ca = CertificateParams::default();
    ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let issuer =
        CertifiedIssuer::self_signed(ca, KeyPair::generate().expect("CA key")).expect("test CA");
    let key = KeyPair::generate().expect("server key");
    let certificate = CertificateParams::new(vec![name.to_owned()])
        .expect("server name")
        .signed_by(&key, &issuer)
        .expect("server certificate");
    let encoded = base64::engine::general_purpose::STANDARD.encode(issuer.der());
    let root = format!("-----BEGIN CERTIFICATE-----\n{encoded}\n-----END CERTIFICATE-----\n");
    let server = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![certificate.der().clone()],
            PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        )
        .expect("server TLS config");
    (root, tokio_rustls::TlsAcceptor::from(Arc::new(server)))
}

#[expect(
    clippy::disallowed_methods,
    reason = "test-owned CA replacements control only a local fixture"
)]
fn replace_ca(path: &std::path::Path, pem: &str) {
    let replacement = path.with_extension("next");
    std::fs::write(&replacement, pem).expect("CA file is written");
    std::fs::rename(replacement, path).expect("CA file is replaced");
}

fn tls_fixture() -> (tempfile::NamedTempFile, tokio_rustls::TlsAcceptor) {
    use std::io::Write as _;

    let (pem, acceptor) = tls_identity("127.0.0.1");
    let mut root = tempfile::NamedTempFile::new().expect("CA file");
    root.write_all(pem.as_bytes()).expect("write CA PEM");
    (root, acceptor)
}

/// The wire fixture answers the native opening PING, then observes the
/// adapter's real topology request before disconnecting to cause recovery.
async fn serve_until_topology(socket: impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin) {
    use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

    let mut socket = tokio::io::BufReader::new(socket);
    let mut line = String::new();
    loop {
        line.clear();
        assert!(socket.read_line(&mut line).await.expect("NATS client line") > 0);
        if line == "PING\r\n" {
            socket.get_mut().write_all(b"PONG\r\n").await.expect("PONG");
        } else if line.starts_with("PUB $JS.API.") || line.starts_with("HPUB $JS.API.") {
            return;
        }
    }
}

fn info(discovered: &str) -> Vec<u8> {
    format!(
        "INFO {}\r\n",
        serde_json::json!({
            "server_id": "fixture", "version": "2.12.3", "headers": true,
            "jetstream": true, "max_payload": 1_048_576, "proto": 1,
            "connect_urls": [discovered],
        })
    )
    .into_bytes()
}

#[tokio::test]
async fn direct_tls_first_admission_refuses_plaintext_even_when_it_is_allowed() {
    for servers in [
        vec!["nats://127.0.0.1:1".to_owned()],
        vec![
            "tls://127.0.0.1:1".to_owned(),
            "nats://127.0.0.1:2".to_owned(),
        ],
    ] {
        let mut options = trust_options(servers);
        options.tls_first = true;
        let failure = Messaging::connect(
            options,
            Instant::now() + Duration::from_secs(5),
            CancellationToken::new(),
        )
        .await
        .expect_err("TLS-first must reject plaintext before any dial");
        std::assert_matches!(failure, infra_messaging::MessagingError::Configuration(_));
    }
}

#[tokio::test]
async fn ordinary_tls_and_mixed_seeds_ignore_untrusted_info_destinations() {
    use tokio::io::AsyncWriteExt as _;

    for mixed in [false, true] {
        let (root, tls) = tls_fixture();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("seed listener");
        let mut seeds = vec![format!(
            "tls://{}",
            listener.local_addr().expect("seed address")
        )];
        if mixed {
            seeds.push("nats://127.0.0.1:1".to_owned());
        }
        let mut server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("seed connection");
            // Native discovery would reject this invalid address before CONNECT.
            // It arrives before TLS authenticates the server.
            socket
                .write_all(&info("http://untrusted.invalid"))
                .await
                .expect("INFO");
            let socket = tls.accept(socket).await.expect("ordinary TLS handshake");
            serve_until_topology(socket).await;
        });
        let mut options = trust_options(seeds);
        options.allow_plaintext = mixed;
        options.root_ca_path = Some(root.path().to_owned());
        let cancel = CancellationToken::new();
        let mut connect = tokio::spawn(Messaging::connect(
            options,
            Instant::now() + Duration::from_secs(5),
            cancel.clone(),
        ));
        let observed = timeout(Duration::from_secs(5), &mut server).await;
        cancel.cancel();
        let connected = timeout(Duration::from_secs(5), &mut connect).await;
        if connected.is_err() {
            connect.abort();
            let _ = connect.await;
        }
        if observed.is_err() {
            server.abort();
            let _ = server.await;
        }
        connected
            .expect("cancelled admission finishes")
            .expect("adapter admission task")
            .expect_err("fixture does not supply topology");
        observed
            .expect("configured seed reaches topology admission")
            .expect("seed task accepted authenticated native CONNECT");
    }
}

#[tokio::test]
async fn authenticated_tls_first_and_trusted_plaintext_info_allow_discovery() {
    use tokio::io::AsyncWriteExt as _;

    for tls_first in [false, true] {
        let (root, tls) = tls_fixture();
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("seed listener");
        let discovered = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("discovered listener");
        let advertised = discovered
            .local_addr()
            .expect("discovered address")
            .to_string();
        let seed = format!(
            "{}://{}",
            if tls_first { "tls" } else { "nats" },
            listener.local_addr().expect("seed address")
        );
        let seed_tls = tls.clone();
        let mut server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("seed connection");
            drop(listener);
            if tls_first {
                let mut socket = seed_tls.accept(socket).await.expect("TLS precedes INFO");
                socket
                    .write_all(&info(&advertised))
                    .await
                    .expect("authenticated INFO");
                serve_until_topology(socket).await;
            } else {
                socket
                    .write_all(&info(&advertised))
                    .await
                    .expect("trusted network INFO");
                serve_until_topology(socket).await;
            }
        });
        let mut options = trust_options(vec![seed]);
        options.allow_plaintext = !tls_first;
        options.tls_first = tls_first;
        options.root_ca_path = Some(root.path().to_owned());
        let cancel = CancellationToken::new();
        let mut connect = tokio::spawn(Messaging::connect(
            options,
            Instant::now() + Duration::from_secs(5),
            cancel.clone(),
        ));
        let recovery = timeout(Duration::from_secs(5), async {
            let (socket, _) = discovered.accept().await.expect("discovered connection");
            if tls_first {
                // Scheme-less INFO addresses still require authenticated TLS.
                let _socket = tls.accept(socket).await.expect("discovered TLS handshake");
            }
        })
        .await;
        cancel.cancel();
        let connected = timeout(Duration::from_secs(5), &mut connect).await;
        if connected.is_err() {
            connect.abort();
            let _ = connect.await;
        }
        let seed_result = timeout(Duration::from_secs(5), &mut server).await;
        if seed_result.is_err() {
            server.abort();
            let _ = server.await;
        }
        connected
            .expect("cancelled admission finishes")
            .expect("adapter admission task")
            .expect_err("fixture does not supply topology");
        seed_result.expect("seed task finishes").expect("seed task");
        recovery.expect("same native owner connects to its discovered destination");
    }
}

#[tokio::test]
async fn tls_root_ca_file_is_reread_for_a_same_client_reconnect_to_a_new_localhost_address() {
    let fixture = Fixture::create(false).await;
    let roots = tempfile::tempdir().expect("temporary root directory");
    let root_path = roots.path().join("nats-ca.pem");
    let (ca_a, tls_a) = tls_identity("localhost");
    replace_ca(&root_path, &ca_a);
    let (first, port) = TlsRotationRelay::start_v4(tls_a).await;
    let mut options = options_with_servers(
        &fixture,
        vec![format!("tls://localhost:{port}")],
        None,
        1024,
    );
    options.allow_plaintext = false;
    options.tls_first = true;
    options.root_ca_path = Some(root_path.clone());
    let cancel = CancellationToken::new();
    let messaging = Messaging::connect(options, deadline(), cancel.clone())
        .await
        .expect("the IPv4 relay is admitted with private CA A");
    assert_eq!(first.accepted.load(Ordering::SeqCst), 1);

    first.finish().await;
    let (ca_b, tls_b) = tls_identity("localhost");
    let replacement = TlsRotationRelay::start_v6(port, tls_b).await;
    replacement.wait_for_rejected_handshake().await;
    assert!(
        messaging.probe().check().await.is_err(),
        "the old root cannot authenticate CA B after the IPv4 connection closes"
    );

    replace_ca(&root_path, &ca_b);
    replacement.wait_for_authenticated_connection().await;
    timeout(Duration::from_secs(10), async {
        loop {
            if messaging.probe().check().await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the same messaging connection reports fresh source metadata after CA rotation");
    let prepared = registry(&fixture)
        .prepare(&event("after-root-ca-rotation"), 1024)
        .expect("fixture event is prepared");
    messaging
        .producer()
        .publish(&prepared, &context(deadline(), &cancel))
        .await
        .expect("the same messaging connection publishes after CA rotation");

    close(messaging).await;
    replacement.finish().await;
    fixture.cleanup().await;
}

struct ClosedEventGate {
    entered: Mutex<Option<std::sync::mpsc::Sender<()>>>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
    control_failed: Arc<AtomicBool>,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for ClosedEventGate {
    fn on_event(&self, event: &tracing::Event<'_>, _: tracing_subscriber::layer::Context<'_, S>) {
        let mut fields = LoggedFields(Vec::new());
        event.record(&mut fields);
        let message = fields
            .0
            .iter()
            .any(|(key, value)| *key == "message" && value == "messaging_connection");
        let closed = fields
            .0
            .iter()
            .any(|(key, value)| *key == "result" && value == "closed");
        if !message || !closed {
            return;
        }
        let Some(entered) = self.entered.lock().expect("callback gate lock").take() else {
            return;
        };
        let _ = entered.send(());
        if self
            .release
            .lock()
            .expect("callback release lock")
            .recv_timeout(Duration::from_secs(5))
            .is_err()
        {
            self.control_failed.store(true, Ordering::Release);
        }
    }
}

#[tokio::test]
async fn close_waits_for_the_terminal_connection_event() {
    use std::task::Poll;
    use tracing_subscriber::layer::SubscriberExt as _;

    let fixture = Fixture::create(false).await;
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let control_failed = Arc::new(AtomicBool::new(false));
    let subscriber = tracing_subscriber::registry().with(ClosedEventGate {
        entered: Mutex::new(Some(entered_tx)),
        release: Mutex::new(release_rx),
        control_failed: Arc::clone(&control_failed),
    });
    // The native callback runs on this test's current-thread runtime. The
    // observer below owns a separate executor so it can inspect close while
    // the real subscriber holds the callback before its last effect returns.
    let _subscriber = tracing::subscriber::set_default(subscriber);
    let messaging = Box::pin(Messaging::connect(
        options(&fixture, None, 1024),
        deadline(),
        CancellationToken::new(),
    ))
    .await
    .expect("fixture source stream is admitted");
    let (done_tx, done_rx) = oneshot::channel();
    let observer = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("close observer runtime");
        let result = runtime.block_on(async move {
            let cancel = CancellationToken::new();
            let mut closing = Box::pin(messaging.close(deadline(), &cancel));
            let first = futures_util::poll!(&mut closing);
            entered_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("real terminal callback reached subscriber");
            let before_release = match first {
                Poll::Ready(outcome) => Some(outcome),
                Poll::Pending => match futures_util::poll!(&mut closing) {
                    Poll::Ready(outcome) => Some(outcome),
                    Poll::Pending => None,
                },
            };
            let premature = before_release.is_some();
            release_tx.send(()).expect("release terminal callback");
            let outcome = match before_release {
                Some(outcome) => outcome,
                None => closing.await,
            };
            (premature, outcome)
        });
        let _ = done_tx.send(result);
    });
    let result = timeout(Duration::from_secs(10), done_rx).await;
    let joined = observer.join();
    fixture.cleanup().await;
    joined.expect("close observer thread");
    let (premature, outcome) = result
        .expect("close observer bound")
        .expect("close observer result");
    assert!(
        !control_failed.load(Ordering::Acquire),
        "callback hold lost its release control"
    );
    assert!(
        !premature,
        "close acknowledged before the terminal connection event returned"
    );
    assert_eq!(outcome, infra_messaging::CloseOutcome::Complete);
}

#[tokio::test]
async fn cancelled_topology_admission_retains_the_native_client_until_closed() {
    let fixture = Fixture::create(false).await;
    let topology_reached = Arc::new(Notify::new());
    let reached = Arc::clone(&topology_reached);
    let relay = AckDroppingRelay::start_filtering(move |payload| {
        let topology = std::str::from_utf8(payload)
            .is_ok_and(|body| body.contains("io.nats.jetstream.api.v1.stream_info_response"));
        if topology {
            reached.notify_one();
        }
        topology
    })
    .await;
    let mut startup = Messaging::prepare(
        options_with_servers(&fixture, vec![relay.url.clone()], None, 1024),
        deadline(),
        CancellationToken::new(),
    )
    .unwrap();
    let mut admission = Box::pin(startup.admit());
    tokio::select! {
        result = &mut admission => panic!("topology admission completed with its reply withheld: {result:?}"),
        reached = timeout(Duration::from_secs(3), topology_reached.notified()) => {
            reached.expect("the real broker must answer the topology request");
        }
    }
    drop(admission);
    assert_eq!(
        startup.close(deadline(), &CancellationToken::new()).await,
        infra_messaging::CloseOutcome::Complete,
        "the retained client must drain and deliver its native Closed notification"
    );
    relay.join().await;
    fixture.cleanup().await;
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one retained-client scenario owns outage, contending publication, recovery, and cleanup"
)]
async fn a_consumer_recovers_after_its_broker_connection_is_interrupted() {
    let fixture = Fixture::create(false).await;
    let relay = OutageRelay::start().await;
    let cancel = CancellationToken::new();
    let messaging = Box::pin(Messaging::connect(
        options_with_servers(
            &fixture,
            vec![relay.url.clone()],
            Some(consumer_options(&fixture)),
            1024,
        ),
        deadline(),
        cancel.clone(),
    ))
    .await
    .unwrap();
    let (observed_tx, mut observed_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut registered = registry(&fixture);
    registered
        .register::<ExampleEvent, _, _>(move |event, _| {
            observed_tx.send(event.id).unwrap();
            async { Ok(()) }
        })
        .unwrap();
    let mut handle = messaging.consumer(registered).await.unwrap().start(&cancel);
    let before = registry(&fixture)
        .prepare(&event("before-outage"), 1024)
        .unwrap();
    messaging
        .producer()
        .publish(&before, &context(deadline(), &cancel))
        .await
        .unwrap();
    assert_eq!(
        timeout(Duration::from_secs(3), observed_rx.recv())
            .await
            .unwrap()
            .unwrap(),
        "before-outage"
    );
    wait_for_source_ack(&fixture).await;

    relay.pause.cancel();
    timeout(Duration::from_secs(3), async {
        // Only this Messaging client uses the relay. Its replacement dial
        // proves it processed the old connection's EOF, and INFO stays held.
        relay.replacement_held.cancelled().await;
        assert!(messaging.probe().check().await.is_err());
    })
    .await
    .expect("the disconnected dependency becomes unready");
    let contending = registry(&fixture)
        .prepare(&event("concurrent-silent-publication"), 1024)
        .unwrap();
    std::assert_matches!(
        messaging
            .producer()
            .publish(
                &contending,
                &context(Instant::now() + Duration::from_millis(50), &cancel)
            )
            .await,
        Err(PublishError::Ambiguous)
    );
    let after = registry(&fixture)
        .prepare(&event("after-outage"), 1024)
        .unwrap();
    fixture
        .jetstream
        .publish_with_headers(
            fixture.subject.clone(),
            infra_messaging::wire::encode_prepared(&after).unwrap(),
            after.payload().clone(),
        )
        .await
        .unwrap()
        .await
        .unwrap();
    relay.resume.cancel();
    timeout(Duration::from_secs(45), async {
        loop {
            if observed_rx.recv().await.as_deref() == Some("after-outage") {
                return;
            }
        }
    })
    .await
    .expect("the same consumer recovers and handles retained work after a bounded contending publication");
    let recovered = registry(&fixture)
        .prepare(&event("after-recovery-publication"), 1024)
        .unwrap();
    messaging
        .producer()
        .publish(&recovered, &context(deadline(), &cancel))
        .await
        .expect("the same retained producer publishes after recovery");
    timeout(Duration::from_secs(10), async {
        loop {
            if observed_rx.recv().await.as_deref() == Some("after-recovery-publication") {
                return;
            }
        }
    })
    .await
    .expect("the recovered consumer observes the retained producer publication");
    wait_for_source_ack_at_least(&fixture, 2).await;
    handle.finish(deadline()).await.unwrap();
    close(messaging).await;
    relay.finish().await;
    fixture.cleanup().await;
}
