use std::path::PathBuf;
use std::time::Duration;

use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use secrecy::SecretString;
use tokio_util::task::AbortOnDropHandle;

use crate::{Cache, CacheError, CacheOptions, ClientCertificate};

mod admission;
mod connection;
mod observe;

fn options(
    dsn: &str,
    allow_plaintext: bool,
    allow_unauthenticated: bool,
    root_ca_path: Option<PathBuf>,
) -> CacheOptions {
    CacheOptions {
        dsn: SecretString::from(dsn.to_owned()),
        password_file: None,
        root_ca_path,
        client_certificate: None,
        allow_plaintext,
        allow_unauthenticated,
        command_timeout: Duration::from_millis(100),
    }
}

fn admitted(dsn: &str, allow_plaintext: bool, allow_unauthenticated: bool) -> Cache {
    on_runtime(|| {
        Cache::start(options(dsn, allow_plaintext, allow_unauthenticated, None))
            .expect("start connect admits without a server")
    })
}

fn on_runtime<T>(body: impl FnOnce() -> T) -> T {
    if tokio::runtime::Handle::try_current().is_ok() {
        return body();
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let _guard = runtime.enter();
    body()
}

fn with_password_file(dsn: &str, password_file: PathBuf) -> CacheOptions {
    CacheOptions {
        password_file: Some(password_file),
        ..options(dsn, true, false, None)
    }
}

fn observation_recorder() -> metrics_exporter_prometheus::PrometheusRecorder {
    let mut builder = PrometheusBuilder::new();
    for &(name, buckets) in crate::HISTOGRAMS {
        builder = builder
            .set_buckets_for_metric(Matcher::Full(name.to_owned()), buckets)
            .expect("observation buckets are valid");
    }
    builder.build_recorder()
}

/// A server that refuses authentication until told otherwise and answers
/// every `GET` with a miss. The client authenticates inside `HELLO 3`; the
/// server counts those attempts so tests can observe the complete setup retry
/// allowance. Each accepted connection records the primary
/// generation; a `SET` on an older generation is `READONLY`. With a
/// `password` set, authentication succeeds only with that password.
struct FakeServer {
    address: std::net::SocketAddr,
    accept_auth: std::sync::Arc<std::sync::atomic::AtomicBool>,
    password: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    auth_attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    primary: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    connections: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    sessions: std::sync::Arc<std::sync::Mutex<Vec<AbortOnDropHandle<()>>>>,
    observed: std::sync::Arc<SocketObservations>,
    listener: tokio::task::JoinHandle<()>,
}

impl FakeServer {
    async fn start() -> Self {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fake server bind");
        let address = listener.local_addr().expect("fake server address");
        let accept_auth = std::sync::Arc::new(AtomicBool::new(false));
        let password = std::sync::Arc::new(std::sync::Mutex::new(None));
        let auth_attempts = std::sync::Arc::new(AtomicUsize::new(0));
        let primary = std::sync::Arc::new(AtomicUsize::new(0));
        let connections = std::sync::Arc::new(AtomicUsize::new(0));
        let sessions = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (accept, expected, attempts, generation, accepted, open) = (
            accept_auth.clone(),
            password.clone(),
            auth_attempts.clone(),
            primary.clone(),
            connections.clone(),
            sessions.clone(),
        );
        let observed = std::sync::Arc::new(SocketObservations::default());
        let events = observed.clone();
        let listener = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let id = accepted.fetch_add(1, Ordering::SeqCst) + 1;
                let socket = ObservedSocket::new(id, events.clone());
                let recorded = generation.load(Ordering::SeqCst);
                let session = AbortOnDropHandle::new(tokio::spawn(serve_resp(
                    stream,
                    accept.clone(),
                    expected.clone(),
                    attempts.clone(),
                    recorded,
                    generation.clone(),
                    socket,
                )));
                open.lock().expect("sessions lock").push(session);
            }
        });
        Self {
            address,
            accept_auth,
            password,
            auth_attempts,
            primary,
            connections,
            sessions,
            observed,
            listener,
        }
    }

    /// Close every open connection; the listener keeps accepting.
    fn hang_up(&self) {
        self.sessions.lock().expect("sessions lock").clear();
    }

    /// Accept only this password from now on.
    fn require_password(&self, password: &str) {
        *self.password.lock().expect("password lock") = Some(password.to_owned());
    }

    fn attempts(&self) -> usize {
        self.auth_attempts.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn connections(&self) -> usize {
        self.connections.load(std::sync::atomic::Ordering::SeqCst)
    }
}

fn resp_arguments(values: Vec<redis::Value>) -> Option<Vec<String>> {
    values
        .into_iter()
        .map(|value| match value {
            redis::Value::BulkString(bytes) => Some(String::from_utf8_lossy_owned(bytes)),
            _ => None,
        })
        .collect()
}

async fn serve_resp(
    mut stream: tokio::net::TcpStream,
    accept_auth: std::sync::Arc<std::sync::atomic::AtomicBool>,
    password: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    auth_attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    generation: usize,
    primary: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    socket: ObservedSocket,
) {
    use std::sync::atomic::Ordering;
    use tokio::io::AsyncWriteExt;

    #[expect(
        clippy::default_trait_access,
        reason = "redis does not re-export the decoder type used by its parser"
    )]
    let mut decoder = Default::default();
    while let Ok(redis::Value::Array(values)) =
        redis::parse_redis_value_async(&mut decoder, &mut stream).await
    {
        let Some(arguments) = resp_arguments(values) else {
            return;
        };
        socket
            .observed
            .commands
            .lock()
            .expect("commands lock")
            .push((socket.id, arguments.clone()));
        socket.observed.changed.notify_waiters();
        if socket.observed.silence_all.load(Ordering::SeqCst)
            || socket.id <= socket.observed.silence_through.load(Ordering::SeqCst)
        {
            // Keep reading to detect client EOF and count dispatched commands,
            // but never answer a slot on a deliberately stalled connection.
            continue;
        }
        if arguments.first().is_some_and(|command| command == "AUTH")
            && socket.observed.silence_auth.load(Ordering::SeqCst)
        {
            // Keep observing the socket's actual EOF while AUTH has no reply.
            continue;
        }
        let auth_error = format!(
            "-WRONGPASS {}\r\n",
            socket.observed.auth_error.lock().expect("auth error lock")
        );
        let authenticates = match arguments.first().map(String::as_str) {
            Some("AUTH") => true,
            Some("HELLO") => arguments.iter().any(|argument| argument == "AUTH"),
            _ => false,
        };
        let reply: &[u8] = match arguments.first().map(String::as_str) {
            _ if authenticates => {
                auth_attempts.fetch_add(1, Ordering::SeqCst);
                let accepted = match password.lock().expect("password lock").as_ref() {
                    Some(expected) => arguments.last() == Some(expected),
                    None => accept_auth.load(Ordering::SeqCst),
                } && socket
                    .observed
                    .username
                    .lock()
                    .expect("username lock")
                    .as_ref()
                    .is_none_or(|expected| arguments.get(arguments.len() - 2) == Some(expected));
                if accepted {
                    socket
                        .observed
                        .auth_successes
                        .fetch_add(1, Ordering::SeqCst);
                    socket.observed.changed.notify_waiters();
                    b"+OK\r\n"
                } else {
                    socket
                        .observed
                        .auth_rejections
                        .fetch_add(1, Ordering::SeqCst);
                    socket.observed.changed.notify_waiters();
                    auth_error.as_bytes()
                }
            }
            Some("GET")
                if arguments
                    .get(1)
                    .is_some_and(|key| key.ends_with(":wrongtype")) =>
            {
                b"-WRONGTYPE Operation against a key holding the wrong kind of value\r\n"
            }
            Some("GET") => b"$-1\r\n",
            Some("PING") if socket.observed.deny_ping.load(Ordering::SeqCst) => {
                b"-NOPERM no permissions to run the 'ping' command\r\n"
            }
            Some("PING") => b"+PONG\r\n",
            Some("DEL") => b":1\r\n",
            Some("SET") => {
                if generation == primary.load(Ordering::SeqCst) {
                    b"+OK\r\n"
                } else {
                    b"-READONLY You can't write against a read only replica.\r\n"
                }
            }
            _ => b"+OK\r\n",
        };
        let reply_delay = socket.observed.reply_delay(&arguments);
        if !reply_delay.is_zero() {
            tokio::time::sleep(reply_delay).await;
        }
        if arguments.first().is_some_and(|command| command == "SET") {
            socket.observed.wait_for_set_reply_release().await;
        }
        if stream.write_all(reply).await.is_err() {
            return;
        }
    }
}

fn ephemeral_password() -> String {
    let mut password_bytes = [0_u8; 32];
    let random = rustls::crypto::aws_lc_rs::default_provider().secure_random;
    random
        .fill(&mut password_bytes)
        .expect("ephemeral password");
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, password_bytes)
}

#[derive(Default)]
struct SocketObservations {
    active: std::sync::Mutex<std::collections::HashSet<usize>>,
    commands: std::sync::Mutex<Vec<(usize, Vec<String>)>>,
    silence_through: std::sync::atomic::AtomicUsize,
    silence_all: std::sync::atomic::AtomicBool,
    silence_auth: std::sync::atomic::AtomicBool,
    deny_ping: std::sync::atomic::AtomicBool,
    auth_successes: std::sync::atomic::AtomicUsize,
    auth_rejections: std::sync::atomic::AtomicUsize,
    auth_error: std::sync::Mutex<String>,
    username: std::sync::Mutex<Option<String>>,
    auth_reply_delay: std::sync::Mutex<Duration>,
    ping_reply_delays: std::sync::Mutex<std::collections::VecDeque<Duration>>,
    next_set_reply: std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    /// Reply delay of a `GET` whose key ends in `:slow`.
    slow_get_delay: std::sync::Mutex<Duration>,
    changed: tokio::sync::Notify,
}

impl SocketObservations {
    async fn wait_for_set_reply_release(&self) {
        let release = self
            .next_set_reply
            .lock()
            .expect("SET reply gate lock")
            .take();
        if let Some(release) = release {
            let _ = release.await;
        }
    }

    /// How long the fake server holds the reply to `arguments`.
    fn reply_delay(&self, arguments: &[String]) -> Duration {
        match arguments.first().map(String::as_str) {
            Some("AUTH") => *self.auth_reply_delay.lock().expect("delay lock"),
            Some("GET") if arguments.get(1).is_some_and(|key| key.ends_with(":slow")) => {
                *self.slow_get_delay.lock().expect("delay lock")
            }
            Some("PING") => self
                .ping_reply_delays
                .lock()
                .expect("delay lock")
                .pop_front()
                .unwrap_or_default(),
            _ => Duration::ZERO,
        }
    }
}

struct ObservedSocket {
    id: usize,
    observed: std::sync::Arc<SocketObservations>,
}

impl ObservedSocket {
    fn new(id: usize, observed: std::sync::Arc<SocketObservations>) -> Self {
        observed.active.lock().expect("active lock").insert(id);
        observed.changed.notify_waiters();
        Self { id, observed }
    }
}

impl Drop for ObservedSocket {
    fn drop(&mut self) {
        self.observed
            .active
            .lock()
            .expect("active lock")
            .remove(&self.id);
        self.observed.changed.notify_waiters();
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.listener.abort();
        self.hang_up();
    }
}

impl FakeServer {
    async fn wait_for(&self, budget: Duration, reason: &str, ready: impl Fn() -> bool) {
        tokio::time::timeout(budget, async {
            loop {
                let changed = self.observed.changed.notified();
                if ready() {
                    break;
                }
                changed.await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{reason}"));
    }

    fn active(&self) -> usize {
        self.observed.active.lock().expect("active lock").len()
    }

    fn command_count(&self, command: &str, key: Option<&str>) -> usize {
        self.observed
            .commands
            .lock()
            .expect("commands lock")
            .iter()
            .filter(|(_, args)| {
                args.first().is_some_and(|name| name == command)
                    && key.is_none_or(|key| args.get(1).is_some_and(|arg| arg == key))
            })
            .count()
    }

    fn stall_existing(&self) -> usize {
        let through = self.connections();
        self.observed
            .silence_through
            .store(through, std::sync::atomic::Ordering::SeqCst);
        through
    }

    fn closed_through(&self, through: usize) -> bool {
        self.observed
            .active
            .lock()
            .expect("active lock")
            .iter()
            .all(|id| *id > through)
    }
}
