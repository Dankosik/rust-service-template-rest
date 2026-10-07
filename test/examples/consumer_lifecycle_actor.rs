//! Source-only actor, applied byte-for-byte to both historical consumer trees.
//! No production source, migration or dependency is part of this overlay.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[cfg(not(feature = "integration"))]
#[allow(clippy::print_stderr, reason = "finite source-only CLI refusal")]
fn main() {
    eprintln!("consumer_lifecycle_actor requires --features integration");
    std::process::exit(2);
}

#[cfg(feature = "integration")]
fn main() -> std::process::ExitCode {
    fixture::main()
}

#[cfg(feature = "integration")]
mod fixture {
    use std::num::NonZeroU32;
    use std::process::ExitCode;
    use std::time::Duration;

    use async_nats::jetstream::{self, consumer, stream};
    use domain_events::{Event, EventPayload};
    use infra_jobs::{EnqueueOptions, Job, JobError, JobKind, Policy};
    use infra_messaging::{
        ConsumerOptions, HandlerError, Messaging, MessagingOptions, Registry, Route,
    };
    use infra_postgres::{Dsn, Isolation, PgPool, PoolOptions, Tx, in_tx};
    use serde::{Deserialize, Serialize};
    use tokio::time::{Instant, timeout};
    use tokio_util::sync::CancellationToken;

    const WAIT: Duration = Duration::from_secs(30);
    const SOURCE: &str = "LIFECYCLE_SOURCE";
    const DLQ: &str = "LIFECYCLE_DLQ";
    const SUBJECT: &str = "lifecycle.created";
    const DLQ_SUBJECT: &str = "lifecycle.dead";
    type Error = Box<dyn std::error::Error + Send + Sync>;

    #[derive(Clone, Serialize, Deserialize)]
    struct Work {
        logical_id: String,
        value: String,
        fail: bool,
    }

    impl JobKind for Work {
        const NAME: &'static str = "test.lifecycle";
    }

    #[derive(Clone, Serialize, Deserialize)]
    struct Created {
        value: String,
    }

    impl EventPayload for Created {
        const EVENT_TYPE: &'static str = "test.lifecycle.created";
        const SCHEMA_VERSION: u16 = 1;
    }

    pub(super) fn main() -> ExitCode {
        let args: Vec<_> = std::env::args_os().collect();
        if args.get(1).is_some_and(|arg| arg == "worker") {
            // The historical worker owns startup history admission, retention,
            // claims, outbox publication and bounded signal-driven shutdown.
            return jobs_worker::run(
                std::iter::once(args[0].clone()).chain(args.into_iter().skip(2)),
                |registration| {
                    registration.jobs.register(Policy::default(), perform_job);
                    Ok(())
                },
            );
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("fixture runtime");
        runtime.block_on(async {
            Box::pin(timeout(Duration::from_secs(180), command()))
                .await
                .expect("finite actor command");
        });
        runtime.shutdown_timeout(Duration::from_secs(1));
        ExitCode::SUCCESS
    }

    async fn pool() -> PgPool {
        infra_postgres::connect(
            &Dsn::admit(&std::env::var("DATABASE_URL").expect("synthetic database URL")).unwrap(),
            &PoolOptions {
                max_connections: NonZeroU32::new(3).unwrap(),
                application_name: "consumer-lifecycle-fixture",
                default_isolation: Isolation::ReadCommitted,
                session_budgets: infra_postgres::SessionBudgets::Startup,
            },
        )
        .await
        .expect("fixture pool")
    }

    async fn command() {
        let args: Vec<_> = std::env::args().collect();
        let action = args.get(1).expect("actor action").as_str();
        if action == "migrate" {
            let dsn = Dsn::admit(&std::env::var("DATABASE_URL").unwrap()).unwrap();
            migrate::run(
                &migrate::MIGRATOR,
                &migrate::RunOptions::defaults(
                    &dsn,
                    "consumer-lifecycle-migrate",
                    Duration::from_secs(120),
                ),
            )
            .await
            .expect("source's actual embedded migrations");
            return;
        }
        let pool = pool().await;
        match action {
            "setup" => setup(&pool).await,
            "enqueue" => enqueue(&pool, &args[2], &args[3]).await,
            "consume" => consume(&pool).await,
            "replay" => replay(&args[2]).await,
            "retain" => {
                let mut kinds = infra_jobs::Kinds::new();
                kinds.register(Policy::default(), perform_job);
                let engine = infra_jobs::Engine::new(
                    pool.clone(),
                    kinds.validate().unwrap(),
                    NonZeroU32::MIN,
                );
                engine.check_startup().await.unwrap();
                engine
                    .remove_expired()
                    .await
                    .expect("actual retention owner");
            }
            _ => panic!("unknown finite fixture command"),
        }
        assert_eq!(
            infra_postgres::close(&pool, WAIT).await,
            infra_postgres::Closed::Complete
        );
    }

    async fn setup(pool: &PgPool) {
        // Synthetic fixture tables deliberately stay outside embedded history.
        sqlx::raw_sql(
            "CREATE TABLE lifecycle_intents (
                logical_id text PRIMARY KEY, category text NOT NULL, meaning jsonb NOT NULL);
            CREATE TABLE lifecycle_effects (
                logical_id text PRIMARY KEY, meaning jsonb NOT NULL);
            CREATE TABLE lifecycle_deliveries (
                sequence bigserial PRIMARY KEY, logical_id text NOT NULL);",
        )
        .execute(pool)
        .await
        .expect("synthetic tables");
        let js = jetstream::new(
            async_nats::connect(std::env::var("NATS_URL").unwrap())
                .await
                .unwrap(),
        );
        for (name, subject) in [(SOURCE, SUBJECT), (DLQ, DLQ_SUBJECT)] {
            js.create_stream(stream::Config {
                name: name.to_owned(),
                subjects: vec![subject.to_owned()],
                storage: stream::StorageType::File,
                max_messages: 64,
                // DLQ copies add original-subject and transfer-identity headers.
                max_message_size: if name == DLQ { 16 * 1024 } else { 9 * 1024 },
                duplicate_window: Duration::from_millis(100),
                ..Default::default()
            })
            .await
            .expect("finite file-backed source/DLQ");
        }
        js.get_stream(SOURCE)
            .await
            .unwrap()
            .create_consumer(consumer::pull::Config {
                name: Some("lifecycle".into()),
                durable_name: Some("lifecycle".into()),
                filter_subject: SUBJECT.into(),
                ack_wait: Duration::from_secs(41),
                max_deliver: -1,
                max_ack_pending: 1,
                ..Default::default()
            })
            .await
            .expect("named durable");
    }

    fn routes() -> Registry {
        Registry::new([Route::new::<Created>(SUBJECT)]).unwrap()
    }

    fn event(id: &str) -> Event<Created> {
        Event {
            id: id.to_owned(),
            occurred_at: time::UtcDateTime::from_unix_timestamp(1_700_000_000).unwrap(),
            payload: Created {
                value: format!("meaning:{id}"),
            },
        }
    }

    async fn enqueue(pool: &PgPool, category: &str, id: &str) {
        let meaning = serde_json::json!({"value": format!("meaning:{id}")});
        in_tx(pool, async |tx| -> Result<(), Error> {
            sqlx::query(
                "INSERT INTO lifecycle_intents (logical_id, category, meaning) VALUES ($1,$2,$3)",
            )
            .bind(id)
            .bind(category)
            .bind(&meaning)
            .execute(&mut *tx)
            .await?;
            if category == "event" {
                let prepared = routes().prepare(&event(id), 1024).unwrap();
                assert_eq!(
                    prepared.enqueue(tx).await?,
                    infra_messaging::outbox::OutboxEnqueued::Created
                );
            } else {
                let work = Work {
                    logical_id: id.to_owned(),
                    value: format!("meaning:{id}"),
                    fail: category == "failed",
                };
                assert!(matches!(
                    infra_jobs::enqueue(
                        tx,
                        &work,
                        EnqueueOptions {
                            unique_key: Some(id),
                            ..Default::default()
                        }
                    )
                    .await?,
                    infra_jobs::Enqueued::Created(_)
                ));
            }
            Ok(())
        })
        .await
        .expect("intent and queue commit together");
    }

    async fn effect(tx: &mut Tx<'_>, id: &str, value: &str) -> Result<(), Error> {
        let meaning = serde_json::json!({"value": value});
        sqlx::query("INSERT INTO lifecycle_effects (logical_id, meaning) VALUES ($1,$2) ON CONFLICT DO NOTHING")
            .bind(id).bind(&meaning).execute(&mut *tx).await?;
        let actual: serde_json::Value =
            sqlx::query_scalar("SELECT meaning FROM lifecycle_effects WHERE logical_id = $1")
                .bind(id)
                .fetch_one(&mut *tx)
                .await?;
        if actual != meaning {
            return Err("logical identity has conflicting meaning".into());
        }
        sqlx::query("INSERT INTO lifecycle_deliveries (logical_id) VALUES ($1)")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        Ok(())
    }

    #[allow(
        clippy::needless_pass_by_value,
        reason = "the jobs Handler contract owns its Job"
    )]
    async fn perform_job(job: Job<Work>) -> Result<(), JobError> {
        if job.payload().fail {
            return Err(JobError::permanent("synthetic retained failure"));
        }
        in_tx(job.pool(), async |tx| -> Result<(), Error> {
            effect(tx, &job.payload().logical_id, &job.payload().value).await?;
            job.complete_in_tx(tx).await?;
            Ok(())
        })
        .await
        .map_err(|_| JobError::retryable("synthetic effect transaction failed"))
    }

    async fn messaging(consumer: bool) -> Messaging {
        Messaging::connect(
            MessagingOptions {
                connection_name: "consumer-lifecycle-fixture".into(),
                servers: vec![std::env::var("NATS_URL").unwrap()],
                credentials: None,
                credentials_file: None,
                root_ca_path: None,
                allow_plaintext: true,
                tls_first: false,
                source_stream: SOURCE.into(),
                dlq_stream: consumer.then(|| DLQ.into()),
                max_payload_bytes: 1024,
                consumer: consumer.then(|| ConsumerOptions {
                    durable_name: "lifecycle".into(),
                    filter_subject: SUBJECT.into(),
                    dlq_subject: DLQ_SUBJECT.into(),
                    concurrency: 1,
                }),
            },
            Instant::now() + WAIT,
            CancellationToken::new(),
        )
        .await
        .expect("fresh topology admission")
    }

    async fn replay(id: &str) {
        let messaging = messaging(false).await;
        let prepared = routes().prepare(&event(id), 1024).unwrap();
        messaging
            .producer()
            .publish(&prepared, Instant::now() + WAIT, &CancellationToken::new())
            .await
            .expect("republication ACK");
        close(messaging).await;
    }

    #[allow(
        clippy::print_stdout,
        reason = "parent observes this finite actor's readiness marker"
    )]
    async fn consume(pool: &PgPool) {
        // Install before reporting ready; parent joins this process after SIGTERM.
        let mut signal =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        let messaging = messaging(true).await;
        let mut registry = routes();
        let effects = pool.clone();
        registry
            .register::<Created, _, _>(move |event, _| {
                let pool = effects.clone();
                async move {
                    if event.id == "event-dead-letter" {
                        return Err(HandlerError::Permanent);
                    }
                    in_tx(&pool, async |tx| {
                        effect(tx, &event.id, &event.payload.value).await
                    })
                    .await
                    .map_err(|_| HandlerError::Retryable)
                }
            })
            .unwrap();
        let cancel = CancellationToken::new();
        let mut handle = messaging.consumer(registry).await.unwrap().start(&cancel);
        println!("consumer_lifecycle_ready");
        signal.recv().await.expect("parent stop signal");
        handle.drain();
        handle
            .finish(Instant::now() + WAIT)
            .await
            .expect("consumer joins");
        cancel.cancel();
        close(messaging).await;
    }

    async fn close(messaging: Messaging) {
        assert_eq!(
            messaging
                .close(Instant::now() + WAIT, &CancellationToken::new())
                .await,
            infra_messaging::CloseOutcome::Complete
        );
    }
}
