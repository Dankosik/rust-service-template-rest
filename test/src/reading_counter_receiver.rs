//! Independent, bounded NATS and HTTP receivers for the reading-counter recipe.

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use futures_util::StreamExt;
use sqlx::PgPool;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_util::sync::CancellationToken;

use crate::reading_counter::{self, Channel, Operation};

/// Receiver failures propagate to the fixture process owner.
pub type ReceiverError = Box<dyn std::error::Error + Send + Sync>;

#[allow(
    clippy::print_stdout,
    reason = "JSON lines are the fixture protocol consumed by the process driver"
)]
fn emit(value: serde_json::Value) {
    println!("{value}");
}

/// A fixture-only barrier after durable readback and before transport completion.
#[derive(Clone, Debug)]
pub struct CompletionHold {
    /// The fixed receiver channel to interrupt.
    pub channel: Channel,
    /// The one immutable operation whose completion is held.
    pub operation_id: String,
    /// Created after the marker has been read back.
    pub ready: PathBuf,
    /// Creating this file releases the hold before its five-second ceiling.
    pub release: PathBuf,
}

/// Explicit process inputs; the driver owns their disposable resources.
#[derive(Debug)]
pub struct ReceiverOptions {
    /// Existing integration broker.
    pub nats_url: String,
    /// Existing run-scoped stream.
    pub stream: String,
    /// Run-scoped event subject.
    pub subject: String,
    /// Stable durable identity across receiver restarts.
    pub consumer: String,
    /// Loopback listener only.
    pub http_bind: SocketAddr,
    /// Finite admission window, at most 250 seconds, leaving shutdown headroom.
    pub run_for: Duration,
    /// Optional lost-completion barrier.
    pub hold: Option<CompletionHold>,
}

async fn hold_completion(
    hold: Option<&CompletionHold>,
    channel: Channel,
    operation: &Operation,
) -> Result<(), ReceiverError> {
    let Some(hold) =
        hold.filter(|h| h.channel == channel && h.operation_id == operation.operation_id)
    else {
        return Ok(());
    };
    let receipt = serde_json::to_vec(&serde_json::json!({
        "status": "effect_committed", "channel": channel,
        "operation_id": operation.operation_id,
    }))?;
    tokio::fs::write(&hold.ready, receipt).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !tokio::fs::try_exists(&hold.release).await? {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    Ok(())
}

async fn record(
    pool: &PgPool,
    channel: Channel,
    operation: &Operation,
    hold: Option<&CompletionHold>,
) -> Result<(), ReceiverError> {
    // apply_effect owns CommitUnknown reconciliation. Even its acknowledged
    // path is independently read back before this receiver confirms delivery.
    let _effect = reading_counter::apply_effect(pool, channel, operation).await?;
    let effect = reading_counter::read_effect(pool, channel, operation)
        .await?
        .ok_or("committed receiver marker is absent")?;
    emit(serde_json::json!({"status": "effect", "effect": effect}));
    hold_completion(hold, channel, operation).await
}

async fn receive_nats(
    pool: &PgPool,
    consumer: async_nats::jetstream::consumer::PullConsumer,
    stop: &CancellationToken,
    hold: Option<&CompletionHold>,
) -> Result<(), ReceiverError> {
    // One outstanding broker delivery and one active database operation.
    let mut messages = consumer
        .stream()
        .max_messages_per_batch(1)
        .messages()
        .await?;
    loop {
        let message = tokio::select! {
            biased;
            () = stop.cancelled() => return Ok(()),
            message = messages.next() => message.ok_or("NATS receiver stream ended")??,
        };
        let result = tokio::time::timeout(Duration::from_secs(12), async {
            if message.payload.len() > 1024 {
                return Err::<(), ReceiverError>("operation exceeds one KiB".into());
            }
            let operation: Operation = serde_json::from_slice(&message.payload)?;
            record(pool, Channel::Outbox, &operation, hold).await?;
            message.double_ack().await?;
            Ok(())
        })
        .await;
        if !matches!(result, Ok(Ok(()))) {
            // No ACK claims an unconfirmed effect. The same durable delivery
            // can return after its ACK deadline, including a conflicting body.
            emit(serde_json::json!({"status":"unconfirmed","channel":"outbox"}));
        }
    }
}

async fn request_body(socket: &mut TcpStream) -> Result<Vec<u8>, ReceiverError> {
    // The fixture sender uses a bounded Content-Length request. Reject other
    // framing instead of building a second general HTTP protocol stack.
    let mut request = Vec::with_capacity(2048);
    let header_end = loop {
        if let Some(index) = request.windows(4).position(|value| value == b"\r\n\r\n") {
            break index + 4;
        }
        if request.len() >= 16 * 1024 {
            return Err("receiver headers exceed bound".into());
        }
        let mut chunk = [0_u8; 1024];
        let read = socket.read(&mut chunk).await?;
        if read == 0 {
            return Err("receiver request ended before headers".into());
        }
        request.extend_from_slice(&chunk[..read]);
    };
    let headers = std::str::from_utf8(&request[..header_end])?;
    if headers.lines().next() != Some("POST /reading HTTP/1.1") {
        return Err("receiver requires POST /reading".into());
    }
    let mut length = None;
    for line in headers.lines().skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err("receiver requires Content-Length framing".into());
        }
        if name.eq_ignore_ascii_case("content-length") {
            if length.is_some() {
                return Err("receiver refuses duplicate Content-Length".into());
            }
            length = Some(value.trim().parse::<usize>()?);
        }
    }
    let length = length
        .filter(|length| *length <= 1024)
        .ok_or("receiver body bound")?;
    let mut body = request.split_off(header_end);
    if body.len() > length {
        return Err("receiver body exceeds Content-Length".into());
    }
    let already_read = body.len();
    body.resize(length, 0);
    socket.read_exact(&mut body[already_read..]).await?;
    Ok(body)
}

async fn receive_http(
    pool: &PgPool,
    listener: TcpListener,
    stop: &CancellationToken,
    hold: Option<&CompletionHold>,
) -> Result<(), ReceiverError> {
    loop {
        // Sequential admission bounds accepted sockets and database work to
        // one; NATS independently owns the other receiver pool connection.
        let (mut socket, _) = tokio::select! {
            biased;
            () = stop.cancelled() => return Ok(()),
            accepted = listener.accept() => accepted?,
        };
        let result = tokio::time::timeout(Duration::from_secs(12), async {
            let body = request_body(&mut socket).await?;
            let operation: Operation = serde_json::from_slice(&body)?;
            record(pool, Channel::Webhook, &operation, hold).await?;
            socket
                .write_all(
                    b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await?;
            socket.shutdown().await?;
            Ok::<(), ReceiverError>(())
        })
        .await;
        if !matches!(result, Ok(Ok(()))) {
            // Closing without success preserves the sender's uncertainty.
            emit(serde_json::json!({"status":"unconfirmed","channel":"webhook"}));
        }
    }
}

/// Receive against an independent pool until signal, failure, or finite expiry.
/// Both serial receive loops are joined before returning; no task is detached.
///
/// # Errors
/// Returns invalid bounds, unavailable broker/listener, or receiver-loop failure.
pub async fn run(pool: &PgPool, options: ReceiverOptions) -> Result<(), ReceiverError> {
    if !options.http_bind.ip().is_loopback()
        || options.run_for.is_zero()
        || options.run_for > Duration::from_secs(250)
    {
        return Err("receiver requires loopback and a 1..=250 second admission window".into());
    }
    let (client, consumer, listener) = tokio::time::timeout(Duration::from_secs(10), async {
        let client = async_nats::connect(&options.nats_url).await?;
        let jetstream = async_nats::jetstream::new(client.clone());
        let stream = jetstream.get_stream(&options.stream).await?;
        let consumer = stream
            .get_or_create_consumer(
                &options.consumer,
                async_nats::jetstream::consumer::pull::Config {
                    durable_name: Some(options.consumer.clone()),
                    filter_subject: options.subject.clone(),
                    ack_policy: async_nats::jetstream::consumer::AckPolicy::Explicit,
                    ack_wait: Duration::from_secs(15),
                    max_ack_pending: 1,
                    ..Default::default()
                },
            )
            .await?;
        let listener = TcpListener::bind(options.http_bind).await?;
        Ok::<_, ReceiverError>((client, consumer, listener))
    })
    .await??;
    let address = listener.local_addr()?;
    let stop = CancellationToken::new();
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    emit(serde_json::json!({"status":"ready","http_addr":address.to_string()}));
    let nats = async {
        let result = receive_nats(pool, consumer, &stop, options.hold.as_ref()).await;
        stop.cancel();
        result
    };
    let http = async {
        let result = receive_http(pool, listener, &stop, options.hold.as_ref()).await;
        stop.cancel();
        result
    };
    let shutdown = async {
        tokio::select! {
            () = stop.cancelled() => {},
            () = tokio::time::sleep(options.run_for) => {},
            _ = terminate.recv() => {},
            _ = interrupt.recv() => {},
        }
        stop.cancel();
    };
    let (nats, http, ()) = tokio::join!(nats, http, shutdown);
    tokio::time::timeout(Duration::from_secs(5), client.drain()).await??;
    nats?;
    http?;
    emit(serde_json::json!({"status":"stopped"}));
    Ok(())
}
