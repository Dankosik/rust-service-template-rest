//! Wire faults over the real broker; no publication or settlement is simulated.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;
use tokio::time::timeout;

pub(crate) struct AckDroppingRelay {
    pub(crate) url: String,
    pub(crate) dropped_ack: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl AckDroppingRelay {
    pub(crate) async fn start(stream: &str) -> Self {
        let stream = stream.to_owned();
        let mut dropped = false;
        Self::start_filtering(move |payload| {
            let is_ack = is_stream_publish_ack(payload, &stream);
            if !dropped && is_ack {
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

fn payload_length(line: &[u8]) -> Option<usize> {
    let mut fields = std::str::from_utf8(line).ok()?.split_ascii_whitespace();
    match fields.next()? {
        "PUB" | "HPUB" | "MSG" | "HMSG" => fields.last()?.parse().ok(),
        _ => None,
    }
}

pub(crate) fn is_stream_publish_ack(payload: &[u8], stream: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(payload)
        .is_ok_and(|reply| reply["stream"] == stream && reply["seq"].is_u64())
}
