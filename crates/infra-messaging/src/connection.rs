//! The native client connection: connect, reconnect pacing, connection
//! events, and close.

use std::time::Duration;

use async_nats::rustls::crypto::{GetRandomFailed, SecureRandom};
use async_nats::{ConnectErrorKind, ToServerAddrs as _};
use futures_util::FutureExt as _;
use secrecy::ExposeSecret as _;
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::budget::{BROKER_OPERATION_BUDGET, bounded_operation, within_operation_budget};
use crate::credentials::{CredentialsFile, CredentialsFileError};
use crate::error::MessagingError;
use crate::messaging::{CloseOutcome, Credentials, MessagingOptions};

/// async-nats 0.50 caps its reconnect backoff at 4 s, reached from the
/// thirteenth attempt (`connector.rs`).
const RECONNECT_BACKOFF_CAP: Duration = Duration::from_secs(4);
const RECONNECT_CAP_ATTEMPT: usize = 13;

/// The native client and the receiver that reports its final `Closed` event.
#[derive(Clone, Debug)]
pub(crate) struct NativeConnection {
    pub(crate) client: async_nats::Client,
    /// Final event submission is separate from native runner completion.
    closed_event: watch::Receiver<bool>,
}

impl NativeConnection {
    /// Opens the first connection.
    pub(crate) async fn open(
        options: &MessagingOptions,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> Result<Self, MessagingError> {
        let (closed_event_tx, closed_event) = watch::channel(false);
        let servers: Vec<_> = options
            .servers
            .to_server_addrs()
            .map_err(|_| MessagingError::Configuration("broker server address is invalid"))?
            .collect();
        let all_tls = servers.iter().all(async_nats::ServerAddr::tls_required);
        if options.tls_first && !all_tls {
            return Err(MessagingError::Configuration(
                "TLS-first requires every configured server to use TLS",
            ));
        }
        let any_tls = servers.iter().any(async_nats::ServerAddr::tls_required);
        let connect_deadline = within_operation_budget(deadline);
        let connect_budget = connect_deadline.saturating_duration_since(Instant::now());
        let random = async_nats::rustls::crypto::aws_lc_rs::default_provider().secure_random;
        let mut connect = connect_options(options.credentials.as_ref(), deadline, cancel)
            .await?
            .name(&options.connection_name)
            .connection_timeout(BROKER_OPERATION_BUDGET)
            .initial_connect_deadline(connect_deadline)
            .request_timeout(Some(BROKER_OPERATION_BUDGET))
            .reconnect_delay_callback(move |attempts| reconnect_delay(attempts, random))
            .require_tls(!options.allow_plaintext)
            .event_callback(move |event| {
                // Close acknowledgement lets the owner retire telemetry.
                // Submit this final event before publishing completion.
                report_connection_event(&event);
                if matches!(event, async_nats::Event::Closed) {
                    closed_event_tx.send_replace(true);
                }
                std::future::ready(())
            });
        if options.tls_first {
            connect = connect.tls_first();
        } else if any_tls {
            // Ordinary TLS sees INFO before the TLS handshake. Mixed
            // seeds cannot share a single discovery trust boundary.
            connect = connect.ignore_discovered_servers();
        }
        if let Some(root_ca) = options.root_ca_path.clone() {
            connect = connect.add_root_certificates(root_ca);
        }
        let client = bounded_operation(connect_deadline, cancel, async {
            connect
                .connect(servers)
                .await
                .map_err(|error| connect_failure(&error, connect_budget))
        })
        .await?;
        Ok(Self {
            client,
            closed_event,
        })
    }

    // Completion at entry takes precedence over cancellation and deadline.
    // When the wait is interrupted, the native client owns the requested termination.
    pub(crate) async fn close(
        &self,
        deadline: Instant,
        cancel: &CancellationToken,
    ) -> CloseOutcome {
        let deadline = within_operation_budget(deadline);
        let client = &self.client;
        let mut closed_event = self.closed_event.clone();
        let closed_now = || client.wait_closed().now_or_never() == Some(true);
        if closed_now() && *closed_event.borrow() {
            return CloseOutcome::Complete;
        }
        let drain = async {
            // The second check observes a close that `drain` itself completed.
            if !closed_now() && client.drain().await.is_err() && !closed_now() {
                return CloseOutcome::UnobservedClose;
            }
            // A submitted Closed event cannot stand in for native resource release.
            if !client.wait_closed().await {
                return CloseOutcome::UnobservedClose;
            }
            if closed_event.wait_for(|submitted| *submitted).await.is_ok() {
                CloseOutcome::Complete
            } else {
                CloseOutcome::UnobservedClose
            }
        };
        let outcome = if cancel.is_cancelled() || Instant::now() >= deadline {
            CloseOutcome::WaitInterrupted
        } else {
            tokio::select! {
                biased;
                () = cancel.cancelled() => CloseOutcome::WaitInterrupted,
                () = tokio::time::sleep_until(deadline) => CloseOutcome::WaitInterrupted,
                result = drain => result,
            }
        };
        if outcome != CloseOutcome::Complete {
            client.force_close();
            if outcome == CloseOutcome::UnobservedClose && Instant::now() < deadline {
                // Forced closure does not gain a fresh wait budget.
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => {}
                    _ = tokio::time::timeout_at(deadline, client.wait_closed()) => {}
                }
            }
        }
        outcome
    }
}

/// Client options that authenticate with the configured credential source.
async fn connect_options(
    credentials: Option<&Credentials>,
    deadline: Instant,
    cancel: &CancellationToken,
) -> Result<async_nats::ConnectOptions, MessagingError> {
    match credentials {
        None => Ok(async_nats::ConnectOptions::new()),
        Some(Credentials::Inline(credentials)) => async_nats::ConnectOptions::new()
            .credentials(credentials.expose_secret())
            // The client's parse error is free text about the credentials.
            .map_err(|_| credentials_failure(CredentialsFileError::Malformed)),
        Some(Credentials::File(path)) => {
            let file = bounded_operation(deadline, cancel, async {
                CredentialsFile::admit(path.clone())
                    .await
                    .map_err(credentials_failure)
            })
            .await?;
            Ok(async_nats::ConnectOptions::with_auth_callback(
                move |nonce| {
                    let file = file.clone();
                    async move {
                        file.answer(&nonce)
                            .await
                            .map_err(async_nats::AuthError::new)
                    }
                },
            ))
        }
    }
}

/// Preserve the SDK's immediate attempts and capped backoff while spreading
/// each later attempt independently. The SDK still owns all retry and sleep.
/// The spread only shortens a delay, so it never exceeds the SDK's own.
fn reconnect_delay(attempts: usize, random: &dyn SecureRandom) -> Duration {
    if attempts <= 1 {
        return Duration::ZERO;
    }
    // Cap before shifting so even usize::MAX cannot wrap into a short retry.
    let base = if attempts >= RECONNECT_CAP_ATTEMPT {
        RECONNECT_BACKOFF_CAP
    } else {
        Duration::from_millis(1_u64 << (attempts - 1))
    };
    let mut bytes = [0; 2];
    let sample = random.fill(&mut bytes).map(|()| u16::from_le_bytes(bytes));
    spread_reconnect_delay(base, sample)
}

fn spread_reconnect_delay(base: Duration, sample: Result<u16, GetRandomFailed>) -> Duration {
    // A failed random sample spreads nothing.
    let spread = sample.map_or(Duration::ZERO, |sample| {
        shared_rules::jitter::share_of_window(base / 10, sample)
    });
    base.saturating_sub(spread)
}

/// Classifies an unusable credentials source and logs why, without the path
/// or any of its content.
fn credentials_failure(error: CredentialsFileError) -> MessagingError {
    let failure = match error {
        CredentialsFileError::Unreadable(_) => {
            MessagingError::Configuration("credentials file is unreadable")
        }
        CredentialsFileError::Malformed => MessagingError::Authentication,
    };
    tracing::warn!(
        operation = "credentials",
        reason = %failure,
        error.type = error.error_type(),
        io.error_kind = error.io_error_kind(),
        "messaging_admission_failed"
    );
    failure
}

/// Classifies a failed first connection and logs which stage refused it.
///
/// `Connection` covers name resolution, TLS and the socket alike; the client's
/// kind is a closed vocabulary that tells them apart. Its source can quote a
/// server URL and stays out of the log.
fn connect_failure(error: &async_nats::ConnectError, budget: Duration) -> MessagingError {
    let (failure, error_type) = match error.kind() {
        ConnectErrorKind::Authentication => (MessagingError::Authentication, "authentication"),
        ConnectErrorKind::AuthorizationViolation => {
            (MessagingError::Authentication, "authorization_violation")
        }
        ConnectErrorKind::TimedOut => (MessagingError::TimedOut { budget }, "timeout"),
        ConnectErrorKind::ServerParse => (
            MessagingError::Configuration("server URL is invalid"),
            "server_parse",
        ),
        ConnectErrorKind::Dns => (MessagingError::Connection, "dns"),
        ConnectErrorKind::Tls => (MessagingError::Connection, "tls"),
        ConnectErrorKind::Io => (MessagingError::Connection, "io"),
        ConnectErrorKind::MaxReconnects => (MessagingError::Connection, "max_reconnects"),
    };
    tracing::warn!(
        operation = "connect",
        reason = %failure,
        error.type = error_type,
        "messaging_admission_failed"
    );
    failure
}

/// Counts every connection event and logs the ones an operator acts on.
///
/// The client reports a slow consumer once per dropped message, so that
/// event is only counted. Server and client error text is arbitrary and stays
/// out of the log; `error.type` names the client's closed variant.
fn report_connection_event(event: &async_nats::Event) {
    use async_nats::{ClientError, Event, ServerError};

    let (result, error_type) = match event {
        Event::Connected => ("connected", None),
        Event::Disconnected => ("disconnected", None),
        Event::Draining => ("draining", None),
        Event::Closed => ("closed", None),
        Event::LameDuckMode => ("lame_duck", None),
        Event::SlowConsumer(_) => ("slow_consumer", None),
        Event::ServerError(error) => (
            "server_error",
            Some(match error {
                ServerError::AuthorizationViolation => "authorization_violation",
                ServerError::SlowConsumer(_) => "slow_consumer",
                ServerError::Other(_) => "other",
            }),
        ),
        Event::ClientError(error) => (
            "client_error",
            Some(match error {
                ClientError::MaxReconnects => "max_reconnects",
                ClientError::ServerNotInPool => "server_not_in_pool",
                ClientError::Other(_) => "other",
            }),
        ),
    };
    metrics::counter!("messaging_connection_events_total", "result" => result).increment(1);
    match event {
        Event::SlowConsumer(_) => {}
        Event::Connected | Event::Draining | Event::Closed => {
            tracing::info!(result, "messaging_connection");
        }
        Event::Disconnected
        | Event::LameDuckMode
        | Event::ServerError(_)
        | Event::ClientError(_) => {
            tracing::warn!(result, error.type = error_type, "messaging_connection");
        }
    }
}

/// Describes the connection metrics to the installed recorder. Repeating it
/// is harmless.
pub(crate) fn describe_metrics() {
    metrics::describe_counter!(
        "messaging_connection_events_total",
        "Broker connection events by result"
    );
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_nats::{ClientError, ConnectError, Event, ServerError};

    use super::*;

    #[test]
    fn reconnect_keeps_immediate_attempts_and_saturates_before_large_counts() {
        let random = async_nats::rustls::crypto::aws_lc_rs::default_provider().secure_random;
        assert_eq!(reconnect_delay(0, random), Duration::ZERO);
        assert_eq!(reconnect_delay(1, random), Duration::ZERO);
        for (attempts, minimum_micros, maximum_micros) in [
            (2, 1_800, 2_000),
            (4, 7_200, 8_000),
            (12, 1_843_200, 2_048_000),
            (13, 3_600_000, 4_000_000),
            (50, 3_600_000, 4_000_000),
            (usize::MAX, 3_600_000, 4_000_000),
        ] {
            let delay = reconnect_delay(attempts, random);
            assert!(delay >= Duration::from_micros(minimum_micros));
            assert!(delay <= Duration::from_micros(maximum_micros));
        }
    }

    #[test]
    fn reconnect_spread_has_exact_endpoints_and_conservative_source_failure() {
        for (base_millis, minimum_nanos) in [(2, 1_800_000), (4_000, 3_600_000_000)] {
            let base = Duration::from_millis(base_millis);
            assert_eq!(spread_reconnect_delay(base, Ok(0)), base);
            assert_eq!(
                spread_reconnect_delay(base, Ok(u16::MAX)),
                Duration::from_nanos(minimum_nanos)
            );
            assert_eq!(spread_reconnect_delay(base, Err(GetRandomFailed)), base);
        }
    }

    /// One logged event's fields, the message under `message`.
    type Logged = Vec<(&'static str, String)>;

    #[derive(Clone, Default)]
    struct Events(Arc<Mutex<Vec<Logged>>>);

    struct Fields(Logged);

    impl tracing::field::Visit for Fields {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            self.0.push((field.name(), format!("{value:?}")));
        }

        fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
            self.0.push((field.name(), value.to_owned()));
        }
    }

    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Events {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let mut fields = Fields(Vec::new());
            event.record(&mut fields);
            self.0.lock().expect("event lock").push(fields.0);
        }
    }

    fn logged(test: impl FnOnce()) -> Vec<Logged> {
        use tracing_subscriber::layer::SubscriberExt as _;

        let events = Events::default();
        let subscriber = tracing_subscriber::registry().with(events.clone());
        tracing::subscriber::with_default(subscriber, || {
            // Keep callsite interest independent of a sibling test's
            // thread-local subscriber.
            let _interest = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::default());
            test();
        });
        events.0.lock().expect("event lock").clone()
    }

    fn field<'a>(event: &'a Logged, name: &str) -> Option<&'a str> {
        event
            .iter()
            .find(|(field, _)| *field == name)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn a_failed_first_connection_logs_which_stage_refused_it() {
        let stages = [
            (ConnectErrorKind::Dns, "dns"),
            (ConnectErrorKind::Tls, "tls"),
            (ConnectErrorKind::Io, "io"),
        ];
        let events = logged(|| {
            for (kind, _) in stages {
                let source = std::io::Error::other("nats://user:sentinel@broker.invalid");
                let failure = connect_failure(
                    &ConnectError::with_source(kind, source),
                    BROKER_OPERATION_BUDGET,
                );
                std::assert_matches!(failure, MessagingError::Connection);
            }
        });

        assert_eq!(events.len(), stages.len());
        for (event, (_, error_type)) in events.iter().zip(stages) {
            assert_eq!(field(event, "message"), Some("messaging_admission_failed"));
            assert_eq!(field(event, "operation"), Some("connect"));
            assert_eq!(field(event, "error.type"), Some(error_type));
            assert!(event.iter().all(|(_, value)| !value.contains("sentinel")));
        }
    }

    #[test]
    fn connection_events_are_counted_and_logged_without_broker_text() {
        let recorder = metrics_exporter_prometheus::PrometheusBuilder::new().build_recorder();
        let events = metrics::with_local_recorder(&recorder, || {
            logged(|| {
                for event in [
                    Event::Disconnected,
                    Event::SlowConsumer(7),
                    Event::SlowConsumer(7),
                    Event::ServerError(ServerError::Other("sentinel broker text".to_owned())),
                    Event::ClientError(ClientError::MaxReconnects),
                    Event::Connected,
                ] {
                    report_connection_event(&event);
                }
            })
        });

        let scrape = recorder.handle().render();
        for (result, count) in [
            ("disconnected", 1),
            ("slow_consumer", 2),
            ("server_error", 1),
            ("client_error", 1),
            ("connected", 1),
        ] {
            let line = format!("messaging_connection_events_total{{result=\"{result}\"}} {count}");
            assert!(scrape.contains(&line), "{line} is missing from:\n{scrape}");
        }
        // A slow consumer is reported once per dropped message and only counted.
        let logged: Vec<_> = events
            .iter()
            .map(|event| {
                assert_eq!(field(event, "message"), Some("messaging_connection"));
                assert!(event.iter().all(|(_, value)| !value.contains("sentinel")));
                (
                    field(event, "result").expect("result"),
                    field(event, "error.type"),
                )
            })
            .collect();
        assert_eq!(
            logged,
            [
                ("disconnected", None),
                ("server_error", Some("other")),
                ("client_error", Some("max_reconnects")),
                ("connected", None),
            ]
        );
    }
}
