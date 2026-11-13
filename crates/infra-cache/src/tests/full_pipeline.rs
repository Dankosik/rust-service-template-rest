use std::pin::pin;
use std::sync::atomic::Ordering;
use std::task::Wake;

use health::Probe;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

use super::*;
use crate::{Cache, CacheNamespace};

const BUDGET: Duration = Duration::from_secs(30);

struct FullPipeline {
    cache: Cache,
    generation: Arc<Generation>,
    replies: Pin<Box<dyn Future<Output = ()>>>,
    driver: Pin<Box<dyn Future<Output = ()>>>,
    server: DuplexStream,
}

impl FullPipeline {
    async fn new() -> Self {
        let (stream, server) = tokio::io::duplex(32 * 1024);
        let info = redis::RedisConnectionInfo::default().set_skip_set_lib_name();
        let config = redis::AsyncConnectionConfig::new()
            .set_response_timeout(None)
            .set_pipeline_buffer_size(PIPELINE_BUFFER_SIZE);
        let (connection, driver) = timeout(
            BUDGET,
            MultiplexedConnection::new_with_config(&info, stream, config),
        )
        .await
        .expect("bound Redis setup")
        .expect("initialize Redis driver over duplex");
        let (abandoned, replies) = mpsc::unbounded_channel();
        let generation = Arc::new(Generation {
            connection,
            caller_capacity: Arc::new(Semaphore::new(CALLER_PIPELINE_CAPACITY)),
            abandoned,
            retired: CancellationToken::new(),
        });
        let shared = Arc::new(Shared {
            state: Mutex::new(State::Connected(generation.clone())),
            changed: Notify::new(),
            retirements: Counter::noop(),
        });
        let finished = tokio::spawn(async {});
        let supervisor = finished.abort_handle();
        finished.await.expect("fixture has no running supervisor");
        let cache = Cache {
            command_timeout: BUDGET,
            link: Arc::new(Link {
                server: ServerEndpoint {
                    host: "duplex".into(),
                    port: 0,
                    tls: false,
                },
                shared,
                application_slots: Semaphore::new(APPLICATION_SLOTS),
                probe_slots: Semaphore::new(PROBE_SLOTS),
                admission_refused: Counter::noop(),
                commands_in_flight: Gauge::noop(),
                supervisor,
            }),
        };
        Self {
            cache,
            generation,
            replies: Box::pin(drain_abandoned_replies(replies)),
            driver: Box::pin(tokio::task::unconstrained(driver)),
            server,
        }
    }

    fn poll_driver(&mut self, waker: &Waker) {
        assert!(
            self.driver
                .as_mut()
                .poll(&mut Context::from_waker(waker))
                .is_pending()
        );
    }

    async fn answer_gets_and_ping(&mut self) {
        let mut expected = redis::cmd("GET");
        expected.arg("test:peer");
        let mut expected = expected.get_packed_command();
        let mut abandoned = redis::cmd("GET");
        abandoned.arg("test:abandoned");
        expected.extend(abandoned.get_packed_command().repeat(APPLICATION_SLOTS));
        expected.extend(redis::Cmd::ping().get_packed_command());
        let mut actual = vec![0; expected.len()];
        timeout(BUDGET, self.server.read_exact(&mut actual))
            .await
            .expect("driver writes the queued GETs and maintenance PING")
            .expect("read requests");
        assert_eq!(actual, expected);
        let mut replies = b"$4\r\nlive\r\n".to_vec();
        replies.extend(b"$-1\r\n".repeat(APPLICATION_SLOTS));
        replies.extend(b"+PONG\r\n");
        timeout(BUDGET, self.server.write_all(&replies))
            .await
            .expect("bound reply writes")
            .expect("answer queued requests");
        self.poll_driver(Waker::noop());
        poll_pending(self.replies.as_mut()).await;
    }
}

async fn poll_pending<F: Future + ?Sized>(mut future: Pin<&mut F>) {
    poll_fn(|cx| {
        assert!(future.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
}

async fn fill_with_cancelled_commands(namespace: &CacheNamespace) {
    // The live GET already holds one place. The unpolled driver retains the
    // abandoned sends, and the maintenance PING fills the last place.
    for _ in 0..APPLICATION_SLOTS {
        let mut abandoned = pin!(namespace.get("abandoned"));
        poll_pending(abandoned.as_mut()).await;
    }
}

#[derive(Default)]
struct DriverWake(std::sync::atomic::AtomicBool);

impl Wake for DriverWake {
    fn wake(self: Arc<Self>) {
        self.0.store(true, Ordering::Relaxed);
    }
}

#[tokio::test]
async fn dropping_a_mutation_waiting_for_capacity_does_not_enqueue_it() {
    for delete in [false, true] {
        tokio::task::unconstrained(async {
            let mut pipeline = FullPipeline::new().await;
            let namespace = pipeline.cache.namespace("test");
            let generation = pipeline.generation.clone();
            let mut peer = pin!(namespace.get("peer"));
            poll_pending(peer.as_mut()).await;
            fill_with_cancelled_commands(&namespace).await;
            let mut maintenance = pin!(ping_until(&generation, BUDGET));
            poll_pending(maintenance.as_mut()).await;

            let mut mutation = Box::pin(async {
                if delete {
                    namespace.delete("cancelled").await.expect("DEL");
                } else {
                    namespace
                        .set("cancelled", b"value", Duration::from_secs(1))
                        .await
                        .expect("SET");
                }
            });
            poll_pending(mutation.as_mut()).await;

            let wake = Arc::new(DriverWake::default());
            let waker = Waker::from(wake.clone());
            pipeline.poll_driver(&waker);
            // The driver has drained the queue and is listening for another
            // send. Drop without giving the mutation a normal poll.
            drop(mutation);
            assert!(
                !wake.0.load(Ordering::Relaxed),
                "cancelling a blocked mutation woke the driver with a new send"
            );
            pipeline.answer_gets_and_ping().await;
            assert_eq!(peer.await, Ok(Some(b"live".to_vec())));
            assert_eq!(maintenance.await, Ok(()));
        })
        .await;
    }
}

#[tokio::test]
async fn dropping_a_probe_waiting_for_capacity_keeps_the_peer_command() {
    tokio::task::unconstrained(async {
        let mut pipeline = FullPipeline::new().await;
        let namespace = pipeline.cache.namespace("test");
        let generation = pipeline.generation.clone();
        let mut peer = pin!(namespace.get("peer"));
        poll_pending(peer.as_mut()).await;
        fill_with_cancelled_commands(&namespace).await;
        let mut maintenance = pin!(ping_until(&generation, BUDGET));
        poll_pending(maintenance.as_mut()).await;

        let probe = pipeline.cache.probe();
        let mut check = probe.check();
        poll_pending(check.as_mut()).await;
        drop(check);
        poll_pending(peer.as_mut()).await;

        pipeline.poll_driver(Waker::noop());
        pipeline.answer_gets_and_ping().await;
        assert_eq!(peer.await, Ok(Some(b"live".to_vec())));
        assert_eq!(maintenance.await, Ok(()));
    })
    .await;
}
