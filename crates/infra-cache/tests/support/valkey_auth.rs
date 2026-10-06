//! Actual server authentication, isolated to one named user on the existing server.

use std::io::Write;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use infra_cache::Cache;
use redis::Value;
use tokio::io::{AsyncRead, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};

use super::{cache_url, observation_recorder, options, upstream_addr};

const EXCHANGE_BOUND: Duration = Duration::from_secs(2);
const REFRESH_BOUND: Duration = Duration::from_secs(7);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Material {
    Initial,
    Pending,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Authentication {
    Hello,
    Auth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Accepted,
    WrongPassword,
    Unexpected,
}

#[derive(Debug)]
struct Exchange {
    connection: u64,
    command: Authentication,
    material: Material,
    outcome: Outcome,
}

// Only bounded classifications leave the relay; synthetic passwords are never
// included in assertion diagnostics, logs, or metrics.
struct Passwords {
    username: String,
    initial: String,
    pending: String,
}

impl Passwords {
    fn classify(&self, request: &Value) -> Option<(Authentication, Material)> {
        let Value::Array(args) = request else {
            return None;
        };
        let (command, username, password) = match args.as_slice() {
            [
                Value::BulkString(command),
                Value::BulkString(username),
                Value::BulkString(password),
            ] if command.eq_ignore_ascii_case(b"AUTH") => {
                (Authentication::Auth, username, password)
            }
            [
                Value::BulkString(command),
                _,
                Value::BulkString(auth),
                Value::BulkString(username),
                Value::BulkString(password),
                ..,
            ] if command.eq_ignore_ascii_case(b"HELLO") && auth.eq_ignore_ascii_case(b"AUTH") => {
                (Authentication::Hello, username, password)
            }
            _ => return None,
        };
        let material = if username != self.username.as_bytes() {
            Material::Other
        } else if password == self.initial.as_bytes() {
            Material::Initial
        } else if password == self.pending.as_bytes() {
            Material::Pending
        } else {
            Material::Other
        };
        Some((command, material))
    }
}

/// Record exactly one parsed frame without reading bytes belonging to the next
/// pipelined reply. The installed RESP parser owns all framing and validation.
struct FrameReader<'a> {
    stream: &'a mut TcpStream,
    bytes: Vec<u8>,
}

impl AsyncRead for FrameReader<'_> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if output.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        let mut byte = [0];
        let mut input = ReadBuf::new(&mut byte);
        match Pin::new(&mut *self.stream).poll_read(cx, &mut input) {
            Poll::Ready(Ok(())) => {
                self.bytes.extend_from_slice(input.filled());
                output.put_slice(input.filled());
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}

#[allow(
    clippy::default_trait_access,
    reason = "redis's public parser accepts a transitive combine Decoder that redis does not re-export"
)]
async fn frame(stream: &mut TcpStream) -> redis::RedisResult<(Value, Vec<u8>)> {
    let mut reader = FrameReader {
        stream,
        bytes: Vec::new(),
    };
    let value = redis::parse_redis_value_async(&mut Default::default(), &mut reader).await?;
    Ok((value, reader.bytes))
}

struct AuthRelay {
    port: u16,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
}

impl AuthRelay {
    async fn start(passwords: Arc<Passwords>) -> (Self, mpsc::Receiver<Exchange>) {
        let upstream = upstream_addr(&cache_url());
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("relay bind");
        let port = listener.local_addr().expect("relay address").port();
        let (events_tx, events) = mpsc::channel(32);
        let (stop, mut stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            let mut connection = 0;
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    Some(result) = connections.join_next(), if !connections.is_empty() => {
                        result.expect("authentication relay task");
                    }
                    accepted = listener.accept() => {
                        let (inbound, _) = accepted.expect("relay accept");
                        connection += 1;
                        let passwords = passwords.clone();
                        let events = events_tx.clone();
                        connections.spawn(async move {
                            let Ok(outbound) = TcpStream::connect(upstream).await else { return };
                            relay_connection(inbound, outbound, connection, &passwords, &events).await;
                        });
                    }
                }
            }
            connections.abort_all();
            while connections.join_next().await.is_some() {}
        });
        (
            Self {
                port,
                stop: Some(stop),
                task,
            },
            events,
        )
    }

    async fn stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
            if let Ok(result) = tokio::time::timeout(EXCHANGE_BOUND, &mut self.task).await {
                result.expect("authentication relay failed");
            } else {
                self.task.abort();
                let _ = (&mut self.task).await;
                panic!("authentication relay did not stop");
            }
        }
    }
}

async fn next_exchange(events: &mut mpsc::Receiver<Exchange>) -> Exchange {
    tokio::time::timeout(REFRESH_BOUND, events.recv())
        .await
        .expect("adapter did not complete authentication against Valkey")
        .expect("authentication relay stopped")
}

impl Drop for AuthRelay {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn relay_connection(
    mut inbound: TcpStream,
    mut outbound: TcpStream,
    connection: u64,
    passwords: &Passwords,
    events: &mpsc::Sender<Exchange>,
) {
    loop {
        let Ok((request, bytes)) = frame(&mut inbound).await else {
            return;
        };
        if outbound.write_all(&bytes).await.is_err() {
            return;
        }
        loop {
            let Ok((reply, bytes)) = frame(&mut outbound).await else {
                return;
            };
            // Forward every server byte unchanged, including unsolicited RESP3
            // pushes. A push does not consume the request's reply slot.
            if inbound.write_all(&bytes).await.is_err() {
                return;
            }
            if matches!(reply, Value::Push { .. }) {
                continue;
            }
            if let Some((command, material)) = passwords.classify(&request) {
                let outcome = match (&command, &reply) {
                    (Authentication::Auth, Value::Okay)
                    | (Authentication::Hello, Value::Map(_)) => Outcome::Accepted,
                    (_, Value::ServerError(error)) if error.code() == "WRONGPASS" => {
                        Outcome::WrongPassword
                    }
                    _ => Outcome::Unexpected,
                };
                if events
                    .send(Exchange {
                        connection,
                        command,
                        material,
                        outcome,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
            }
            break;
        }
    }
}

#[derive(Clone, Default)]
struct CapturedLogs {
    bytes: Arc<Mutex<Vec<u8>>>,
    changed: Arc<Notify>,
}

impl CapturedLogs {
    fn rendered(&self) -> String {
        String::from_utf8(self.bytes.lock().expect("logs lock").clone()).expect("UTF-8 logs")
    }

    async fn wait_for(&self, message: &str) {
        tokio::time::timeout(EXCHANGE_BOUND, async {
            loop {
                if self.rendered().contains(message) {
                    return;
                }
                self.changed.notified().await;
            }
        })
        .await
        .expect("adapter did not observe the server's authentication result");
    }
}

impl Write for CapturedLogs {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.bytes
            .lock()
            .expect("logs lock")
            .extend_from_slice(bytes);
        self.changed.notify_one();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "fixture-owned atomic password publication completes before its server assertion"
)]
fn replace_password(path: &Path, password: &str) {
    let mut file = tempfile::NamedTempFile::new_in(path.parent().expect("password directory"))
        .expect("private password file");
    writeln!(file, "{password}").expect("write synthetic password");
    file.persist(path)
        .expect("atomically replace password file");
}

async fn admin<T: redis::FromRedisValue>(
    connection: &mut redis::aio::MultiplexedConnection,
    command: &redis::Cmd,
) -> T {
    tokio::time::timeout(EXCHANGE_BOUND, command.query_async(connection))
        .await
        .expect("fixture administration timed out")
        .unwrap_or_else(|_| {
            panic!("CACHE_URL requires disposable fixture ACL administration rights")
        })
}

fn assert_exchange(
    exchange: &Exchange,
    command: Authentication,
    material: Material,
    outcome: Outcome,
) {
    assert_eq!(exchange.command, command);
    assert_eq!(exchange.material, material);
    assert_eq!(exchange.outcome, outcome);
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one sequential authentication scenario keeps socket identity, pending-password transitions, and panic-safe fixture cleanup together"
)]
async fn replacement_password_is_authenticated_on_retained_and_new_connections() {
    // This real server boundary catches AUTH being skipped or a rejected AUTH
    // being reported as success; old sessions alone cannot distinguish either.
    let recorder = observation_recorder();
    let _recorder = metrics::set_default_local_recorder(&recorder);
    let logs = CapturedLogs::default();
    let writer = logs.clone();
    let _subscriber = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish(),
    );
    let directory = tempfile::tempdir().expect("private fixture directory");
    let identity = directory
        .path()
        .file_name()
        .expect("fixture identity")
        .to_string_lossy();
    let passwords = Arc::new(Passwords {
        username: format!("rotation_{identity}"),
        initial: format!("synthetic-initial-{identity}"),
        pending: format!("synthetic-pending-{identity}"),
    });
    let path = directory.path().join("password");
    replace_password(&path, &passwords.initial);
    let key = format!("{identity}:probe");
    let stored_key = format!("rotation_auth:{key}");
    let client = redis::Client::open(cache_url()).expect("fixture admin URL");
    let mut connection =
        tokio::time::timeout(EXCHANGE_BOUND, client.get_multiplexed_async_connection())
            .await
            .expect("fixture admin connection timed out")
            .expect("fixture admin connection");
    let (mut relay, mut events) = AuthRelay::start(passwords.clone()).await;
    let port = relay.port;
    let key_pattern = format!("~rotation_auth:{identity}:*");

    // The outer owner cleans up even when a scenario assertion panics. The
    // current-thread runtime keeps the local recorder/subscriber on every task.
    let test_passwords = passwords.clone();
    let mut test_connection = connection.clone();
    let test_logs = logs.clone();
    let handle = recorder.handle();
    let mut scenario = tokio::spawn(async move {
        admin::<()>(
            &mut test_connection,
            redis::cmd("ACL")
                .arg("SETUSER")
                .arg(&test_passwords.username)
                .arg("reset")
                .arg("on")
                .arg(format!(">{}", test_passwords.initial))
                .arg(key_pattern)
                .arg(&[
                    "+auth",
                    "+hello",
                    "+ping",
                    "+get",
                    "+set",
                    "+del",
                    "+select",
                    "+client|setinfo",
                ]),
        )
        .await;
        let cache = Cache::connect_lazy(infra_cache::CacheOptions {
            password_file: Some(path.clone()),
            allow_unauthenticated: false,
            ..options(
                format!("redis://{}@127.0.0.1:{port}", test_passwords.username),
                Duration::from_secs(1),
                None,
            )
        })
        .expect("admit authenticated password-file cache");
        let namespace = cache.namespace("rotation_auth");
        let first = next_exchange(&mut events).await;
        assert_exchange(
            &first,
            Authentication::Hello,
            Material::Initial,
            Outcome::Accepted,
        );
        namespace
            .set(&key, b"kept", Duration::from_secs(60))
            .await
            .expect("initial authenticated SET");

        replace_password(&path, &test_passwords.pending);
        let rejected = next_exchange(&mut events).await;
        assert_exchange(
            &rejected,
            Authentication::Auth,
            Material::Pending,
            Outcome::WrongPassword,
        );
        assert_eq!(
            rejected.connection, first.connection,
            "maintenance must use the retained connection"
        );
        test_logs.wait_for("cache_password_refresh_failed").await;
        assert!(!test_logs.rendered().contains("cache_password_reloaded"));
        let scrape = handle.render();
        assert!(scrape.contains(
            "cache_password_file_refreshes_total{outcome=\"refresh_failed\",reason=\"auth\"} 1"
        ));
        assert!(!scrape.contains("outcome=\"auth_accepted\""));

        // Only server policy changes: identical pending file bytes must be
        // retried. resetpass also rules out nopass and the retired password.
        admin::<()>(
            &mut test_connection,
            redis::cmd("ACL")
                .arg("SETUSER")
                .arg(&test_passwords.username)
                .arg("resetpass")
                .arg(format!(">{}", test_passwords.pending)),
        )
        .await;
        let accepted = next_exchange(&mut events).await;
        assert_exchange(
            &accepted,
            Authentication::Auth,
            Material::Pending,
            Outcome::Accepted,
        );
        assert_eq!(
            accepted.connection, first.connection,
            "new-session success cannot stand in for maintained AUTH"
        );
        test_logs.wait_for("cache_password_reloaded").await;
        assert!(handle.render().contains(
            "cache_password_file_refreshes_total{outcome=\"auth_accepted\",reason=\"none\"} 1"
        ));

        let old_reply = tokio::time::timeout(EXCHANGE_BOUND, async {
            let mut old = TcpStream::connect(upstream_addr(&cache_url()))
                .await
                .expect("fresh old-password socket");
            old.write_all(
                &redis::cmd("HELLO")
                    .arg(3)
                    .arg("AUTH")
                    .arg(&test_passwords.username)
                    .arg(&test_passwords.initial)
                    .get_packed_command(),
            )
            .await
            .expect("send old-password HELLO");
            frame(&mut old).await.expect("old-password reply").0
        })
        .await
        .expect("old-password authentication timed out");
        assert!(
            matches!(old_reply, Value::ServerError(ref error) if error.code() == "WRONGPASS"),
            "retired password must fail new authentication"
        );

        let killed: u64 = admin(
            &mut test_connection,
            redis::cmd("CLIENT")
                .arg("KILL")
                .arg("USER")
                .arg(&test_passwords.username),
        )
        .await;
        assert_eq!(
            killed, 1,
            "only the fixture's adapter connection should be open"
        );
        let reopened = next_exchange(&mut events).await;
        assert_exchange(
            &reopened,
            Authentication::Hello,
            Material::Pending,
            Outcome::Accepted,
        );
        assert_ne!(reopened.connection, first.connection);
        assert_eq!(
            namespace
                .get(&key)
                .await
                .expect("GET on replacement connection"),
            Some(b"kept".to_vec())
        );
        assert_eq!(
            test_logs
                .rendered()
                .matches("cache_password_reloaded")
                .count(),
            1
        );
        for secret in [&test_passwords.initial, &test_passwords.pending] {
            assert!(!test_logs.rendered().contains(secret));
            assert!(!handle.render().contains(secret));
        }
        drop(namespace);
        drop(cache);
    });
    let result = tokio::time::timeout(Duration::from_secs(30), &mut scenario).await;
    if result.is_err() {
        scenario.abort();
        let _ = scenario.await;
    }
    // DELUSER disconnects any remaining owned sessions, including after panic.
    let removed: u64 = admin(
        &mut connection,
        redis::cmd("ACL").arg("DELUSER").arg(&passwords.username),
    )
    .await;
    admin::<u64>(&mut connection, redis::cmd("DEL").arg(&stored_key)).await;
    relay.stop().await;
    result
        .expect("authenticated rotation scenario timed out")
        .expect("authenticated rotation scenario failed");
    assert_eq!(removed, 1, "remove exactly the owned ACL user");
}
