//! Disposable Technical Design probe; copied temporarily into test/tests.+//! It proves the selected driver seam, not the completed adapter.
#![cfg(feature = "integration")]
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::{net::{Ipv4Addr, SocketAddr}, num::NonZeroU32, sync::{Arc, atomic::{AtomicUsize, Ordering}}, time::Duration};
use infra_postgres::{Dsn, Isolation, PgPool, PoolOptions, SessionBudgets};
use integration_tests::{DATABASE_URL, url_for};
use tokio::{net::{TcpListener, TcpStream}, sync::{watch, Notify}, task::JoinHandle, time::{Instant, timeout}};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

struct Relay {
    addr: SocketAddr,
    silence: watch::Sender<u64>,
    count: Arc<AtomicUsize>,
    changed: Arc<Notify>,
    stop: CancellationToken,
    accept: JoinHandle<()>,
    sessions: TaskTracker,
}

impl Relay {
    async fn start(server: SocketAddr) -> Self {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (silence, _) = watch::channel(0_u64);
        let count = Arc::new(AtomicUsize::new(0));
        let changed = Arc::new(Notify::new());
        let stop = CancellationToken::new();
        let sessions = TaskTracker::new();
        let (signal, stopped, tracked, silenced, notify) = (silence.clone(), stop.clone(), sessions.clone(), count.clone(), changed.clone());
        let accept = tokio::spawn(async move {
            loop {
                let accepted = tokio::select! {
                    () = stopped.cancelled() => break,
                    accepted = listener.accept() => accepted,
                };
                let (mut client, _) = accepted.unwrap();
                let (mut signal, stopped, silenced, notify) = (signal.subscribe(), stopped.clone(), silenced.clone(), notify.clone());
                tracked.spawn(async move {
                    let mut upstream = TcpStream::connect(server).await.unwrap();
                    tokio::select! {
                        () = stopped.cancelled() => {},
                        _ = tokio::io::copy_bidirectional(&mut client, &mut upstream) => {},
                        _ = signal.changed() => {
                            silenced.fetch_add(1, Ordering::SeqCst);
                            notify.notify_one();
                            stopped.cancelled().await;
                        },
                    }
                });
            }
        });
        Self { addr, silence, count, changed, stop, accept, sessions }
    }

    async fn silence_old(&self) {
        let previous = self.count.load(Ordering::SeqCst);
        self.silence.send_modify(|generation| *generation += 1);
        timeout(Duration::from_secs(1), async {
            while self.count.load(Ordering::SeqCst) == previous {
                self.changed.notified().await;
            }
        }).await.expect("relay acknowledged silence");
    }

    async fn finish(self) {
        self.stop.cancel();
        self.accept.await.unwrap();
        self.sessions.close();
        timeout(Duration::from_secs(1), self.sessions.wait()).await.unwrap();
    }
}

async fn setup(admin: &PgPool) -> (Relay, PgPool) {
    let mut url = url_for(admin, DATABASE_URL).await;
    let server = tokio::net::lookup_host((url.host_str().unwrap(), url.port().unwrap())).await.unwrap().next().unwrap();
    let relay = Relay::start(server).await;
    url.set_ip_host(relay.addr.ip()).unwrap();
    url.set_port(Some(relay.addr.port())).unwrap();
    let pool = infra_postgres::connect(&Dsn::admit(url.as_str()).unwrap(), &PoolOptions {
        max_connections: NonZeroU32::MIN,
        application_name: "pool-design-probe",
        default_isolation: Isolation::ServerDefault,
        session_budgets: SessionBudgets::Startup,
    }).await.unwrap();
    (relay, pool)
}

async fn active(admin: &PgPool, pid: i32) {
    timeout(Duration::from_secs(2), async {
        loop {
            let active: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid = $1 AND state = 'active' AND query LIKE '%pg_sleep%')")
                .bind(pid).fetch_one(admin).await.unwrap();
            if active { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("server observed active SQL");
}

async fn release(mut conn: sqlx::pool::PoolConnection<sqlx::Postgres>) -> bool {
    let deadline = Instant::now() + Duration::from_secs(1);
    let returned = conn.return_to_pool();
    drop(conn);
    tokio::time::timeout_at(deadline, returned).await.is_ok()
}

#[sqlx::test]
async fn driver_release_decision(admin: PgPool) {
    let version: String = sqlx::query_scalar("SHOW server_version").fetch_one(&admin).await.unwrap();
    println!("PROBE server_version={version} driver=sqlx-0.9.0 max_connections=1 acquire_seconds=3 release_seconds=1");

    for bounded in [false, true] {
        let (relay, pool) = setup(&admin).await;
        let mut conn = pool.acquire().await.unwrap();
        let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *conn).await.unwrap();
        {
            let statement = sqlx::query("SELECT pg_sleep(20)").execute(&mut *conn);
            tokio::pin!(statement);
            tokio::select! {
                answer = &mut statement => panic!("statement ended before cancellation: {answer:?}"),
                () = active(&admin, pid) => {},
            }
            relay.silence_old().await;
        }
        let cancelled_at = Instant::now();
        if bounded {
            assert!(!release(conn).await, "silent release must hit its deadline");
            assert_eq!(pool.size(), 0, "timeout drops capacity guard");
            let mut replacement = pool.acquire().await.unwrap();
            let answer: i32 = sqlx::query_scalar("SELECT 1").fetch_one(&mut *replacement).await.unwrap();
            assert_eq!(answer, 1);
            println!("PROBE bounded_active_cancel recovery_ms={} pool_size={} ordinary_query=1", cancelled_at.elapsed().as_millis(), pool.size());
            assert!(cancelled_at.elapsed() < Duration::from_secs(3));
            assert!(release(replacement).await);
        } else {
            drop(conn);
            let result = pool.acquire().await;
            assert!(matches!(result, Err(sqlx::Error::PoolTimedOut)));
            assert_eq!(pool.size(), 1);
            assert_eq!(pool.num_idle(), 0);
            println!("PROBE native_negative_control wait_ms={} pool_size=1 idle=0 result=PoolTimedOut", cancelled_at.elapsed().as_millis());
        }
        relay.finish().await;
        timeout(Duration::from_secs(3), pool.close()).await.unwrap();
    }

    let (relay, pool) = setup(&admin).await;
    let mut conn = pool.acquire().await.unwrap();
    let first: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *conn).await.unwrap();
    assert!(release(conn).await);
    let mut conn = pool.acquire().await.unwrap();
    let second: i32 = sqlx::query_scalar("SELECT pg_backend_pid()").fetch_one(&mut *conn).await.unwrap();
    assert_eq!(first, second);
    println!("PROBE healthy_return same_backend=true");
    relay.silence_old().await;
    let started = Instant::now();
    assert!(!release(conn).await);
    assert_eq!(pool.size(), 0);
    println!("PROBE silent_after_success release_ms={} pool_size=0", started.elapsed().as_millis());
    let mut conn = pool.acquire().await.unwrap();
    let one: i32 = sqlx::query_scalar("SELECT 1").fetch_one(&mut *conn).await.unwrap();
    assert_eq!(one, 1);
    relay.silence_old().await;
    let started = Instant::now();
    drop(conn.return_to_pool());
    drop(conn);
    assert_eq!(pool.size(), 0);
    println!("PROBE unpolled_discard elapsed_us={} pool_size=0", started.elapsed().as_micros());
    relay.finish().await;
    timeout(Duration::from_secs(3), pool.close()).await.unwrap();
}
