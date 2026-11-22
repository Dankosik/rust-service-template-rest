use super::*;
use aws_smithy_async::rt::sleep::Sleep;
use aws_smithy_async::time::SystemTimeSource;
use aws_smithy_runtime_api::client::dns::{DnsFuture, ResolveDns};
use std::net::{IpAddr, Ipv4Addr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

const TEST_BOUND: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct ControlledSleep(mpsc::UnboundedSender<(Duration, oneshot::Sender<()>)>);

impl AsyncSleep for ControlledSleep {
    fn sleep(&self, duration: Duration) -> Sleep {
        let (finish, elapsed) = oneshot::channel();
        self.0.send((duration, finish)).unwrap();
        Sleep::new(async move {
            let _ = elapsed.await;
        })
    }
}

fn components() -> (
    RuntimeComponents,
    mpsc::UnboundedReceiver<(Duration, oneshot::Sender<()>)>,
) {
    let (sender, timers) = mpsc::unbounded_channel();
    let runtime = RuntimeComponentsBuilder::for_tests()
        .with_sleep_impl(Some(ControlledSleep(sender)))
        .with_time_source(Some(SystemTimeSource::new()))
        .build()
        .unwrap();
    (runtime, timers)
}

fn rustls_builder() -> Builder<TlsProviderSelected> {
    Builder::new().tls_provider(tls::Provider::Rustls(
        tls::rustls_provider::CryptoMode::AwsLc,
    ))
}

fn request(
    client: &SharedHttpClient,
    components: &RuntimeComponents,
    uri: &str,
    budget: Duration,
) -> HttpConnectorFuture {
    let settings = HttpConnectorSettings::builder()
        .connect_timeout(budget)
        .build();
    client
        .http_connector(&settings, components)
        .call(HttpRequest::get(uri).unwrap())
}

#[derive(Clone, Debug)]
struct FirstLookupSucceeds {
    attempts: Arc<AtomicUsize>,
    pending: mpsc::UnboundedSender<oneshot::Receiver<()>>,
}

impl ResolveDns for FirstLookupSucceeds {
    fn resolve_dns<'a>(&'a self, _: &'a str) -> DnsFuture<'a> {
        if self.attempts.fetch_add(1, Ordering::Relaxed) == 0 {
            return DnsFuture::ready(Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)]));
        }
        let (dropped, pending) = oneshot::channel();
        self.pending.send(pending).unwrap();
        DnsFuture::new(async move {
            let _dropped = dropped;
            std::future::pending().await
        })
    }
}

async fn read_request(socket: &mut TcpStream) {
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") {
        request.push(socket.read_u8().await.unwrap());
    }
}

async fn reply(socket: &mut TcpStream) {
    socket
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
        .await
        .unwrap();
}

#[tokio::test]
async fn varying_budgets_share_checkout_and_background_dial_keeps_its_timer() {
    tokio::time::timeout(TEST_BOUND, async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!(
            "http://checkout.test:{}",
            listener.local_addr().unwrap().port()
        );
        let (pending, mut lookups) = mpsc::unbounded_channel();
        let client = rustls_builder()
            .per_request_connect_timeout()
            .build_with_resolver(FirstLookupSucceeds {
                attempts: Arc::default(),
                pending,
            });
        let (first_runtime, mut first_timers) = components();
        let (second_runtime, mut second_timers) = components();
        let first_budget = Duration::from_micros(731_137);
        let second_budget = Duration::from_micros(418_021);

        let first = tokio::spawn(request(&client, &first_runtime, &uri, first_budget));
        let (observed, _first_timer) = first_timers.recv().await.unwrap();
        assert_eq!(observed, first_budget);
        let (mut connection, _) = listener.accept().await.unwrap();
        read_request(&mut connection).await;

        let second = tokio::spawn(request(&client, &second_runtime, &uri, second_budget));
        let (observed, finish_dial) = second_timers.recv().await.unwrap();
        assert_eq!(observed, second_budget);
        let mut dial_closed = lookups.recv().await.unwrap();
        reply(&mut connection).await;
        assert_eq!(first.await.unwrap().unwrap().status().as_u16(), 200);
        read_request(&mut connection).await;
        reply(&mut connection).await;
        assert_eq!(second.await.unwrap().unwrap().status().as_u16(), 200);

        assert_eq!(
            dial_closed.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        );
        finish_dial.send(()).unwrap();
        assert!(dial_closed.await.is_err());

        let warm_budget = Duration::from_micros(239_633);
        let warm = request(&client, &second_runtime, &uri, warm_budget);
        let server = async {
            read_request(&mut connection).await;
            reply(&mut connection).await;
        };
        let (response, ()) = tokio::join!(warm, server);
        assert_eq!(response.unwrap().status().as_u16(), 200);
        assert!(second_timers.try_recv().is_err());
    })
    .await
    .expect("shared checkout and its background dial must finish");
}

#[tokio::test]
async fn cold_tls_timeout_and_abandonment_close_the_socket() {
    tokio::time::timeout(TEST_BOUND, async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let uri = format!("https://{}", listener.local_addr().unwrap());
        let client = rustls_builder().per_request_connect_timeout().build_https();
        let (runtime, mut timers) = components();

        for (budget, expire) in [(261_147, true), (791_003, false)] {
            let budget = Duration::from_micros(budget);
            let mut request = Box::pin(request(&client, &runtime, &uri, budget));
            let (mut peer, finish) = tokio::select! {
                result = &mut request => panic!("stalled TLS finished: {result:?}"),
                connected = async {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    assert_eq!(stream.read_u8().await.unwrap(), 22);
                    let (observed, finish) = timers.recv().await.unwrap();
                    assert_eq!(observed, budget);
                    (stream, finish)
                } => connected,
            };
            let abandoned_timer = if expire {
                finish.send(()).unwrap();
                let error = request.as_mut().await.unwrap_err();
                assert!(error.is_timeout(), "{error:?}");
                assert!(!error.is_io(), "{error:?}");
                None
            } else {
                Some(finish)
            };
            drop(request);
            if let Some(finish) = abandoned_timer {
                assert!(finish.is_closed());
            }
            let mut bytes = Vec::new();
            peer.read_to_end(&mut bytes).await.unwrap();
        }
    })
    .await
    .expect("stalled TLS sockets must close within the test bound");
}
