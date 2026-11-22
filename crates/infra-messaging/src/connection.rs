use std::time::Duration;

use async_nats::rustls::crypto::{GetRandomFailed, SecureRandom};
use async_nats::{ConnectErrorKind, ToServerAddrs as _};
use futures_util::FutureExt as _;
use operation_context::OperationContext;
use secrecy::ExposeSecret as _;
use tokio::sync::watch;

use crate::budget::{BROKER_OPERATION_BUDGET, BrokerBudget, bounded_operation, bounded_with};
use crate::credentials::{CredentialsFile, CredentialsFileError};
use crate::error::MessagingError;
use crate::messaging::{CloseOutcome, Credentials, MessagingOptions, TransportSecurity};

const RECONNECT_BACKOFF_CAP: Duration = Duration::from_secs(4);
const RECONNECT_CAP_ATTEMPT: usize = 13;
const CONNECTION_EVENTS_TOTAL: &str = "messaging_connection_events_total";

#[derive(Clone, Debug)]
pub(crate) struct NativeConnection {
    pub(crate) client: async_nats::Client,
    closed_event: watch::Receiver<bool>,
}

impl NativeConnection {
    pub(crate) async fn open(
        options: &MessagingOptions,
        context: &OperationContext,
    ) -> Result<Self, MessagingError> {
        let (closed_event_tx, closed_event) = watch::channel(false);
        let servers: Vec<_> = options
            .servers
            .to_server_addrs()
            .map_err(|_| MessagingError::Configuration("broker server address is invalid"))?
            .collect();
        let (require_tls, tls_first) = match options.security {
            TransportSecurity::Tls => (true, false),
            TransportSecurity::TlsFirst => (true, true),
            TransportSecurity::AllowPlaintext => (false, false),
        };
        let all_tls = servers.iter().all(async_nats::ServerAddr::tls_required);
        if tls_first && !all_tls {
            return Err(MessagingError::Configuration(
                "TLS-first requires every configured server to use TLS",
            ));
        }
        let any_tls = servers.iter().any(async_nats::ServerAddr::tls_required);
        let connect_budget = BrokerBudget::from_context(context);
        let connect_remaining = connect_budget.remaining();
        let random = async_nats::rustls::crypto::aws_lc_rs::default_provider().secure_random;
        let mut builder = connect_options(options.credentials.as_ref(), context)
            .await?
            .name(&options.connection_name)
            .connection_timeout(BROKER_OPERATION_BUDGET)
            .request_timeout(Some(BROKER_OPERATION_BUDGET))
            .reconnect_delay_callback(move |attempts| reconnect_delay(attempts, random))
            .require_tls(require_tls)
            .event_callback(move |event| {
                report_connection_event(&event);
                if matches!(event, async_nats::Event::Closed) {
                    closed_event_tx.send_replace(true);
                }
                std::future::ready(())
            });
        if let Some(instant) = connect_budget.deadline().instant() {
            builder = builder.initial_connect_deadline(instant);
        }
        if tls_first {
            builder = builder.tls_first();
        } else if any_tls {
            builder = builder.ignore_discovered_servers();
        }
        if let Some(root_ca) = options.root_ca_path.clone() {
            builder = builder.add_root_certificates(root_ca);
        }
        let client = bounded_with(connect_budget, async {
            builder
                .connect(servers)
                .await
                .map_err(|error| connect_failure(&error, connect_remaining))
        })
        .await?;
        Ok(Self {
            client,
            closed_event,
        })
    }

    pub(crate) async fn close(&self, context: &OperationContext) -> CloseOutcome {
        let budget = BrokerBudget::from_context(context);
        let client = &self.client;
        let mut closed_event = self.closed_event.clone();
        let closed_now = || client.wait_closed().now_or_never() == Some(true);
        if closed_now() && *closed_event.borrow() {
            return CloseOutcome::Complete;
        }
        let drain = async {
            if !closed_now() && client.drain().await.is_err() && !closed_now() {
                return CloseOutcome::UnobservedClose;
            }
            if !client.wait_closed().await {
                return CloseOutcome::UnobservedClose;
            }
            if closed_event.wait_for(|closed| *closed).await.is_ok() {
                CloseOutcome::Complete
            } else {
                CloseOutcome::UnobservedClose
            }
        };
        let outcome = if budget.stopped().is_some() {
            CloseOutcome::WaitInterrupted
        } else {
            tokio::select! {
                biased;
                _ = budget.wait_stopped() => CloseOutcome::WaitInterrupted,
                result = drain => result,
            }
        };
        match outcome {
            CloseOutcome::Complete => {}
            CloseOutcome::WaitInterrupted => client.force_close(),
            CloseOutcome::UnobservedClose => {
                client.force_close();
                if budget.stopped().is_none() {
                    tokio::select! {
                        biased;
                        _ = budget.wait_stopped() => {}
                        _ = client.wait_closed() => {}
                    }
                }
            }
        }
        outcome
    }
}

async fn connect_options(
    credentials: Option<&Credentials>,
    context: &OperationContext,
) -> Result<async_nats::ConnectOptions, MessagingError> {
    match credentials {
        None => Ok(async_nats::ConnectOptions::new()),
        Some(Credentials::Inline(credentials)) => async_nats::ConnectOptions::new()
            .credentials(credentials.expose_secret())
            .map_err(|_| credentials_failure(CredentialsFileError::Malformed)),
        Some(Credentials::File(path)) => {
            let file = bounded_operation(context, async {
                CredentialsFile::admit(path)
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

fn reconnect_delay(attempts: usize, random: &dyn SecureRandom) -> Duration {
    if attempts <= 1 {
        return Duration::ZERO;
    }
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
    let spread = sample.map_or(Duration::ZERO, |sample| {
        shared_rules::jitter::share_of_window(base / 10, sample)
    });
    base.saturating_sub(spread)
}

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

fn connect_failure(error: &async_nats::ConnectError, budget: Duration) -> MessagingError {
    let (failure, error_type) = match error.kind() {
        ConnectErrorKind::Authentication => (MessagingError::Authentication, "authentication"),
        ConnectErrorKind::AuthorizationViolation => {
            (MessagingError::Authentication, "authorization_violation")
        }
        ConnectErrorKind::TimedOut => (MessagingError::TimedOut { budget }, "timeout"),
        ConnectErrorKind::Configuration => (
            MessagingError::Configuration("broker TLS configuration is incompatible"),
            "configuration",
        ),
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

fn report_connection_event(event: &async_nats::Event) {
    use async_nats::{ClientError, Event, ServerError};

    enum Log {
        Silent,
        Info,
        Warn(Option<&'static str>),
    }

    let (result, log) = match event {
        Event::Connected => ("connected", Log::Info),
        Event::Disconnected => ("disconnected", Log::Warn(None)),
        Event::Draining => ("draining", Log::Info),
        Event::Closed => ("closed", Log::Info),
        Event::LameDuckMode => ("lame_duck", Log::Warn(None)),
        Event::SlowConsumer(_) => ("slow_consumer", Log::Silent),
        Event::ServerError(error) => (
            "server_error",
            Log::Warn(Some(match error {
                ServerError::AuthorizationViolation => "authorization_violation",
                ServerError::SlowConsumer(_) => "slow_consumer",
                ServerError::Other(_) => "other",
            })),
        ),
        Event::ClientError(error) => (
            "client_error",
            Log::Warn(Some(match error {
                ClientError::MaxReconnects => "max_reconnects",
                ClientError::ServerNotInPool => "server_not_in_pool",
                ClientError::Other(_) => "other",
            })),
        ),
    };
    metrics::counter!(CONNECTION_EVENTS_TOTAL, "result" => result).increment(1);
    match log {
        Log::Silent => {}
        Log::Info => tracing::info!(result, "messaging_connection"),
        Log::Warn(error_type) => {
            tracing::warn!(result, error.type = error_type, "messaging_connection");
        }
    }
}

pub(crate) fn describe_metrics() {
    metrics::describe_counter!(
        CONNECTION_EVENTS_TOTAL,
        "Broker connection events by result"
    );
}

#[cfg(test)]
mod tests {
    use async_nats::{ClientError, ConnectError, Event, ServerError};
    use observe_test_support::{CapturedEvent, EventCapture};

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

    fn logged(test: impl FnOnce()) -> Vec<CapturedEvent> {
        let events = EventCapture::new();
        {
            let _guard = events.install_default();
            test();
        }
        events.snapshot()
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
            assert_eq!(event.message(), Some("messaging_admission_failed"));
            assert_eq!(event.unquoted("operation"), Some("connect"));
            assert_eq!(event.unquoted("error.type"), Some(error_type));
            assert!(
                event
                    .fields
                    .values()
                    .all(|value| !value.contains("sentinel"))
            );
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
        let logged: Vec<_> = events
            .iter()
            .map(|event| {
                assert_eq!(event.message(), Some("messaging_connection"));
                assert!(
                    event
                        .fields
                        .values()
                        .all(|value| !value.contains("sentinel"))
                );
                (
                    event.unquoted("result").expect("result"),
                    event.unquoted("error.type"),
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
