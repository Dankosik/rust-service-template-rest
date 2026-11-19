use std::pin::pin;
use std::sync::atomic::Ordering;
use std::task::{Wake, Waker};

use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

use super::*;
use crate::{Cache, CacheNamespace, namespace};

const BUDGET: Duration = Duration::from_secs(30);
const APPLICATION_SLOTS: usize = 2;
const ABANDONED_COMMANDS: usize = APPLICATION_SLOTS - 1;

struct FullPipeline {
    cache: Cache,
    generation: Arc<Generation>,
    replies: Pin<Box<dyn Future<Output = ()>>>,
    driver: Pin<Box<dyn Future<Output = ()>>>,
    server: DuplexStream,
}

impl FullPipeline {
    async fn new() -> Self {
        let (stream, server) = tokio::io::duplex(64 * 1024);
        let info = redis::RedisConnectionInfo::default().set_skip_set_lib_name();
        let config = redis::AsyncConnectionConfig::new()
            .set_response_timeout(None)
            .set_pipeline_buffer_size(APPLICATION_SLOTS + 1);
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
            caller_capacity: Arc::new(Semaphore::new(APPLICATION_SLOTS)),
            abandoned,
            retired: CancellationToken::new(),
            cooldown_until: Instant::now(),
        });
        let shared = Arc::new(Shared {
            max_concurrency: APPLICATION_SLOTS,
            state: ArcSwap::from_pointee(State::Connected(generation.clone())),
            state_lock: Mutex::new(()),
            changed: Notify::new(),
            retirements: Counter::noop(),
            tasks: TaskTracker::new(),
        });
        let finished = tokio::spawn(async {});
        let supervisor = finished.abort_handle();
        finished.await.expect("fixture has no running supervisor");
        let cache = Cache {
            max_key_bytes: 128,
            max_value_bytes: 32,
            link: Arc::new(Link {
                server: ServerEndpoint {
                    host: "duplex".into(),
                    port: 0,
                    tls: false,
                    database_index: 0,
                },
                command_timeout: BUDGET,
                shared,
                application_slots: Semaphore::new(APPLICATION_SLOTS),
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
        let read = |key: &str| {
            redis::pipe()
                .atomic()
                .cmd("EXISTS")
                .arg(key)
                .cmd("GETRANGE")
                .arg(key)
                .arg(0)
                .arg(32)
                .get_packed_pipeline()
        };
        let mut expected = read("test:peer");
        expected.extend(read("test:abandoned").repeat(ABANDONED_COMMANDS));
        expected.extend(redis::Cmd::ping().get_packed_command());
        let mut actual = vec![0; expected.len()];
        timeout(BUDGET, self.server.read_exact(&mut actual))
            .await
            .expect("driver writes the queued GETs and maintenance PING")
            .expect("read requests");
        assert_eq!(actual, expected);
        let mut replies = b"+OK\r\n+QUEUED\r\n+QUEUED\r\n*2\r\n:1\r\n$4\r\nlive\r\n".to_vec();
        replies.extend(
            b"+OK\r\n+QUEUED\r\n+QUEUED\r\n*2\r\n:0\r\n$0\r\n\r\n".repeat(ABANDONED_COMMANDS),
        );
        replies.extend(b"+PONG\r\n");
        timeout(BUDGET, self.server.write_all(&replies))
            .await
            .expect("bound reply writes")
            .expect("answer queued requests");
        self.poll_driver(Waker::noop());
        assert!(futures_util::poll!(self.replies.as_mut()).is_pending());
    }
}

async fn fill_with_cancelled_commands(namespace: &CacheNamespace) {
    let caller = OperationContext::unbounded();
    for _ in 0..ABANDONED_COMMANDS {
        let mut abandoned = pin!(namespace.get(&caller, "abandoned"));
        assert!(futures_util::poll!(abandoned.as_mut()).is_pending());
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
    let caller = OperationContext::unbounded();
    for delete in [false, true] {
        tokio::task::unconstrained(async {
            let mut pipeline = FullPipeline::new().await;
            let namespace = pipeline.cache.namespace(namespace!("test"));
            let generation = pipeline.generation.clone();
            let mut peer = pin!(namespace.get(&caller, "peer"));
            assert!(futures_util::poll!(peer.as_mut()).is_pending());
            fill_with_cancelled_commands(&namespace).await;
            let mut maintenance = pin!(ping_generation(&generation, BUDGET));
            assert!(futures_util::poll!(maintenance.as_mut()).is_pending());

            let mut mutation = Box::pin(async {
                if delete {
                    namespace.delete(&caller, "cancelled").await.expect("DEL");
                } else {
                    namespace
                        .set(&caller, "cancelled", b"value", Duration::from_secs(1))
                        .await
                        .expect("SET");
                }
            });
            assert!(futures_util::poll!(mutation.as_mut()).is_pending());

            let wake = Arc::new(DriverWake::default());
            let waker = Waker::from(wake.clone());
            pipeline.poll_driver(&waker);
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
