#![expect(clippy::expect_used, reason = "relay failures are test failures")]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

pub async fn eventually<T>(
    budget: Duration,
    what: &str,
    mut probe: impl AsyncFnMut() -> Option<T>,
) -> T {
    timeout(budget, async {
        loop {
            if let Some(value) = probe().await {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect(what)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DroppedAck {
    Request,
    Reply,
}

#[derive(Debug)]
pub struct AckDroppingRelay {
    pub url: String,
    pub dropped_ack: CancellationToken,
    task: JoinHandle<()>,
}

#[derive(Debug, Default)]
struct AuthObservations {
    first_closed: bool,
    refused: bool,
    latest_public_user: Option<String>,
}

#[derive(Debug)]
pub struct AuthObservingRelay {
    pub url: String,
    close_first: CancellationToken,
    stop: CancellationToken,
    observations: watch::Sender<AuthObservations>,
    task: JoinHandle<()>,
}

impl AuthObservingRelay {
    pub async fn start(target_url: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("authentication relay listener");
        let address = listener.local_addr().expect("authentication relay address");
        let target = relay_target(target_url);
        let close_first = CancellationToken::new();
        let stop = CancellationToken::new();
        let observations = watch::Sender::<AuthObservations>::default();
        let observations_for_task = observations.clone();
        let close_for_task = close_first.clone();
        let stop_for_task = stop.clone();
        let task = tokio::spawn(async move {
            let never_close = CancellationToken::new();
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
                let close = if first { &close_for_task } else { &never_close };
                relay_authenticated_connection(
                    client,
                    broker,
                    &stop_for_task,
                    close,
                    &observations_for_task,
                )
                .await;
                if first {
                    observations_for_task.send_modify(|state| state.first_closed = true);
                    first = false;
                }
            }
        });
        Self {
            url: format!("nats://{address}"),
            close_first,
            stop,
            observations,
            task,
        }
    }

    pub async fn close_old_connection(&self) {
        self.close_first.cancel();
        self.wait_for(
            Duration::from_secs(5),
            |state| state.first_closed,
            "relay closes the original authenticated connection",
        )
        .await;
    }

    pub async fn wait_for_broker_refusal(&self) {
        self.wait_for(
            Duration::from_secs(10),
            |state| state.refused,
            "broker forwards an authentication refusal",
        )
        .await;
    }

    pub async fn wait_for_latest_public_user(&self, user: &str) {
        self.wait_for(
            Duration::from_secs(10),
            |state| state.latest_public_user.as_deref() == Some(user),
            "relay observes the expected public user on its live connection",
        )
        .await;
    }

    async fn wait_for(
        &self,
        bound: Duration,
        predicate: impl FnMut(&AuthObservations) -> bool,
        diagnostic: &'static str,
    ) {
        let mut updates = self.observations.subscribe();
        timeout(bound, updates.wait_for(predicate))
            .await
            .expect(diagnostic)
            .expect("relay holds the observation sender");
    }

    #[must_use]
    pub fn latest_public_user(&self) -> Option<String> {
        self.observations.borrow().latest_public_user.clone()
    }

    pub async fn finish(self) {
        self.stop.cancel();
        timeout(Duration::from_secs(3), self.task)
            .await
            .expect("authentication relay stops")
            .expect("authentication relay task completes");
    }
}

enum ReplyAckState {
    WaitingForRequest,
    WaitingForReply(String),
    Dropped,
}

impl AckDroppingRelay {
    pub async fn start(stream: &str) -> Self {
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

    pub async fn start_filtering(
        mut drop_reply: impl FnMut(&[u8]) -> bool + Send + 'static,
    ) -> Self {
        Self::start_withholding(|_, _| false, move |_, payload| drop_reply(payload)).await
    }

    pub async fn intercept_second_pull(
        subject: String,
    ) -> (Self, oneshot::Receiver<(String, Bytes)>) {
        let (pull_tx, pull_rx) = oneshot::channel();
        let mut pull_tx = Some(pull_tx);
        let mut pulls = 0;
        let relay = Self::start_withholding(
            move |header, payload| {
                if header.verb != "PUB" || header.subject != subject {
                    return false;
                }
                pulls += 1;
                if pulls != 2 {
                    return false;
                }
                let reply = header.reply.expect("pull request has a reply inbox");
                pull_tx
                    .take()
                    .expect("only one pull is intercepted")
                    .send((reply.to_owned(), Bytes::copy_from_slice(payload)))
                    .expect("test is waiting for the second pull");
                true
            },
            |_, _| false,
        )
        .await;
        (relay, pull_rx)
    }

    pub async fn start_source_ack(stream: &str, durable: &str, dropped: DroppedAck) -> Self {
        let identity = format!(".{stream}.{durable}.");
        match dropped {
            DroppedAck::Request => {
                let mut dropped_request = false;
                Self::start_withholding(
                    move |header, payload| {
                        if !dropped_request
                            && source_ack_reply(header, payload, &identity).is_some()
                        {
                            dropped_request = true;
                            return true;
                        }
                        false
                    },
                    |_, _| false,
                )
                .await
            }
            DroppedAck::Reply => {
                let state = Arc::new(Mutex::new(ReplyAckState::WaitingForRequest));
                let request_state = Arc::clone(&state);
                Self::start_withholding(
                    move |header, payload| {
                        if let Some(reply) = source_ack_reply(header, payload, &identity) {
                            let mut state = request_state.lock().expect("ACK reply lock");
                            if matches!(*state, ReplyAckState::WaitingForRequest) {
                                *state = ReplyAckState::WaitingForReply(reply.to_owned());
                            }
                        }
                        false
                    },
                    move |header, payload| {
                        let mut state = state.lock().expect("ACK reply lock");
                        let matched = matches!(
                            &*state,
                            ReplyAckState::WaitingForReply(expected)
                                if payload.is_empty() && header.subject == expected
                        );
                        if matched {
                            *state = ReplyAckState::Dropped;
                            return true;
                        }
                        false
                    },
                )
                .await
            }
        }
    }

    async fn start_withholding(
        mut withhold_to_broker: impl FnMut(&PayloadHeader<'_>, &[u8]) -> bool + Send + 'static,
        mut withhold_to_client: impl FnMut(&PayloadHeader<'_>, &[u8]) -> bool + Send + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("relay listener");
        let address = listener.local_addr().expect("relay address");
        let target = relay_target(&std::env::var("NATS_URL").expect("NATS_URL"));
        let dropped_ack = CancellationToken::new();
        let dropped = dropped_ack.clone();
        let task = tokio::spawn(async move {
            let (client, _) = listener.accept().await.expect("relay client");
            let broker = TcpStream::connect(target).await.expect("relay broker");
            let (client_read, client_write) = client.into_split();
            let (broker_read, broker_write) = broker.into_split();
            let requests = relay_frames(client_read, broker_write, |_, payload| {
                if let Some((header, payload)) = payload
                    && withhold_to_broker(header, payload)
                {
                    dropped.cancel();
                    FrameAction::Drop
                } else {
                    FrameAction::Forward
                }
            });
            let replies = relay_frames(broker_read, client_write, |_, payload| {
                if let Some((header, payload)) = payload
                    && withhold_to_client(header, payload)
                {
                    dropped.cancel();
                    FrameAction::Drop
                } else {
                    FrameAction::Forward
                }
            });
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

    pub async fn wait_for_drop(&self) {
        timeout(Duration::from_secs(5), self.dropped_ack.cancelled())
            .await
            .expect("configured ACK crosses the relay");
    }

    pub async fn join(self) {
        timeout(Duration::from_secs(3), self.task)
            .await
            .expect("relay finishes after client close")
            .expect("relay task completes");
        assert!(
            self.dropped_ack.is_cancelled(),
            "configured ACK was withheld"
        );
    }
}

#[must_use]
pub fn is_stream_publish_ack(payload: &[u8], stream: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(payload)
        .is_ok_and(|reply| reply["stream"] == stream && reply["seq"].is_u64())
}

#[must_use]
pub fn relay_target(url: &str) -> String {
    let after_scheme = url
        .strip_prefix("nats://")
        .expect("integration NATS scheme");
    let authority = after_scheme
        .rsplit_once('@')
        .map_or(after_scheme, |(_, authority)| authority)
        .trim_end_matches('/');
    assert!(
        !authority.is_empty() && !authority.contains('/'),
        "one broker address"
    );
    authority.to_owned()
}

enum FrameAction {
    Forward,
    Drop,
}

async fn relay_frames(
    reader: impl AsyncRead + Unpin,
    mut writer: impl AsyncWrite + Unpin,
    mut on_frame: impl FnMut(&[u8], Option<(&PayloadHeader<'_>, &[u8])>) -> FrameAction,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(reader);
    loop {
        let mut line = Vec::new();
        if reader.read_until(b'\n', &mut line).await? == 0 {
            return Ok(());
        }
        let Some(header) = parse_payload_header(&line) else {
            if matches!(on_frame(&line, None), FrameAction::Forward) {
                writer.write_all(&line).await?;
            }
            continue;
        };
        let (payload, ending) = read_payload_and_ending(&mut reader, header.length).await?;
        if matches!(
            on_frame(&line, Some((&header, &payload))),
            FrameAction::Forward
        ) {
            writer.write_all(&line).await?;
            writer.write_all(&payload).await?;
            writer.write_all(&ending).await?;
        }
    }
}

async fn read_payload_and_ending(
    reader: &mut (impl AsyncRead + Unpin),
    length: usize,
) -> std::io::Result<(Vec<u8>, [u8; 2])> {
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload).await?;
    let mut ending = [0; 2];
    reader.read_exact(&mut ending).await?;
    Ok((payload, ending))
}

async fn relay_authenticated_requests(
    client_read: impl AsyncRead + Unpin,
    mut broker_write: impl AsyncWrite + Unpin,
    observations: &watch::Sender<AuthObservations>,
) -> std::io::Result<()> {
    let mut client_read = BufReader::new(client_read);
    let mut connect = Vec::new();
    if client_read.read_until(b'\n', &mut connect).await? == 0 {
        return Ok(());
    }
    if let Some(user) = public_user_from_connect(&connect) {
        observations.send_modify(|state| state.latest_public_user = Some(user));
    }
    broker_write.write_all(&connect).await?;
    tokio::io::copy(&mut client_read, &mut broker_write)
        .await
        .map(|_| ())
}

async fn relay_authenticated_connection(
    client: TcpStream,
    broker: TcpStream,
    stop: &CancellationToken,
    close: &CancellationToken,
    observations: &watch::Sender<AuthObservations>,
) {
    let (client_read, client_write) = client.into_split();
    let (broker_read, broker_write) = broker.into_split();
    let requests = relay_authenticated_requests(client_read, broker_write, observations);
    let replies = relay_frames(broker_read, client_write, |line, _| {
        if is_broker_auth_refusal(line) {
            observations.send_modify(|state| state.refused = true);
        }
        FrameAction::Forward
    });
    tokio::select! {
        () = stop.cancelled() => {},
        () = close.cancelled() => {},
        _ = requests => {},
        _ = replies => {},
    }
}

fn source_ack_reply<'a>(
    header: &PayloadHeader<'a>,
    payload: &[u8],
    identity: &str,
) -> Option<&'a str> {
    if header.verb == "PUB"
        && header.subject.starts_with("$JS.ACK.")
        && header.subject.contains(identity)
        && payload == b"+ACK"
    {
        header.reply
    } else {
        None
    }
}

fn is_broker_auth_refusal(line: &[u8]) -> bool {
    line.starts_with(b"-ERR")
        && (line
            .array_windows()
            .any(|window| window == b"Authorization Violation")
            || line
                .array_windows()
                .any(|window| window == b"Authentication"))
}

fn public_user_from_connect(line: &[u8]) -> Option<String> {
    let connect = line.strip_prefix(b"CONNECT ")?;
    let connect = serde_json::from_slice::<serde_json::Value>(connect).ok()?;
    jwt_subject(connect.get("jwt")?.as_str()?)
}

pub fn jwt_subject(jwt: &str) -> Option<String> {
    use base64::Engine as _;

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

struct PayloadHeader<'a> {
    verb: &'a str,
    subject: &'a str,
    reply: Option<&'a str>,
    length: usize,
}

fn parse_payload_header(line: &[u8]) -> Option<PayloadHeader<'_>> {
    let mut fields = std::str::from_utf8(line).ok()?.split_ascii_whitespace();
    let verb = fields.next()?;
    let subject = fields.next()?;
    let (reply, length) = match verb {
        "PUB" => match (fields.next()?, fields.next(), fields.next()) {
            (length, None, None) => (None, length),
            (reply, Some(length), None) => (Some(reply), length),
            _ => return None,
        },
        "HPUB" => match (fields.next()?, fields.next()?, fields.next(), fields.next()) {
            (_, length, None, None) => (None, length),
            (reply, _, Some(length), None) => (Some(reply), length),
            _ => return None,
        },
        "MSG" => match (fields.next()?, fields.next()?, fields.next(), fields.next()) {
            (_, length, None, None) => (None, length),
            (_, reply, Some(length), None) => (Some(reply), length),
            _ => return None,
        },
        "HMSG" => match (
            fields.next()?,
            fields.next()?,
            fields.next()?,
            fields.next(),
            fields.next(),
        ) {
            (_, _, length, None, None) => (None, length),
            (_, reply, _, Some(length), None) => (Some(reply), length),
            _ => return None,
        },
        _ => return None,
    };
    Some(PayloadHeader {
        verb,
        subject,
        reply,
        length: length.parse().ok()?,
    })
}
