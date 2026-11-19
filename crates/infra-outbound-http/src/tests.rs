use std::{fmt::Write as _, net::SocketAddr, sync::Arc, task::Context, time::Duration};

use bytes::Bytes;
use http::{Request, Version, header};
use tokio::{
    io::AsyncWriteExt,
    net::{TcpListener, TcpStream},
    sync::oneshot,
    time::Instant,
};

use tls_test_support::TlsMaterial;

use crate::{Error, Limits, MAX_RESPONSE_HEADER_COUNT, test_support::*};

#[tokio::test]
async fn standard_request_returns_standard_response_with_full_body() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 201 Created\r\nX-Fixture: yes\r\nContent-Length: 2\r\n\r\nok",
    )
    .await;
    let response = fixture_client(address, &material)
        .execute(request(), deadline())
        .await
        .expect("trusted TLS response");
    assert_eq!(response.status(), http::StatusCode::CREATED);
    assert_eq!(response.version(), Version::HTTP_11);
    assert_eq!(response.headers()["x-fixture"], "yes");
    assert_eq!(response.body().as_ref(), b"ok");
    server.await.expect("fixture server succeeds");
}

#[tokio::test]
async fn tls_trust_and_hostname_failures_remain_transport_errors() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, server) =
        tls_server(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let untrusted = TlsMaterial::new(FIXTURE_HOST);
    std::assert_matches!(
        fixture_client(address, &untrusted)
            .execute(request(), deadline())
            .await,
        Err(Error::Transport { .. })
    );
    server.await.expect("untrusted fixture server succeeds");

    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, server) =
        tls_server(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let client = fixture_client_for_host("different.fixture.test", address, &material, limits());
    std::assert_matches!(
        client
            .execute(
                request_to("https://different.fixture.test/fixture"),
                deadline()
            )
            .await,
        Err(Error::Transport { .. })
    );
    server.await.expect("hostname fixture server succeeds");
}

#[tokio::test]
async fn framed_and_streamed_bodies_obey_the_response_ceiling() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let mut ceiling = limits();
    ceiling.response_body_bytes = 2;
    let cases = [
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".as_slice(),
            true,
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nno!\r\n0\r\n\r\n"
                .as_slice(),
            false,
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\no\r\n1\r\nk\r\n1\r\n!\r\n0\r\n\r\n"
                .as_slice(),
            false,
        ),
        (
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nno!".as_slice(),
            false,
        ),
    ];
    for (wire, succeeds) in cases {
        let (address, server) = tls_server(&material, wire).await;
        let result = fixture_client_with_limits(address, &material, ceiling)
            .execute_buffered(request(), &operation_context::OperationContext::unbounded())
            .await;
        if succeeds {
            assert_eq!(
                result.expect("exact ceiling response").body().as_ref(),
                b"ok"
            );
        } else {
            let error = result.expect_err("response exceeds the ceiling after headers");
            std::assert_matches!(
                error,
                crate::ExchangeError::AfterResponse {
                    status: http::StatusCode::OK,
                    source: Error::ResponseBodyTooLarge,
                }
            );
        }
        server.await.expect("body fixture server succeeds");
    }
}

#[tokio::test]
async fn fragmented_body_preserves_bytes_and_discards_trailers() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nTrailer: X-Fixture\r\n\r\n\
          1\r\na\r\n2\r\nbc\r\n3\r\ndef\r\n5\r\nghijk\r\n8\r\nlmnopqrs\r\n7\r\ntuvwxyz\r\n\
          0\r\nX-Fixture: discarded\r\n\r\n",
    )
    .await;
    let response = fixture_client_with_limits(
        address,
        &material,
        Limits {
            response_body_bytes: 26,
            ..limits()
        },
    )
    .execute(request(), deadline())
    .await
    .expect("fragmented exact-ceiling response reaches EOF");
    assert_eq!(response.body().as_ref(), b"abcdefghijklmnopqrstuvwxyz");
    assert!(!response.headers().contains_key("x-fixture"));
    server.await.expect("fragmented fixture server succeeds");
}

#[tokio::test]
async fn parser_header_count_overflow_is_a_transport_error() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let mut parser_limit = limits();
    parser_limit.response_header_count = 1;
    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nX-Overflow: one\r\n\r\n",
    )
    .await;
    let error = fixture_client_with_limits(address, &material, parser_limit)
        .execute_buffered(request(), &operation_context::OperationContext::unbounded())
        .await
        .expect_err("header overflow precedes an admitted response");
    std::assert_matches!(
        error,
        crate::ExchangeError::BeforeResponse(Error::Transport { .. })
    );
    server.await.expect("parser header fixture joins");
}

#[tokio::test]
async fn largest_header_count_limit_admits_that_many_response_headers() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let mut largest = limits();
    largest.response_header_count = MAX_RESPONSE_HEADER_COUNT;
    let mut head = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n".to_vec();
    for index in 1..MAX_RESPONSE_HEADER_COUNT {
        head.extend_from_slice(format!("h{index}:\r\n").as_bytes());
    }
    head.extend_from_slice(b"\r\n");
    let (address, server) = tls_server(&material, head.leak()).await;
    let response = fixture_client_with_limits(address, &material, largest)
        .execute(request(), deadline())
        .await
        .expect("response at the header ceiling");
    assert_eq!(response.headers().len(), MAX_RESPONSE_HEADER_COUNT);
    server.await.expect("header ceiling fixture joins");
}

#[tokio::test]
async fn response_head_beyond_the_parser_buffer_is_a_transport_error() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    for (length, admitted) in [(1024, true), (512 * 1024, false)] {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("head fixture listener");
        let address = listener.local_addr().expect("head fixture address");
        let acceptor = fixture_acceptor(&material);
        let server = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(3), async move {
                let (socket, _) = listener.accept().await.expect("head fixture accepts");
                let mut stream = acceptor.accept(socket).await.expect("head TLS handshake");
                read_request_headers(&mut stream).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nX-Large: {}\r\n\r\n",
                    "a".repeat(length)
                );
                stream.write_all(head.as_bytes()).await?;
                stream.flush().await
            })
            .await
            .expect("head fixture completes within its budget")
        });
        let result = fixture_client(address, &material)
            .execute(request(), deadline())
            .await;
        let written = server.await.expect("head fixture joins");
        if admitted {
            written.expect("small response head was sent");
            let response = result.expect("small response head is admitted");
            assert_eq!(response.headers()["x-large"].as_bytes().len(), length);
        } else {
            std::assert_matches!(
                result,
                Err(Error::Transport { ref source }) if source.source()
                    .and_then(|cause| cause.downcast_ref::<hyper::Error>())
                    .is_some_and(hyper::Error::is_parse),
                "expected the HTTP parser to reject the oversized head, write: {written:?}"
            );
        }
    }
}

#[tokio::test]
async fn advertised_overflow_and_missing_exact_cap_eof_do_not_return_a_body() {
    enum Expected {
        BodyTooLarge,
        Transport,
    }

    let material = TlsMaterial::new(FIXTURE_HOST);
    let mut ceiling = limits();
    ceiling.response_body_bytes = 2;
    let cases = [
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nno!".as_slice(),
            "advertised overflow",
            Expected::BodyTooLarge,
        ),
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\no".as_slice(),
            "missing exact-cap EOF",
            Expected::Transport,
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\no\r\n1\r\nk\r\n"
                .as_slice(),
            "missing chunked EOF after exact-cap payload",
            Expected::Transport,
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nok\r\n0\r\ninvalid-trailer\r\n\r\n"
                .as_slice(),
            "malformed trailer after exact-cap payload",
            Expected::Transport,
        ),
    ];
    for (wire, name, expected) in cases {
        let (address, server) = tls_server(&material, wire).await;
        let error = fixture_client_with_limits(address, &material, ceiling)
            .execute(request(), deadline())
            .await
            .expect_err(name);
        match expected {
            Expected::BodyTooLarge => std::assert_matches!(error, Error::ResponseBodyTooLarge),
            Expected::Transport => {
                let Error::Transport { source } = error else {
                    panic!("{name} must retain a transport cause");
                };
                assert!(source.is::<hyper::Error>(), "{name} retains the body error");
            }
        }
        server.await.expect("framing fixture server succeeds");
    }
}

#[tokio::test]
async fn expired_deadline_refuses_before_network_work() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("no-connect listener");
    let client = fixture_client(listener.local_addr().expect("listener address"), &material);
    let expired = client
        .execute(request(), Instant::now() - Duration::from_millis(1))
        .await;
    std::assert_matches!(expired, Err(Error::Timeout));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn stopped_request_context_refuses_both_entry_points_before_dispatch() {
    use operation_context::OperationContext;

    let material = TlsMaterial::new(FIXTURE_HOST);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let client = fixture_client(listener.local_addr().expect("address"), &material);
    for expired in [false, true] {
        let context = OperationContext::with_timeout(if expired {
            Duration::ZERO
        } else {
            Duration::from_secs(1)
        });
        if !expired {
            context.cancel();
        }
        let refused = |result: Result<_, Error>| {
            if expired {
                matches!(result, Err(Error::Timeout))
            } else {
                matches!(result, Err(Error::Cancelled))
            }
        };
        let mut supplied = request();
        supplied.extensions_mut().insert(context.clone());
        assert!(refused(client.execute(supplied, deadline()).await));
        let mut supplied = request();
        supplied.extensions_mut().insert(context.clone());
        assert!(refused(
            client
                .execute_with_context(supplied, &OperationContext::unbounded())
                .await
        ));
        assert!(refused(
            client.execute_with_context(request(), &context).await
        ));
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn nested_request_stops_for_each_inherited_or_local_cancellation() {
    use operation_context::OperationContext;

    let material = TlsMaterial::new(FIXTURE_HOST);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = fixture_client(listener.local_addr().unwrap(), &material);
    for cancelled in 0..3 {
        let scopes = [(); 3].map(|()| OperationContext::unbounded());
        let mut outer = request();
        outer.extensions_mut().insert(scopes[1].clone());
        let parent = client.prepare(outer, &scopes[0]).unwrap();
        let mut inner = request();
        inner.extensions_mut().insert(scopes[2].clone());
        let exchange = client.execute_buffered(inner, parent.context().clone());
        let (result, _connection) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(exchange, async {
                let (connection, _) = listener.accept().await.unwrap();
                scopes[cancelled].cancel();
                connection
            })
        })
        .await
        .expect("a cancelled exchange must finish during the TLS handshake");
        std::assert_matches!(
            result,
            Err(crate::ExchangeError::BeforeResponse(Error::Cancelled))
        );
    }
}

#[tokio::test]
async fn context_cancellation_ends_an_incomplete_buffered_body() {
    use operation_context::OperationContext;

    let material = TlsMaterial::new(FIXTURE_HOST);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let acceptor = fixture_acceptor(&material);
    let (received, dispatched) = oneshot::channel();
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(3), async move {
            let (socket, _) = listener.accept().await.expect("accept");
            let mut stream = acceptor.accept(socket).await.expect("TLS");
            read_request_headers(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\no")
                .await
                .expect("partial body");
            stream.flush().await.expect("flush");
            received.send(()).expect("client waiting");
            std::future::pending::<()>().await;
        })
        .await
        .expect("fixture bounded");
    });
    let client = fixture_client(address, &material);
    let context = OperationContext::with_timeout(Duration::from_secs(2));
    let exchange = client.execute_with_context(request(), &context);
    tokio::pin!(exchange);
    tokio::select! {
        result = &mut exchange => panic!("incomplete body completed: {result:?}"),
        result = dispatched => result.expect("request observed"),
    }
    context.cancel();
    std::assert_matches!(
        tokio::time::timeout(Duration::from_millis(200), exchange)
            .await
            .expect("cancellation must end waiting before the local ceiling"),
        Err(Error::Cancelled)
    );
    server.abort();
    assert!(server.await.expect_err("fixture cancelled").is_cancelled());
}

struct AdvanceOnAttemptStart(Duration);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for AdvanceOnAttemptStart {
    fn on_new_span(
        &self,
        attributes: &tracing::span::Attributes<'_>,
        _: &tracing::span::Id,
        _: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if attributes.metadata().name() == "outbound_http" {
            let mut advance = std::pin::pin!(tokio::time::advance(self.0));
            let _ = std::future::Future::poll(
                advance.as_mut(),
                &mut Context::from_waker(std::task::Waker::noop()),
            );
        }
    }
}

#[tokio::test(start_paused = true)]
async fn setup_spends_the_fixed_parent_or_operation_end_before_dispatch() {
    use tracing::instrument::WithSubscriber as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    let material = TlsMaterial::new(FIXTURE_HOST);
    for (operation, parent) in [(1, 2), (2, 1), (u64::MAX, 1)] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut ceiling = limits();
        ceiling.operation_timeout = Duration::from_secs(operation);
        let client = fixture_client_with_limits(listener.local_addr().unwrap(), &material, ceiling);
        let subscriber =
            tracing_subscriber::registry().with(AdvanceOnAttemptStart(Duration::from_secs(1)));
        let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
        let result = client
            .execute(request(), Instant::now() + Duration::from_secs(parent))
            .with_subscriber(subscriber)
            .await;
        std::assert_matches!(result, Err(Error::Timeout), "{result:?}");
        assert!(
            tokio::time::timeout(Duration::from_millis(20), listener.accept())
                .await
                .is_err(),
            "expired setup must not start a connection"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn dispatch_can_be_refused_after_request_setup() {
    use operation_context::OperationContext;
    use tracing::instrument::WithSubscriber as _;
    use tracing_subscriber::layer::SubscriberExt as _;

    let material = TlsMaterial::new(FIXTURE_HOST);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut ceiling = limits();
    ceiling.operation_timeout = Duration::from_secs(5);
    let client = fixture_client_with_limits(listener.local_addr().unwrap(), &material, ceiling);
    let subscriber =
        tracing_subscriber::registry().with(AdvanceOnAttemptStart(Duration::from_secs(1)));
    let credential_expiry = Instant::now() + Duration::from_secs(1);
    let context = OperationContext::with_timeout(Duration::from_secs(5));
    let mut observed_status = None;

    let result = client
        .prepare(request(), &context)
        .unwrap()
        .send(
            || Instant::now() < credential_expiry,
            |status| observed_status = Some(status),
        )
        .with_subscriber(subscriber)
        .await;

    std::assert_matches!(result, Err(Error::DispatchRefused));
    assert_eq!(observed_status, None);
    assert!(
        tokio::time::timeout(Duration::from_millis(20), listener.accept())
            .await
            .is_err(),
        "a refused dispatch must not start a connection"
    );
}

struct AdvanceOnAttemptFinish;

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for AdvanceOnAttemptFinish {
    fn on_record(
        &self,
        _: &tracing::span::Id,
        values: &tracing::span::Record<'_>,
        _: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut fields = FieldVisitor(SpanFields::new());
        values.record(&mut fields);
        if fields.0.contains_key("outbound.outcome") {
            tokio::time::pause();
            let mut advance = std::pin::pin!(tokio::time::advance(Duration::from_secs(20)));
            let _ = std::future::Future::poll(
                advance.as_mut(),
                &mut Context::from_waker(std::task::Waker::noop()),
            );
        }
    }
}

#[tokio::test]
async fn terminal_observation_preserves_the_timely_operation_result_once() {
    use tracing_subscriber::layer::SubscriberExt as _;

    let recorder = observation_recorder();
    let diagnostics = SpanDiagnostics::default();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let _subscriber =
        tracing::subscriber::set_default(diagnostics.clone().with(AdvanceOnAttemptFinish));
    let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, server) =
        tls_server(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let client = fixture_client(address, &material);
    let end = deadline();
    let response = client.execute(request(), end).await.unwrap();
    assert_eq!(response.status(), http::StatusCode::OK);
    assert!(Instant::now() > end, "terminal callback crossed the end");
    server.abort();
    if let Err(error) = server.await {
        assert!(error.is_cancelled());
    }
    let scrape = recorder.handle().render();
    assert_eq!(recorded_count(&scrape, &[]), 1);
    assert_eq!(
        recorded_count(&scrape, &["outbound_outcome=\"response\""]),
        1
    );
    let spans = diagnostics.0.lock().unwrap();
    assert_eq!(spans.len(), 1);
    assert_eq!(spans[0]["outbound.outcome"], "\"response\"");
}

struct ResponseWake(tokio::sync::Notify);

impl std::task::Wake for ResponseWake {
    fn wake(self: Arc<Self>) {
        self.0.notify_one();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.notify_one();
    }
}

#[tokio::test]
async fn ready_response_after_the_fixed_end_is_timeout() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let acceptor = fixture_acceptor(&material);
    let (received, request_received) = oneshot::channel();
    let (release, response_release) = oneshot::channel();
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(3), async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut stream = acceptor.accept(socket).await.unwrap();
            read_request_headers(&mut stream).await;
            received.send(()).unwrap();
            response_release.await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await
                .unwrap();
            stream.flush().await.unwrap();
        })
        .await
        .expect("ready-response fixture budget");
    });
    let mut ceiling = limits();
    ceiling.operation_timeout = Duration::from_secs(10);
    let client = fixture_client_with_limits(address, &material, ceiling);
    let mut exchange =
        Box::pin(client.execute(request(), Instant::now() + Duration::from_secs(10)));
    tokio::select! {
        result = &mut exchange => panic!("response is still gated: {result:?}"),
        result = request_received => result.unwrap(),
    }
    let response_ready = Arc::new(ResponseWake(tokio::sync::Notify::new()));
    let waker = std::task::Waker::from(response_ready.clone());
    assert!(
        std::future::Future::poll(exchange.as_mut(), &mut Context::from_waker(&waker),)
            .is_pending()
    );
    release.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), response_ready.0.notified())
        .await
        .expect("transport delivers the ready response");
    server.await.expect("ready-response fixture joins");
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(10)).await;
    let result = exchange.await;
    std::assert_matches!(result, Err(Error::Timeout), "{result:?}");
}

async fn unresponsive_listener() -> (SocketAddr, tokio::net::TcpListener, Vec<TcpStream>) {
    let socket = tokio::net::TcpSocket::new_v4().expect("unresponsive socket");
    socket
        .bind("127.0.0.1:0".parse().expect("loopback address"))
        .expect("unresponsive bind");
    let listener = socket.listen(1).expect("unresponsive listen");
    let address = listener.local_addr().expect("unresponsive address");
    let mut queued = Vec::new();
    for _ in 0..64 {
        match tokio::time::timeout(Duration::from_millis(200), TcpStream::connect(address)).await {
            Ok(stream) => queued.push(stream.expect("queued connection")),
            Err(_) => return (address, listener, queued),
        }
    }
    panic!("the accept queue never filled");
}

#[tokio::test]
async fn unresponsive_address_leaves_time_for_the_next_one() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (unresponsive, _listener, _queued) = unresponsive_listener().await;
    let (address, server) =
        tls_server(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let mut limits = limits();
    limits.operation_timeout = Duration::from_secs(2);
    let response =
        fixture_client_resolving(FIXTURE_HOST, vec![unresponsive, address], &material, limits)
            .execute(request(), Instant::now() + Duration::from_secs(2))
            .await
            .expect("the second address answers");
    assert_eq!(response.status(), 200);
    server.await.expect("fallback fixture server succeeds");
}

#[tokio::test]
async fn a_reused_client_uses_a_replacement_address_on_its_next_dial() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (old_address, old_server) =
        tls_server(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let (new_address, new_server) = tls_server(
        &material,
        b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n",
    )
    .await;
    let resolver = FixtureResolver::with_addresses(vec![old_address]);
    let client = fixture_client_with_resolver(FIXTURE_HOST, resolver.clone(), &material, limits());

    assert_eq!(
        client
            .execute(request(), deadline())
            .await
            .expect("old address response")
            .status(),
        http::StatusCode::OK
    );
    old_server.await.expect("old address TLS server joins");

    resolver.replace(vec![new_address]);
    assert_eq!(
        client
            .execute(request(), deadline())
            .await
            .expect("replacement address response")
            .status(),
        http::StatusCode::NO_CONTENT
    );
    new_server
        .await
        .expect("replacement address TLS server joins");
}

#[tokio::test]
async fn request_is_not_replayed_after_the_peer_received_it() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("replay listener");
    let address = listener.local_addr().expect("replay address");
    let acceptor = fixture_acceptor(&material);
    let server = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(3), async move {
            let (socket, _) = listener.accept().await.expect("first request connects");
            let mut stream = acceptor.accept(socket).await.expect("first TLS handshake");
            read_request_headers(&mut stream).await;
            drop(stream);
            tokio::time::timeout(Duration::from_millis(100), listener.accept())
                .await
                .is_ok()
        })
        .await
        .expect("replay fixture is bounded")
    });
    std::assert_matches!(
        fixture_client(address, &material)
            .execute(request(), deadline())
            .await,
        Err(Error::Transport { .. })
    );
    assert!(
        !server.await.expect("replay fixture joins"),
        "request was replayed"
    );
}

#[tokio::test]
async fn completed_exchange_reuses_an_idle_https_connection() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, server) = tls_server_reuses_one_connection(&material).await;
    let client = fixture_client(address, &material);
    client
        .execute(request(), deadline())
        .await
        .expect("first pooled response");
    let second = client
        .execute(request(), deadline())
        .await
        .expect("second pooled response");
    assert_eq!(second.status(), http::StatusCode::NO_CONTENT);
    assert_eq!(server.await.expect("pool fixture joins"), 1);
}

#[tokio::test]
async fn target_and_caller_headers_reach_the_wire_unchanged() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, captured, server) =
        tls_server_capture(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let mut request =
        request_to("https://provider.fixture.test//other.fixture.test/items%23safe?q=%23");
    for (name, value) in [
        ("traceparent", "00-abc-def-01"),
        ("accept-encoding", "gzip"),
    ] {
        request
            .headers_mut()
            .insert(name, value.parse().expect("header value"));
    }
    fixture_client(address, &material)
        .execute(request, deadline())
        .await
        .expect("captured request response");
    let wire = captured.await.expect("captured request");
    assert!(wire.starts_with("GET //other.fixture.test/items%23safe?q=%23 HTTP/1.1\r\n"));
    let wire = wire.to_ascii_lowercase();
    assert!(wire.contains("host: provider.fixture.test\r\n"));
    assert!(wire.contains("traceparent: 00-abc-def-01"));
    assert!(wire.contains("accept-encoding: gzip"));
    server.await.expect("capture fixture server succeeds");
}

#[tokio::test]
async fn buffered_body_length_replaces_caller_framing_on_the_wire() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, captured, server) =
        tls_server_capture(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let request = Request::post(FIXTURE_URL)
        .version(Version::HTTP_2)
        .header(header::CONTENT_LENGTH, "2")
        .header(header::TRANSFER_ENCODING, "chunked")
        .body(Bytes::from_static(b"hello"))
        .expect("misframed request");
    fixture_client(address, &material)
        .execute(request, deadline())
        .await
        .expect("captured request response");
    let wire = captured.await.expect("captured request");
    assert!(wire.starts_with("POST /fixture HTTP/1.1\r\n"), "{wire}");
    let wire = wire.to_ascii_lowercase();
    assert!(wire.contains("content-length: 5\r\n"), "{wire}");
    assert_eq!(wire.matches("content-length:").count(), 1);
    assert!(!wire.contains("transfer-encoding"), "{wire}");
    server.await.expect("capture fixture server succeeds");
}

#[tokio::test]
async fn the_client_sets_host_and_a_default_accept_on_the_wire() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, captured, server) =
        tls_server_capture(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    fixture_client(address, &material)
        .execute(
            request_to("https://PROVIDER.fixture.test/items"),
            deadline(),
        )
        .await
        .expect("captured request response");
    let wire = captured.await.expect("captured request");
    assert!(wire.starts_with("GET /items HTTP/1.1\r\n"), "{wire}");
    let wire = wire.to_ascii_lowercase();
    assert!(
        wire.contains("\r\nhost: provider.fixture.test\r\n"),
        "{wire}"
    );
    assert!(wire.contains("\r\naccept: */*\r\n"), "{wire}");
    server.await.expect("capture fixture server succeeds");
}

#[tokio::test]
async fn an_empty_body_is_framed_by_its_method_on_the_wire() {
    for (method, sent, expected) in [
        (http::Method::POST, None, ["content-length: 0"].as_slice()),
        (http::Method::POST, Some("7"), &["content-length: 0"]),
        (http::Method::PUT, Some("7"), &["content-length: 0"]),
        (http::Method::PATCH, Some("7"), &["content-length: 0"]),
        (http::Method::GET, Some("7"), &[]),
        (http::Method::DELETE, Some("7"), &[]),
    ] {
        let material = TlsMaterial::new(FIXTURE_HOST);
        let (address, captured, server) =
            tls_server_capture(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
        let mut request = Request::builder()
            .method(method.clone())
            .uri(FIXTURE_URL)
            .header(header::TRANSFER_ENCODING, "chunked")
            .body(Bytes::new())
            .expect("empty request");
        if let Some(sent) = sent {
            request
                .headers_mut()
                .insert(header::CONTENT_LENGTH, sent.parse().expect("length"));
        }
        fixture_client(address, &material)
            .execute(request, deadline())
            .await
            .expect("captured request response");
        let wire = captured
            .await
            .expect("captured request")
            .to_ascii_lowercase();
        let lengths = wire
            .lines()
            .filter(|line| line.starts_with("content-length:"))
            .collect::<Vec<_>>();
        assert_eq!(lengths, expected, "{method} sent {sent:?}: {wire}");
        assert!(!wire.contains("transfer-encoding:"), "{wire}");
        server.await.expect("capture fixture server succeeds");
    }
}

#[tokio::test]
async fn opted_in_client_sends_the_context_of_its_named_client_span() {
    use opentelemetry::trace::{SpanKind, TracerProvider as _};
    use opentelemetry_sdk::{
        propagation::TraceContextPropagator,
        trace::{InMemorySpanExporter, SdkTracerProvider},
    };
    use tracing_subscriber::layer::SubscriberExt as _;

    opentelemetry::global::set_text_map_propagator(TraceContextPropagator::new());
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    let _guard = tracing::subscriber::set_default(subscriber);
    let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());

    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, captured, server) =
        tls_server_capture(&material, b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").await;
    let mut request = request();
    request.headers_mut().insert(
        "traceparent",
        "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01"
            .parse()
            .expect("header value"),
    );
    fixture_client(address, &material)
        .with_trace_context()
        .execute(request, deadline())
        .await
        .expect("captured request response");
    let wire = captured.await.expect("captured request");
    server.await.expect("capture fixture server succeeds");

    let spans = exporter.get_finished_spans().expect("exported spans");
    let [span] = spans.as_slice() else {
        panic!("one client span, got {spans:?}");
    };
    assert_eq!(span.name, "GET");
    assert_eq!(span.span_kind, SpanKind::Client);
    let context = &span.span_context;
    let expected = format!(
        "traceparent: 00-{}-{}-01\r\n",
        context.trace_id(),
        context.span_id()
    );
    let wire = wire.to_ascii_lowercase();
    assert!(wire.contains(&expected), "{wire} lacks {expected}");
    assert_eq!(wire.matches("traceparent:").count(), 1);
}

#[tokio::test]
async fn transport_source_redacts_request_and_response_content() {
    let material = TlsMaterial::new(FIXTURE_HOST);
    let (address, server) = tls_server(
        &material,
        b"HTTP/1.1 200 OK\r\nContent-Length: 23\r\n\r\nresponse-sentinel-body",
    )
    .await;
    let mut request =
        Request::get("https://provider.fixture.test/sentinel-path?token=request-query-sentinel")
            .body(Bytes::from_static(b"request-sentinel-body"))
            .expect("request with sentinel target");
    request.headers_mut().insert(
        header::AUTHORIZATION,
        "Bearer request-header-sentinel"
            .parse()
            .expect("credential header"),
    );
    let error = fixture_client(address, &material)
        .execute(request, deadline())
        .await
        .expect_err("truncated response fails");
    let mut diagnostics = String::new();
    for cause in shared_rules::error_chain::causes(&error) {
        write!(diagnostics, "{cause:?} {cause} ").expect("format diagnostics");
    }
    for sentinel in [
        "sentinel-path",
        "request-query-sentinel",
        "request-header-sentinel",
        "request-sentinel-body",
        "response-sentinel-body",
    ] {
        assert!(
            !diagnostics.contains(sentinel),
            "transport diagnostics leaked {sentinel}"
        );
    }
    server.await.expect("truncated fixture server succeeds");
}

#[tokio::test]
async fn collect_bounded_body_avoids_copying_single_frame_and_checks_context_between_frames() {
    use std::collections::VecDeque;
    use std::convert::Infallible;
    use std::pin::Pin;
    use std::task::Poll;

    use hyper::body::{Body, Frame};

    struct FrameBody {
        frames: VecDeque<Frame<Bytes>>,
        on_poll: Option<Box<dyn FnMut(usize) + Send>>,
        polls: usize,
    }

    impl Body for FrameBody {
        type Data = Bytes;
        type Error = Infallible;

        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
            let poll_index = self.polls;
            self.polls += 1;
            if let Some(on_poll) = self.on_poll.as_mut() {
                on_poll(poll_index);
            }
            Poll::Ready(self.frames.pop_front().map(Ok))
        }
    }

    let context: crate::RequestContext = operation_context::OperationContext::unbounded().into();
    let payload = Bytes::from(b"single-frame-payload".to_vec());
    let expected_ptr = payload.as_ptr();
    let single = FrameBody {
        frames: VecDeque::from([
            Frame::data(Bytes::new()),
            Frame::data(payload),
            Frame::data(Bytes::new()),
            Frame::trailers(http::HeaderMap::new()),
        ]),
        on_poll: None,
        polls: 0,
    };
    let buffered = crate::collect_bounded_body(single, 64, &context)
        .await
        .expect("single-frame body succeeds");
    assert_eq!(buffered.as_ref(), b"single-frame-payload");
    assert_eq!(buffered.as_ptr(), expected_ptr);

    let multi = FrameBody {
        frames: VecDeque::from([
            Frame::data(Bytes::from_static(b"ab")),
            Frame::data(Bytes::from_static(b"cd")),
            Frame::data(Bytes::from_static(b"ef")),
        ]),
        on_poll: None,
        polls: 0,
    };
    let concatenated = crate::collect_bounded_body(multi, 6, &context)
        .await
        .expect("multi-frame body at exact limit succeeds");
    assert_eq!(concatenated.as_ref(), b"abcdef");

    let over_limit = FrameBody {
        frames: VecDeque::from([
            Frame::data(Bytes::from_static(b"abcd")),
            Frame::data(Bytes::from_static(b"efg")),
        ]),
        on_poll: None,
        polls: 0,
    };
    std::assert_matches!(
        crate::collect_bounded_body(over_limit, 6, &context).await,
        Err(Error::ResponseBodyTooLarge)
    );

    let op = operation_context::OperationContext::unbounded();
    let cancel = op.clone();
    let cancelled_between_frames = FrameBody {
        frames: VecDeque::from([
            Frame::data(Bytes::from_static(b"first")),
            Frame::data(Bytes::from_static(b"second")),
        ]),
        on_poll: Some(Box::new(move |poll_index| {
            if poll_index == 0 {
                cancel.cancel();
            }
        })),
        polls: 0,
    };
    std::assert_matches!(
        crate::collect_bounded_body(cancelled_between_frames, 64, &op.into()).await,
        Err(Error::Cancelled)
    );
}
