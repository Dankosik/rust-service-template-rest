//! Wire faults over the real broker; no publication or settlement is simulated.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

pub(crate) struct AckDroppingRelay {
    pub(crate) url: String,
    pub(crate) dropped_ack: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

/// One NATS connection relay that observes a broker authorization refusal and
/// retains only the latest public CONNECT subject. It neither validates nor
/// supplies authentication.
pub(crate) struct AuthObservingRelay {
    pub(crate) url: String,
    close_first: CancellationToken,
    stop: CancellationToken,
    first_closed: Arc<AtomicBool>,
    refusals: Arc<AtomicUsize>,
    connections: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}

impl AuthObservingRelay {
    pub(crate) async fn start(target_url: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("authentication relay listener");
        let address = listener.local_addr().expect("authentication relay address");
        let target = relay_target(target_url);
        let close_first = CancellationToken::new();
        let stop = CancellationToken::new();
        let first_closed = Arc::new(AtomicBool::new(false));
        let refusals = Arc::new(AtomicUsize::new(0));
        let connections = Arc::new(Mutex::new(Vec::new()));
        let (
            close_for_task,
            stop_for_task,
            completion_flag,
            refusals_for_task,
            connections_for_task,
        ) = (
            close_first.clone(),
            stop.clone(),
            Arc::clone(&first_closed),
            Arc::clone(&refusals),
            Arc::clone(&connections),
        );
        let task = tokio::spawn(async move {
            let mut first = true;
            loop {
                let accepted = tokio::select! {
                    () = stop_for_task.cancelled() => break,
                    accepted = listener.accept() => accepted,
                };
                let (client, _) = accepted.expect("authentication relay accepts client");
                let broker = TcpStream::connect(&target)
                    .await
                    .expect("authentication relay reaches broker");
                let close = if first {
                    close_for_task.clone()
                } else {
                    CancellationToken::new()
                };
                relay_authenticated_connection(
                    client,
                    broker,
                    &stop_for_task,
                    &close,
                    Arc::clone(&refusals_for_task),
                    Arc::clone(&connections_for_task),
                )
                .await;
                if first {
                    completion_flag.store(true, Ordering::SeqCst);
                    first = false;
                }
            }
        });
        Self {
            url: format!("nats://{address}"),
            close_first,
            stop,
            first_closed,
            refusals,
            connections,
            task,
        }
    }

    pub(crate) async fn close_old_connection(&self) {
        self.close_first.cancel();
        timeout(Duration::from_secs(5), async {
            while !self.first_closed.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("relay closes the original authenticated connection");
    }

    pub(crate) async fn wait_for_broker_refusal(&self) {
        timeout(Duration::from_secs(10), async {
            while self.refusals.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("broker forwards an authentication refusal");
    }

    pub(crate) async fn wait_for_latest_public_user(&self, user: &str) {
        timeout(Duration::from_secs(10), async {
            loop {
                let matches = {
                    let observed = self
                        .connections
                        .lock()
                        .expect("authentication relay connection lock");
                    observed.last().is_some_and(|observed| observed == user)
                };
                if matches {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("relay observes the expected public user on its live connection");
    }

    #[must_use]
    pub(crate) fn latest_public_user(&self) -> Option<String> {
        self.connections
            .lock()
            .expect("authentication relay connection lock")
            .last()
            .cloned()
    }

    pub(crate) async fn finish(self) {
        self.stop.cancel();
        timeout(Duration::from_secs(3), self.task)
            .await
            .expect("authentication relay stops")
            .expect("authentication relay task completes");
    }
}

impl AckDroppingRelay {
    pub(crate) async fn start(stream: &str) -> Self {
        let stream = stream.to_owned();
        let mut dropped = false;
        Self::start_filtering(move |payload| {
            if !dropped && is_stream_publish_ack(payload, &stream) {
                dropped = true;
                true
            } else {
                false
            }
        })
        .await
    }

    pub(crate) async fn start_filtering(
        mut drop_reply: impl FnMut(&[u8]) -> bool + Send + 'static,
    ) -> Self {
        Self::start_with_filters(|_, _| false, move |_, payload| drop_reply(payload)).await
    }

    /// Withholds the first source +ACK request, or its broker confirmation.
    pub(crate) async fn start_source_ack(stream: &str, durable: &str, drop_request: bool) -> Self {
        let identity = format!(".{stream}.{durable}.");
        let reply_subject = Arc::new(Mutex::new(None::<String>));
        let sent_reply = Arc::clone(&reply_subject);
        let mut seen_request = false;
        Self::start_with_filters(
            move |line, payload| {
                let fields = std::str::from_utf8(line)
                    .unwrap_or_default()
                    .split_ascii_whitespace()
                    .collect::<Vec<_>>();
                if !seen_request
                    && fields.first() == Some(&"PUB")
                    && fields.get(1).is_some_and(|subject| {
                        subject.starts_with("$JS.ACK.") && subject.contains(&identity)
                    })
                    && fields.len() == 4
                    && payload == b"+ACK"
                {
                    seen_request = true;
                    *sent_reply.lock().expect("ACK reply lock") = Some(fields[2].to_owned());
                    return drop_request;
                }
                false
            },
            move |line, payload| {
                let subject = std::str::from_utf8(line)
                    .unwrap_or_default()
                    .split_ascii_whitespace()
                    .nth(1);
                let mut reply = reply_subject.lock().expect("ACK reply lock");
                if !drop_request && payload.is_empty() && subject == reply.as_deref() {
                    reply.take();
                    return true;
                }
                false
            },
        )
        .await
    }

    async fn start_with_filters(
        to_broker: impl FnMut(&[u8], &[u8]) -> bool + Send + 'static,
        to_client: impl FnMut(&[u8], &[u8]) -> bool + Send + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay listener");
        let address = listener.local_addr().expect("relay address");
        let target = relay_target(&std::env::var("NATS_URL").expect("NATS_URL"));
        let dropped_ack = Arc::new(AtomicBool::new(false));
        let dropped = Arc::clone(&dropped_ack);
        let task = tokio::spawn(async move {
            let (client, _) = listener.accept().await.expect("relay client");
            let broker = TcpStream::connect(target).await.expect("relay broker");
            let (client_read, client_write) = client.into_split();
            let (broker_read, broker_write) = broker.into_split();
            let requests = relay_frames(client_read, broker_write, to_broker, Arc::clone(&dropped));
            let replies = relay_frames(broker_read, client_write, to_client, dropped);
            // Closing either peer ends both directions, with no detached relay task.
            tokio::select! {
                result = requests => result.expect("relay requests"),
                result = replies => result.expect("relay replies"),
            }
        });
        Self {
            url: format!("nats://{address}"),
            dropped_ack,
            task,
        }
    }

    pub(crate) async fn wait_for_drop(&self) {
        timeout(Duration::from_secs(5), async {
            let mut cadence = tokio::time::interval(Duration::from_millis(10));
            while !self.dropped_ack.load(Ordering::SeqCst) {
                cadence.tick().await;
            }
        })
        .await
        .expect("configured ACK crosses the relay");
    }

    pub(crate) async fn join(self) {
        timeout(Duration::from_secs(3), self.task)
            .await
            .expect("relay finishes after client close")
            .expect("relay task completes");
        assert!(
            self.dropped_ack.load(Ordering::SeqCst),
            "configured ACK was withheld"
        );
    }
}

/// A broker publication acknowledgement for the expected stream. The parser
/// keeps the relay from confusing unrelated JSON replies with publish truth.
pub(crate) fn is_stream_publish_ack(payload: &[u8], stream: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(payload)
        .is_ok_and(|reply| reply["stream"] == stream && reply["seq"].is_u64())
}

pub(crate) fn relay_target(url: &str) -> String {
    let authority = url
        .strip_prefix("nats://")
        .expect("integration NATS scheme")
        .rsplit('@')
        .next()
        .expect("broker authority")
        .trim_end_matches('/');
    assert!(
        !authority.is_empty() && !authority.contains('/'),
        "one broker address"
    );
    authority.to_owned()
}

async fn relay_frames(
    reader: impl AsyncRead + Unpin,
    mut writer: impl AsyncWrite + Unpin,
    mut withhold: impl FnMut(&[u8], &[u8]) -> bool,
    dropped: Arc<AtomicBool>,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(reader);
    loop {
        let mut line = Vec::new();
        if reader.read_until(b'\n', &mut line).await? == 0 {
            return Ok(());
        }
        let Some(length) = payload_length(&line) else {
            writer.write_all(&line).await?;
            continue;
        };
        let mut payload = vec![0; length];
        reader.read_exact(&mut payload).await?;
        let mut ending = [0; 2];
        reader.read_exact(&mut ending).await?;
        if withhold(&line, &payload) {
            dropped.store(true, Ordering::SeqCst);
        } else {
            writer.write_all(&line).await?;
            writer.write_all(&payload).await?;
            writer.write_all(&ending).await?;
        }
    }
}

async fn relay_authenticated_connection(
    client: TcpStream,
    broker: TcpStream,
    stop: &CancellationToken,
    close: &CancellationToken,
    refusals: Arc<AtomicUsize>,
    connections: Arc<Mutex<Vec<String>>>,
) {
    let (client_read, client_write) = client.into_split();
    let (broker_read, broker_write) = broker.into_split();
    let requests = async move {
        let mut client_read = BufReader::new(client_read);
        let mut broker_write = broker_write;
        let mut connect = Vec::new();
        if client_read.read_until(b'\n', &mut connect).await? == 0 {
            return Ok::<(), std::io::Error>(());
        }
        if let Some(user) = public_user_from_connect(&connect) {
            connections
                .lock()
                .expect("authentication relay connection lock")
                .push(user);
        }
        broker_write.write_all(&connect).await?;
        tokio::io::copy(&mut client_read, &mut broker_write)
            .await
            .map(|_| ())
    };
    let replies = async move {
        let mut broker_read = BufReader::new(broker_read);
        let mut client_write = client_write;
        loop {
            let mut line = Vec::new();
            if broker_read.read_until(b'\n', &mut line).await? == 0 {
                return Ok::<(), std::io::Error>(());
            }
            if is_broker_auth_refusal(&line) {
                refusals.fetch_add(1, Ordering::SeqCst);
            }
            client_write.write_all(&line).await?;
            if let Some(length) = payload_length(&line) {
                let mut payload = vec![0; length];
                broker_read.read_exact(&mut payload).await?;
                let mut ending = [0; 2];
                broker_read.read_exact(&mut ending).await?;
                client_write.write_all(&payload).await?;
                client_write.write_all(&ending).await?;
            }
        }
    };
    tokio::pin!(requests);
    tokio::pin!(replies);
    tokio::select! {
        () = stop.cancelled() => {},
        () = close.cancelled() => {},
        _ = &mut requests => {},
        _ = &mut replies => {},
    }
}

/// NATS's broker-owned authorization refusal remains the test oracle. Do not
/// treat an arbitrary protocol error, a timeout, or a parser failure as proof.
fn is_broker_auth_refusal(line: &[u8]) -> bool {
    line.starts_with(b"-ERR")
        && (line
            .windows(b"Authorization Violation".len())
            .any(|window| window == b"Authorization Violation")
            || line
                .windows(b"Authentication".len())
                .any(|window| window == b"Authentication"))
}

/// Extracts only the fixture user's public subject from the CONNECT JWT.
/// The relay does not retain the JWT, seed, nonce, or signature.
fn public_user_from_connect(line: &[u8]) -> Option<String> {
    use base64::Engine as _;

    let connect = line.strip_prefix(b"CONNECT ")?;
    let connect = serde_json::from_slice::<serde_json::Value>(connect).ok()?;
    let jwt = connect.get("jwt")?.as_str()?;
    let payload = jwt.split('.').nth(1)?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice::<serde_json::Value>(&payload)
        .ok()?
        .get("sub")?
        .as_str()
        .map(ToOwned::to_owned)
}

fn payload_length(line: &[u8]) -> Option<usize> {
    let mut fields = std::str::from_utf8(line).ok()?.split_ascii_whitespace();
    match fields.next()? {
        "PUB" | "HPUB" | "MSG" | "HMSG" => fields.last()?.parse().ok(),
        _ => None,
    }
}
