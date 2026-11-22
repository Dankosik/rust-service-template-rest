use std::pin::{Pin, pin};
use std::task::{Context, Wake, Waker};

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
            retired: Retirement::new(),
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
                    database_index: 0,
                },
                command_timeout: BUDGET,
                shared,
                application_slots: ApplicationSlots::new(APPLICATION_SLOTS),
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
            assert_eq!(maintenance.await, PingOutcome::Ok);
        })
        .await;
    }
}

async fn duplex_generation(cooldown_until: Instant) -> (Arc<Generation>, DuplexStream) {
    let (stream, server) = tokio::io::duplex(1024);
    let info = redis::RedisConnectionInfo::default().set_skip_set_lib_name();
    let config = redis::AsyncConnectionConfig::new()
        .set_response_timeout(None)
        .set_pipeline_buffer_size(APPLICATION_SLOTS + 1);
    let (connection, _driver) = timeout(
        BUDGET,
        MultiplexedConnection::new_with_config(&info, stream, config),
    )
    .await
    .expect("bound Redis setup")
    .expect("initialize Redis driver over duplex");
    let (abandoned, _replies) = mpsc::unbounded_channel();
    let generation = Arc::new(Generation {
        connection,
        caller_capacity: Arc::new(Semaphore::new(APPLICATION_SLOTS)),
        abandoned,
        retired: Retirement::new(),
        cooldown_until,
    });
    (generation, server)
}

#[tokio::test]
async fn retiring_past_cooldown_into_connecting_does_not_wake_waiters() {
    let (first, _server1) = duplex_generation(Instant::now()).await;
    let (second, _server2) = duplex_generation(Instant::now()).await;
    let shared = Shared {
        max_concurrency: APPLICATION_SLOTS,
        state: ArcSwap::from_pointee(State::Connected(first.clone())),
        state_lock: Mutex::new(()),
        changed: Notify::new(),
        retirements: Counter::noop(),
        tasks: TaskTracker::new(),
    };
    first.retired.retire();

    let mut waiting = pin!(shared.acquire());
    let wake = Arc::new(DriverWake::default());
    let waker = Waker::from(wake.clone());
    assert!(
        waiting
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );

    shared.retire(&first);
    assert!(
        !wake.0.load(Ordering::Relaxed),
        "storing State::Connecting woke a waiter that still cannot settle"
    );
    assert!(
        waiting
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );

    assert!(shared.publish(second.clone()));
    assert!(
        wake.0.load(Ordering::Relaxed),
        "publishing a replacement generation must wake parked waiters"
    );
    let acquired = waiting
        .await
        .expect("published generation must settle waiting acquire");
    assert!(Arc::ptr_eq(&acquired, &second));

    second.retired.retire();
    let mut waiting_failure = pin!(shared.acquire());
    let failure_wake = Arc::new(DriverWake::default());
    let failure_waker = Waker::from(failure_wake.clone());
    assert!(
        waiting_failure
            .as_mut()
            .poll(&mut Context::from_waker(&failure_waker))
            .is_pending()
    );
    shared.retire(&second);
    assert!(!failure_wake.0.load(Ordering::Relaxed));
    assert!(shared.record_failure(ErrorType::Auth));
    assert!(failure_wake.0.load(Ordering::Relaxed));
    assert_eq!(waiting_failure.await.err(), Some(ErrorType::Auth));

    let (third, _server3) = duplex_generation(Instant::now()).await;
    assert!(shared.publish(third.clone()));
    third.retired.retire();
    let mut waiting_close = pin!(shared.acquire());
    let close_wake = Arc::new(DriverWake::default());
    let close_waker = Waker::from(close_wake.clone());
    assert!(
        waiting_close
            .as_mut()
            .poll(&mut Context::from_waker(&close_waker))
            .is_pending()
    );
    shared.retire(&third);
    assert!(!close_wake.0.load(Ordering::Relaxed));
    shared.close();
    assert!(close_wake.0.load(Ordering::Relaxed));
    assert_eq!(waiting_close.await.err(), Some(ErrorType::Io));
}

#[tokio::test]
async fn retirement_wakes_pending_and_pre_created_waiters_idempotently() {
    let signal = Retirement::new();
    assert!(!signal.is_retired());

    let mut pre_created = pin!(signal.wait());
    let mut pending_a = pin!(signal.wait());
    let mut pending_b = pin!(signal.wait());
    let wake_a = Arc::new(DriverWake::default());
    let wake_b = Arc::new(DriverWake::default());
    assert!(
        pending_a
            .as_mut()
            .poll(&mut Context::from_waker(&Waker::from(wake_a.clone())))
            .is_pending()
    );
    assert!(
        pending_b
            .as_mut()
            .poll(&mut Context::from_waker(&Waker::from(wake_b.clone())))
            .is_pending()
    );

    signal.retire();
    assert!(signal.is_retired());
    assert!(wake_a.0.load(Ordering::Relaxed));
    assert!(wake_b.0.load(Ordering::Relaxed));
    signal.retire();
    assert!(signal.is_retired());
    assert!(futures_util::poll!(pending_a.as_mut()).is_ready());
    assert!(futures_util::poll!(pending_b.as_mut()).is_ready());
    assert!(futures_util::poll!(pre_created.as_mut()).is_ready());
    assert!(futures_util::poll!(pin!(signal.wait())).is_ready());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn retirement_wait_does_not_miss_concurrent_retire() {
    for _ in 0..200 {
        let signal = Retirement::new();
        let barrier = Arc::new(tokio::sync::Barrier::new(4));
        let waiters: Vec<_> = (0..3)
            .map(|_| {
                let signal = signal.clone();
                let barrier = barrier.clone();
                tokio::spawn(async move {
                    barrier.wait().await;
                    signal.wait().await;
                })
            })
            .collect();
        barrier.wait().await;
        signal.retire();
        timeout(BUDGET, async {
            for waiter in waiters {
                waiter.await.expect("waiter task completed");
            }
        })
        .await
        .expect("all concurrent waiters must observe retirement");
    }
}

#[test]
fn application_slots_bounds_capacity_and_releases_on_drop_and_unwind() {
    let slots = ApplicationSlots::new(2);
    let first = slots.try_acquire().expect("first slot");
    let second = slots.try_acquire().expect("second slot");
    assert!(slots.try_acquire().is_none());

    drop(first);
    let recovered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _held = slots.try_acquire().expect("reacquired slot");
        assert!(slots.try_acquire().is_none());
        panic!("simulated unwind while holding slot");
    }));
    assert!(recovered.is_err());

    let third = slots.try_acquire().expect("slot released on unwind");
    assert!(slots.try_acquire().is_none());
    drop(second);
    drop(third);
    let _a = slots.try_acquire().expect("first slot after full release");
    let _b = slots.try_acquire().expect("second slot after full release");
    assert!(slots.try_acquire().is_none());
}
