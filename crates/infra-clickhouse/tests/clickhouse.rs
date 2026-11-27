#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests assert ClickHouse behavior and own their fixture setup"
)]

use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::time::Duration;

use infra_clickhouse::{
    ClickHouse, ClickHouseAuth, ClickHouseOptions, CloseOutcome, Decimal32, Decimal64, Decimal128,
    HISTOGRAMS, InsertError, InsertFailure, InsertOptions, Param, QueryError, QuerySettings, Row,
    TransportSecurity,
};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use operation_context::{Deadline, OperationContext, Stopped};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::instrument::WithSubscriber as _;
use tracing_subscriber::layer::SubscriberExt as _;

fn env_or_panic(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} is unset; set ClickHouse integration env vars"))
}

const fn client_query_settings() -> QuerySettings {
    QuerySettings::Client {
        max_memory_usage: NonZeroU64::new(512 * 1024 * 1024),
    }
}

fn options_for(
    url: String,
    database: String,
    user: String,
    password: String,
    query_settings: QuerySettings,
) -> ClickHouseOptions {
    let transport = if url.starts_with("http://") {
        TransportSecurity::Plaintext
    } else {
        TransportSecurity::default()
    };
    ClickHouseOptions {
        url,
        database,
        auth: ClickHouseAuth::Password {
            username: user,
            password: SecretString::from(password),
        },
        transport,
        query_settings,
        query_timeout: Duration::from_secs(5),
        insert_timeout: Duration::from_secs(5),
        max_read_concurrency: const { NonZeroUsize::new(16).expect("nz") },
        max_write_concurrency: const { NonZeroUsize::new(16).expect("nz") },
        max_result_rows: const { NonZeroU64::new(100_000).expect("nz") },
        max_result_bytes: const { NonZeroUsize::new(16 * 1024 * 1024).expect("nz") },
        http_response_buffer_size: const { NonZeroUsize::new(2 * 1024 * 1024).expect("nz") },
        propagate_trace_context: true,
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Row)]
#[clickhouse(crate = "infra_clickhouse")]
struct EventRow {
    id: u64,
    kind: String,
    #[serde(with = "infra_clickhouse::serde::uuid")]
    trace_id: uuid::Uuid,
    #[serde(with = "infra_clickhouse::serde::time::date")]
    day: time::Date,
    #[serde(with = "infra_clickhouse::serde::time::datetime64::millis")]
    ts: time::OffsetDateTime,
}

#[derive(Debug, Deserialize, PartialEq, Eq, Row)]
#[clickhouse(crate = "infra_clickhouse")]
struct ParamProbeRow {
    flag: bool,
    num: i64,
    s: String,
    #[serde(with = "infra_clickhouse::serde::uuid")]
    uid: uuid::Uuid,
    #[serde(with = "infra_clickhouse::serde::time::date")]
    d: time::Date,
    #[serde(with = "infra_clickhouse::serde::time::datetime64::millis")]
    dt: time::OffsetDateTime,
    opt: Option<u32>,
    arr: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Row)]
#[clickhouse(crate = "infra_clickhouse")]
struct SimpleRow {
    id: u64,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Row)]
#[clickhouse(crate = "infra_clickhouse")]
struct EvolvedRow {
    id: u64,
    extra: u8,
}

async fn assert_param_select(rw: &ClickHouse, ctx: &OperationContext) {
    let uid = uuid::Uuid::new_v4();
    let day = time::Date::from_calendar_date(2026, time::Month::November, 22).expect("date");
    let ts =
        time::OffsetDateTime::from_unix_timestamp_nanos(1_763_820_000_123_000_000).expect("ts");

    let probe = rw
        .query(
            "param_smoke",
            "SELECT
                {flag:Bool} AS flag,
                {num:Int64} AS num,
                {s:String} AS s,
                {uid:UUID} AS uid,
                {d:Date} AS d,
                {dt:DateTime64(3, 'UTC')} AS dt,
                {opt:Nullable(UInt32)} AS opt,
                {arr:Array(String)} AS arr",
        )
        .param("flag", true)
        .param("num", -42_i64)
        .param("s", "hello'world")
        .param("uid", uid)
        .param("d", day)
        .param("dt", ts)
        .param("opt", Option::<u32>::None)
        .param("arr", vec!["one", "two"])
        .fetch_one::<ParamProbeRow>(ctx)
        .await
        .expect("parameterized SELECT");
    assert_eq!(
        probe,
        ParamProbeRow {
            flag: true,
            num: -42,
            s: "hello'world".to_owned(),
            uid,
            d: day,
            dt: ts,
            opt: None,
            arr: vec!["one".to_owned(), "two".to_owned()],
        }
    );
}

fn make_event(
    id: u64,
    kind: &str,
    trace_id: uuid::Uuid,
    day: time::Date,
    ts: time::OffsetDateTime,
) -> EventRow {
    EventRow {
        id,
        kind: kind.to_owned(),
        trace_id,
        day,
        ts,
    }
}

async fn assert_rw_batch_async_dedup_and_empty(
    rw: &ClickHouse,
    ctx: &OperationContext,
    table: &str,
) {
    let empty_report = rw
        .insert(
            ctx,
            "empty_batch",
            table,
            &[] as &[EventRow],
            InsertOptions::batch(),
        )
        .await
        .expect("0-row insert");
    assert_eq!(empty_report.rows, 0);

    let uid = uuid::Uuid::new_v4();
    let day = time::Date::from_calendar_date(2026, time::Month::November, 22).expect("date");
    let ts =
        time::OffsetDateTime::from_unix_timestamp_nanos(1_763_820_000_123_000_000).expect("ts");

    let batch_rows = [make_event(1, "batch", uid, day, ts)];
    let report = rw
        .insert(
            ctx,
            "smoke_batch",
            table,
            &batch_rows,
            InsertOptions::batch(),
        )
        .await
        .expect("insert batch");
    assert_eq!(report.rows, 1);

    let async_rows = [make_event(2, "async", uid, day, ts)];
    let async_opts = InsertOptions::async_with_token("smoke-token-1");
    assert_eq!(
        rw.insert(ctx, "smoke_async", table, &async_rows, async_opts)
            .await
            .expect("insert async 1")
            .rows,
        1
    );
    assert_eq!(
        rw.insert(ctx, "smoke_async_dup", table, &async_rows, async_opts)
            .await
            .expect("insert async 2")
            .rows,
        1
    );

    let hash_rows = [make_event(3, "async_hash", uid, day, ts)];
    let hash_opts = InsertOptions::async_with_payload_hash();
    rw.insert(ctx, "smoke_hash_1", table, &hash_rows, hash_opts)
        .await
        .expect("insert async hash 1");
    rw.insert(ctx, "smoke_hash_2", table, &hash_rows, hash_opts)
        .await
        .expect("insert async hash 2");

    let disabled_rows = [make_event(4, "async_disabled", uid, day, ts)];
    for summary in ["smoke_disabled_1", "smoke_disabled_2"] {
        rw.insert(
            ctx,
            summary,
            table,
            &disabled_rows,
            InsertOptions::async_without_deduplication(),
        )
        .await
        .expect(summary);
    }

    let fetched = rw
        .query(
            "read_back",
            format!("SELECT id, kind, trace_id, day, ts FROM {table} ORDER BY id"),
        )
        .fetch_all::<EventRow>(ctx)
        .await
        .expect("read back rows");
    assert_eq!(
        fetched,
        vec![
            batch_rows[0].clone(),
            async_rows[0].clone(),
            hash_rows[0].clone(),
            disabled_rows[0].clone(),
            disabled_rows[0].clone(),
        ]
    );
}

async fn count_running_marker(rw: &ClickHouse, ctx: &OperationContext, marker: &str) -> u64 {
    rw.query(
        "check_processes",
        "SELECT count() FROM system.processes WHERE position(query, {marker:String}) > 0 AND position(query, 'system.processes') = 0",
    )
    .param("marker", marker)
    .fetch_one(ctx)
    .await
    .expect("query system.processes")
}

async fn assert_server_side_http_post_cancellation(rw: &ClickHouse, ctx: &OperationContext) {
    let marker = format!("cancel_marker_{}", uuid::Uuid::new_v4().simple());
    let token = CancellationToken::new();
    let cancel_ctx =
        OperationContext::new(Some(Deadline::after(Duration::from_secs(5))), token.clone());
    let rw_bg = rw.clone();
    let sql =
        format!("SELECT sum(sipHash64(number)) AS id FROM numbers(5000000000) /* {marker} */");
    let query_task = tokio::spawn(async move {
        rw_bg
            .query("slow_cancel", &sql)
            .fetch_one::<SimpleRow>(&cancel_ctx)
            .await
    });

    let mut observed_running = false;
    for _ in 0..80 {
        if count_running_marker(rw, ctx, &marker).await >= 1 {
            observed_running = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        observed_running,
        "slow query never became visible in system.processes before cancellation"
    );

    let cancelled_at = std::time::Instant::now();
    token.cancel();
    assert_eq!(
        query_task.await.expect("join slow query"),
        Err(QueryError::Stopped(Stopped::Cancelled))
    );

    let mut cancelled_on_server = false;
    for _ in 0..80 {
        if count_running_marker(rw, ctx, &marker).await == 0 {
            cancelled_on_server = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        cancelled_on_server && cancelled_at.elapsed() < Duration::from_secs(2),
        "cancelled HTTP POST query remained active in system.processes (elapsed={:?})",
        cancelled_at.elapsed()
    );
}

async fn assert_read_bounds_cardinality_and_cancellation(
    rw: &ClickHouse,
    ctx: &OperationContext,
    table: &str,
) {
    assert_eq!(
        rw.query("one_0", format!("SELECT id FROM {table} WHERE id = 99999"))
            .fetch_one::<SimpleRow>(ctx)
            .await,
        Err(QueryError::NotFound)
    );
    assert_eq!(
        rw.query("opt_0", format!("SELECT id FROM {table} WHERE id = 99999"))
            .fetch_optional::<SimpleRow>(ctx)
            .await,
        Ok(None)
    );
    assert_eq!(
        rw.query("one_2", format!("SELECT id FROM {table}"))
            .fetch_one::<SimpleRow>(ctx)
            .await,
        Err(QueryError::TooManyRows)
    );
    assert_eq!(
        rw.query("opt_2", format!("SELECT id FROM {table}"))
            .fetch_optional::<SimpleRow>(ctx)
            .await,
        Err(QueryError::TooManyRows)
    );

    let exact_two = rw
        .query("exact_two", "SELECT number AS id FROM numbers(2)")
        .max_result_rows(const { NonZeroU64::new(2).expect("nz") })
        .fetch_all::<SimpleRow>(ctx)
        .await
        .expect("== 2 rows succeeds");
    assert_eq!(exact_two.len(), 2);

    let overflow_three = rw
        .query("over_three", "SELECT number AS id FROM numbers(3)")
        .max_result_rows(const { NonZeroU64::new(2).expect("nz") })
        .fetch_all::<SimpleRow>(ctx)
        .await
        .expect_err("3 rows with cap 2 must return TooLarge");
    assert_eq!(overflow_three, QueryError::TooLarge);

    let overflow_bytes = rw
        .query(
            "over_bytes",
            "SELECT repeat('x', 1024) AS k FROM numbers(200)",
        )
        .max_result_bytes(const { NonZeroUsize::new(64 * 1024).expect("nz") })
        .fetch_all::<String>(ctx)
        .await
        .expect_err("byte overflow must return TooLarge");
    assert_eq!(overflow_bytes, QueryError::TooLarge);

    let unknown_col = rw
        .query("bad_col", format!("SELECT nonexistent FROM {table}"))
        .fetch_all::<SimpleRow>(ctx)
        .await
        .expect_err("unknown column rejected");
    assert_eq!(unknown_col, QueryError::Rejected);

    let missing_param = rw
        .query("missing_param", "SELECT {missing:UInt64} AS id")
        .fetch_one::<SimpleRow>(ctx)
        .await
        .expect_err("missing query parameter rejected");
    assert_eq!(missing_param, QueryError::Rejected);
    assert!(!missing_param.is_retryable());

    let unknown_db = rw
        .query(
            "missing_db",
            "SELECT id FROM no_such_db_clickhouse_test.events",
        )
        .fetch_one::<SimpleRow>(ctx)
        .await
        .expect_err("unknown database rejected");
    assert_eq!(unknown_db, QueryError::Rejected);
    assert!(!unknown_db.is_retryable());

    assert_eq!(
        rw.query("one_server_cap", "SELECT number AS id FROM numbers(10000)")
            .fetch_one::<SimpleRow>(ctx)
            .await,
        Err(QueryError::TooManyRows)
    );
    assert_eq!(
        rw.query(
            "one_big_byte_cap",
            "SELECT repeat('x', 100000) AS k FROM numbers(1)"
        )
        .max_result_bytes(const { NonZeroUsize::new(64 * 1024).expect("nz") })
        .fetch_one::<String>(ctx)
        .await,
        Err(QueryError::TooLarge)
    );
    assert_eq!(
        rw.query(
            "one_scan_limit_158",
            "SELECT count() FROM numbers(1000) SETTINGS max_rows_to_read = 10"
        )
        .fetch_one::<u64>(ctx)
        .await,
        Err(QueryError::TooLarge)
    );

    assert_server_side_http_post_cancellation(rw, ctx).await;
}

async fn assert_readonly_behavior(
    url: String,
    db: String,
    ro_user: String,
    ro_password: String,
    table: &str,
    ctx: &OperationContext,
) {
    let ro_server = ClickHouse::start(options_for(
        url.clone(),
        db.clone(),
        ro_user.clone(),
        ro_password.clone(),
        QuerySettings::Server,
    ))
    .await
    .expect("start ro server");
    let ro_row = ro_server
        .query(
            "ro_read",
            format!("SELECT id, kind, trace_id, day, ts FROM {table} WHERE id = {{id:UInt64}}"),
        )
        .param("id", 1_u64)
        .fetch_optional::<EventRow>(ctx)
        .await
        .expect("ro query with server settings");
    assert_eq!(ro_row.as_ref().map(|r| r.id), Some(1));

    let dummy = [EventRow {
        id: 99,
        kind: "ro".to_owned(),
        trace_id: uuid::Uuid::nil(),
        day: time::Date::from_calendar_date(2026, time::Month::November, 22).expect("date"),
        ts: time::OffsetDateTime::UNIX_EPOCH,
    }];
    let ro_insert_err = ro_server
        .insert(ctx, "ro_write", table, &dummy, InsertOptions::batch())
        .await
        .expect_err("readonly insert must fail");
    assert_eq!(
        ro_insert_err,
        InsertError::NotApplied(InsertFailure::Rejected)
    );

    let ro_client_settings = ClickHouse::start(options_for(
        url.clone(),
        db.clone(),
        ro_user,
        ro_password,
        client_query_settings(),
    ))
    .await
    .expect("start ro client");
    let ro_client_err = ro_client_settings
        .query(
            "ro_rejected",
            format!("SELECT id, kind, trace_id, day, ts FROM {table}"),
        )
        .fetch_all::<EventRow>(ctx)
        .await
        .expect_err("readonly=1 with client settings must fail");
    assert_eq!(ro_client_err, QueryError::Rejected);

    let ro2_client = ClickHouse::start(options_for(
        url,
        db,
        "app_ro2".to_owned(),
        "app_ro2".to_owned(),
        client_query_settings(),
    ))
    .await
    .expect("start ro2 client");
    let mutable_ro_row = ro2_client
        .query(
            "ro2_read",
            format!("SELECT id, kind, trace_id, day, ts FROM {table} WHERE id = {{id:UInt64}}"),
        )
        .param("id", 1_u64)
        .fetch_optional::<EventRow>(ctx)
        .await
        .expect("readonly=2 supports client settings");
    assert_eq!(mutable_ro_row.as_ref().map(|r| r.id), Some(1));
    assert_eq!(
        ro2_client
            .insert(ctx, "ro2_write", table, &dummy, InsertOptions::batch())
            .await,
        Err(InsertError::NotApplied(InsertFailure::Rejected))
    );

    assert_eq!(
        ro_server.close(Duration::from_secs(2)).await,
        CloseOutcome::Complete
    );
    assert_eq!(
        ro_client_settings.close(Duration::from_secs(2)).await,
        CloseOutcome::Complete
    );
    assert_eq!(
        ro2_client.close(Duration::from_secs(2)).await,
        CloseOutcome::Complete
    );
}

struct LiveCredentials<'a> {
    url: &'a str,
    db: &'a str,
    user: &'a str,
    password: &'a str,
}

async fn assert_cold_nullable_column_omission(
    creds: &LiveCredentials<'_>,
    table_b: &str,
    ddl: &clickhouse::Client,
    ctx: &OperationContext,
) {
    ddl.query(&format!(
        "ALTER TABLE {table_b} ADD COLUMN opt Nullable(String)"
    ))
    .execute()
    .await
    .expect("add bare nullable column");
    let cold_old = ClickHouse::start(options_for(
        creds.url.to_owned(),
        creds.db.to_owned(),
        creds.user.to_owned(),
        creds.password.to_owned(),
        client_query_settings(),
    ))
    .await
    .expect("start cold old writer");
    assert_eq!(
        cold_old
            .insert(
                ctx,
                "cold_nullable_old",
                table_b,
                &[SimpleRow { id: 40 }],
                InsertOptions::batch(),
            )
            .await
            .expect("cold writer omitting bare Nullable column succeeds")
            .rows,
        1
    );
    let opt_val: Option<String> = cold_old
        .query(
            "read_nullable_opt",
            format!("SELECT opt FROM {table_b} WHERE id = 40"),
        )
        .fetch_one(ctx)
        .await
        .expect("read nullable opt");
    assert_eq!(opt_val, None);
    assert_eq!(
        cold_old.close(Duration::from_secs(2)).await,
        CloseOutcome::Complete
    );
}

async fn assert_rolling_schema_evolution(
    creds: &LiveCredentials<'_>,
    table_a: &str,
    table_b: &str,
    ddl: &clickhouse::Client,
    ctx: &OperationContext,
) {
    let opts = options_for(
        creds.url.to_owned(),
        creds.db.to_owned(),
        creds.user.to_owned(),
        creds.password.to_owned(),
        client_query_settings(),
    );
    let ch = Arc::new(ClickHouse::start(opts).await.expect("start ch"));

    for (summary, tbl) in [("prime_a", table_a), ("prime_b", table_b)] {
        ch.insert(
            ctx,
            summary,
            tbl,
            &[SimpleRow { id: 1 }],
            InsertOptions::batch(),
        )
        .await
        .expect(summary);
    }

    ddl.query(&format!(
        "ALTER TABLE {table_a} ADD COLUMN extra UInt8 DEFAULT 0"
    ))
    .execute()
    .await
    .expect("alter table A");

    let evolved_first = ch
        .insert(
            ctx,
            "evolved_first",
            table_a,
            &[EvolvedRow { id: 2, extra: 7 }],
            InsertOptions::batch(),
        )
        .await
        .expect("stale warm slot transparently re-primes once before dispatch");
    assert_eq!(evolved_first.rows, 1);

    let mut handles = Vec::new();
    for i in 10..15_u64 {
        let (ch_c, t_a, ctx_c) = (Arc::clone(&ch), table_a.to_owned(), ctx.clone());
        handles.push(tokio::spawn(async move {
            ch_c.insert(
                &ctx_c,
                "evolved_concurrent",
                &t_a,
                &[EvolvedRow { id: i, extra: 3 }],
                InsertOptions::batch(),
            )
            .await
        }));
    }
    let (ch_old, t_a_old, ctx_old) = (Arc::clone(&ch), table_a.to_owned(), ctx.clone());
    handles.push(tokio::spawn(async move {
        ch_old
            .insert(
                &ctx_old,
                "old_after_expand",
                &t_a_old,
                &[SimpleRow { id: 20 }],
                InsertOptions::batch(),
            )
            .await
    }));
    let (ch_b, t_b, ctx_b) = (Arc::clone(&ch), table_b.to_owned(), ctx.clone());
    handles.push(tokio::spawn(async move {
        ch_b.insert(
            &ctx_b,
            "table_b_ok",
            &t_b,
            &[SimpleRow { id: 30 }],
            InsertOptions::batch(),
        )
        .await
    }));

    for h in handles {
        h.await.expect("join").expect("concurrent evolved write ok");
    }

    let read_a = ch
        .query(
            "read_evolved_a",
            format!("SELECT id, extra FROM {table_a} ORDER BY id"),
        )
        .fetch_all::<EvolvedRow>(ctx)
        .await
        .expect("read A");
    assert_eq!(read_a.len(), 8);
    assert_eq!(read_a[0], EvolvedRow { id: 1, extra: 0 });
    assert_eq!(read_a[1], EvolvedRow { id: 2, extra: 7 });
    assert_eq!(read_a.last(), Some(&EvolvedRow { id: 20, extra: 0 }));

    assert_eq!(
        ch.close(Duration::from_secs(2)).await,
        CloseOutcome::Complete
    );
}

struct SpanEnterNotify(Arc<Notify>);

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for SpanEnterNotify {
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        _id: &tracing::span::Id,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if attrs.metadata().name() == "clickhouse" {
            self.0.notify_one();
        }
    }
}

async fn assert_live_busy_shedding(
    url: String,
    db: String,
    user: String,
    password: String,
    table_b: &str,
    ctx: &OperationContext,
) {
    let mut busy_opts = options_for(url, db, user, password, client_query_settings());
    busy_opts.max_read_concurrency = NonZeroUsize::MIN;
    busy_opts.max_write_concurrency = NonZeroUsize::MIN;
    let ch_busy = Arc::new(ClickHouse::start(busy_opts).await.expect("start busy"));
    let entered = Arc::new(Notify::new());
    let registered = entered.notified();
    let mut registered = std::pin::pin!(registered);
    registered.as_mut().enable();
    let dispatch = tracing::Dispatch::new(
        tracing_subscriber::Registry::default().with(SpanEnterNotify(Arc::clone(&entered))),
    );
    let ch_holder = Arc::clone(&ch_busy);
    let ctx_holder = ctx.clone();
    let holder = tokio::spawn(
        async move {
            ch_holder
                .query("busy_sleep", "SELECT sleep(1) AS id")
                .fetch_one::<u8>(&ctx_holder)
                .await
        }
        .with_subscriber(dispatch),
    );
    tokio::time::timeout(Duration::from_secs(5), registered)
        .await
        .expect("holder entered clickhouse span");
    assert_eq!(
        ch_busy
            .query("shed_read", "SELECT 1 AS id")
            .fetch_one::<SimpleRow>(ctx)
            .await,
        Err(QueryError::Busy)
    );
    assert_eq!(
        ch_busy
            .insert(
                ctx,
                "isolated_write",
                table_b,
                &[SimpleRow { id: 99 }],
                InsertOptions::batch()
            )
            .await
            .expect("read saturation must not shed writes")
            .rows,
        1
    );
    let _ = holder.await;
    assert_eq!(
        ch_busy.close(Duration::from_secs(2)).await,
        CloseOutcome::Complete
    );
}

async fn assert_rolling_schema_evolution_and_busy_shedding(
    url: String,
    db: String,
    user: String,
    password: String,
    ddl: &clickhouse::Client,
    ctx: &OperationContext,
) {
    let table_a = format!("t3_evolve_a_{}", uuid::Uuid::new_v4().simple());
    let table_b = format!("t3_evolve_b_{}", uuid::Uuid::new_v4().simple());
    for tbl in [&table_a, &table_b] {
        ddl.query(&format!(
            "CREATE TABLE {tbl} (id UInt64) ENGINE = MergeTree ORDER BY id"
        ))
        .execute()
        .await
        .expect("create evolve table");
    }

    let creds = LiveCredentials {
        url: &url,
        db: &db,
        user: &user,
        password: &password,
    };
    assert_rolling_schema_evolution(&creds, &table_a, &table_b, ddl, ctx).await;
    assert_cold_nullable_column_omission(&creds, &table_b, ddl, ctx).await;
    assert_live_busy_shedding(url, db, user, password, &table_b, ctx).await;

    for tbl in [&table_a, &table_b] {
        ddl.query(&format!("DROP TABLE IF EXISTS {tbl}"))
            .execute()
            .await
            .expect("drop evolve table");
    }
}

async fn assert_dedup_override_under_disabled_profile(
    url: String,
    db: String,
    table: &str,
    ctx: &OperationContext,
) {
    let nodedup = ClickHouse::start(options_for(
        url,
        db,
        "app_nodedup".to_owned(),
        "app_nodedup".to_owned(),
        client_query_settings(),
    ))
    .await
    .expect("start app_nodedup");

    let profile_setting: String = nodedup
        .query(
            "check_nodedup_profile",
            "SELECT getSetting('deduplicate_insert')",
        )
        .fetch_one(ctx)
        .await
        .expect("read deduplicate_insert setting");
    assert_eq!(profile_setting, "disable");

    let uid = uuid::Uuid::new_v4();
    let day = time::Date::from_calendar_date(2026, time::Month::November, 22).expect("date");
    let ts =
        time::OffsetDateTime::from_unix_timestamp_nanos(1_763_820_000_123_000_000).expect("ts");

    let plain_batch_rows = [make_event(10, "batch_plain", uid, day, ts)];
    for summary in ["nodedup_plain_1", "nodedup_plain_2"] {
        nodedup
            .insert(
                ctx,
                summary,
                table,
                &plain_batch_rows,
                InsertOptions::batch(),
            )
            .await
            .expect(summary);
    }

    let batch_tok_rows = [make_event(11, "batch_tok", uid, day, ts)];
    let batch_tok_opts = InsertOptions::batch_with_token("nodedup-batch-tok-1");
    for summary in ["nodedup_batch_1", "nodedup_batch_2"] {
        nodedup
            .insert(ctx, summary, table, &batch_tok_rows, batch_tok_opts)
            .await
            .expect(summary);
    }

    let hash_rows = [make_event(12, "nodedup_hash", uid, day, ts)];
    let hash_opts = InsertOptions::async_with_payload_hash();
    for summary in ["nodedup_hash_1", "nodedup_hash_2"] {
        nodedup
            .insert(ctx, summary, table, &hash_rows, hash_opts)
            .await
            .expect(summary);
    }

    let async_tok_rows = [make_event(13, "nodedup_tok", uid, day, ts)];
    let async_tok_opts = InsertOptions::async_with_token("nodedup-async-tok-1");
    for summary in ["nodedup_async_tok_1", "nodedup_async_tok_2"] {
        nodedup
            .insert(ctx, summary, table, &async_tok_rows, async_tok_opts)
            .await
            .expect(summary);
    }

    let fetched = nodedup
        .query(
            "read_back_nodedup",
            format!("SELECT id, kind, trace_id, day, ts FROM {table} WHERE id >= 10 ORDER BY id"),
        )
        .fetch_all::<EventRow>(ctx)
        .await
        .expect("read back nodedup rows");
    assert_eq!(
        fetched,
        vec![
            plain_batch_rows[0].clone(),
            plain_batch_rows[0].clone(),
            batch_tok_rows[0].clone(),
            hash_rows[0].clone(),
            async_tok_rows[0].clone(),
        ]
    );
    assert_eq!(
        nodedup.close(Duration::from_secs(2)).await,
        CloseOutcome::Complete
    );
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Row)]
#[clickhouse(crate = "infra_clickhouse")]
struct LedgerRow {
    id: u64,
    d32: Decimal32<2>,
    d64: Decimal64<4>,
    d128: Decimal128<6>,
    opt_d64: Option<Decimal64<2>>,
    #[serde(with = "infra_clickhouse::serde::time::datetime64::millis")]
    ts: time::OffsetDateTime,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Row)]
#[clickhouse(crate = "infra_clickhouse")]
struct WrongScaleRead {
    d64: Decimal64<2>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Row)]
#[clickhouse(crate = "infra_clickhouse")]
struct WrongPrecisionRead {
    #[serde(with = "infra_clickhouse::serde::time::datetime64::micros")]
    ts: time::OffsetDateTime,
}

async fn assert_live_decimal_scale_change_rejection(
    rw: &ClickHouse,
    ddl: &clickhouse::Client,
    tbl: &str,
    ts: time::OffsetDateTime,
    ctx: &OperationContext,
) {
    ddl.query(&format!("ALTER TABLE {tbl} MODIFY COLUMN d64 Decimal64(6)"))
        .execute()
        .await
        .expect("modify decimal scale");
    let stale_row = |id| LedgerRow {
        id,
        d32: Decimal32::new(100),
        d64: Decimal64::new(100),
        d128: Decimal128::new(100),
        opt_d64: None,
        ts,
    };
    let modify_err = rw
        .insert(
            ctx,
            "modify_scale_reject",
            tbl,
            &[stale_row(2), stale_row(3)],
            InsertOptions::batch(),
        )
        .await
        .expect_err("scale change must fail validation rather than corrupting data 100x");
    assert_eq!(modify_err, InsertError::NotApplied(InsertFailure::Rejected));

    let reprimes_to_encode = rw
        .insert(
            ctx,
            "modify_scale_reprimed",
            tbl,
            &[stale_row(4)],
            InsertOptions::batch(),
        )
        .await
        .expect_err("re-primed slot must reject scale mismatch locally");
    assert_eq!(
        reprimes_to_encode,
        InsertError::NotApplied(InsertFailure::Encode)
    );
}

async fn assert_timezone_and_decimal_array_params_live(
    rw: &ClickHouse,
    ctx: &OperationContext,
    ts: time::OffsetDateTime,
) {
    for (label, dt_val, expected_ns) in [
        ("modern", ts, 1_763_820_000_123_000_000_i64),
        (
            "pre_2001",
            time::OffsetDateTime::from_unix_timestamp_nanos(946_684_799_500_000_000).expect("1999"),
            946_684_799_500_000_000,
        ),
        ("epoch", time::OffsetDateTime::UNIX_EPOCH, 0),
        (
            "pre_epoch",
            time::OffsetDateTime::from_unix_timestamp_nanos(-1_500_000_000).expect("-1.5s"),
            -1_500_000_000,
        ),
    ] {
        let tz_nanos: i64 = rw
            .query(
                "tz_param_moscow",
                "SELECT toUnixTimestamp64Nano({dt:DateTime64(3, 'Europe/Moscow')})",
            )
            .param("dt", dt_val)
            .fetch_one(ctx)
            .await
            .expect(label);
        assert_eq!(tz_nanos, expected_ns, "{label}");
    }

    let dt32_secs: u32 = rw
        .query("dt32_param", "SELECT toUnixTimestamp({dt:DateTime})")
        .param("dt", Param::datetime_secs(ts))
        .fetch_one(ctx)
        .await
        .expect("dt32 param with subsec truncated ok");
    assert_eq!(dt32_secs, 1_763_820_000);

    let dec_arr_sum: Decimal128<2> = rw
        .query(
            "decimal_array_param",
            "SELECT arraySum({vals:Array(Decimal64(2))})",
        )
        .param(
            "vals",
            vec![Decimal64::<2>::new(1234), Decimal64::<2>::new(-500)],
        )
        .fetch_one(ctx)
        .await
        .expect("Array(Decimal64(2)) param ok");
    assert_eq!(dec_arr_sum, Decimal128::new(734));
}

async fn assert_decimal_datetime64_and_timezone_params_live(
    rw: &ClickHouse,
    ddl: &clickhouse::Client,
    ctx: &OperationContext,
) {
    let tbl = format!("t3_typed_{}", uuid::Uuid::new_v4().simple());
    ddl.query(&format!(
        "CREATE TABLE {tbl} (
            id UInt64,
            d32 Decimal32(2),
            d64 Decimal64(4),
            d128 Decimal128(6),
            opt_d64 Nullable(Decimal64(2)),
            ts DateTime64(3, 'UTC')
        ) ENGINE = MergeTree ORDER BY id"
    ))
    .execute()
    .await
    .expect("create typed table");

    let ts =
        time::OffsetDateTime::from_unix_timestamp_nanos(1_763_820_000_123_000_000).expect("ts");
    let row = LedgerRow {
        id: 1,
        d32: Decimal32::new(-1234),
        d64: Decimal64::new(567_890),
        d128: Decimal128::new(-9_999_999),
        opt_d64: Some(Decimal64::new(-5)),
        ts,
    };
    rw.insert(ctx, "typed_ins", &tbl, &[row], InsertOptions::batch())
        .await
        .expect("typed insert ok");

    assert_timezone_and_decimal_array_params_live(rw, ctx, ts).await;

    let matched: u64 = rw
        .query(
            "decimal_param_filter",
            format!("SELECT id FROM {tbl} WHERE d64 = {{amount:Decimal64(4)}}"),
        )
        .param("amount", Decimal64::<4>::new(567_890))
        .fetch_one(ctx)
        .await
        .expect("decimal param filter ok");
    assert_eq!(matched, 1);

    assert_eq!(
        rw.query("wrong_scale", format!("SELECT d64 FROM {tbl}"))
            .fetch_one::<WrongScaleRead>(ctx)
            .await,
        Err(QueryError::Decode)
    );
    assert_eq!(
        rw.query("wrong_prec", format!("SELECT ts FROM {tbl}"))
            .fetch_one::<WrongPrecisionRead>(ctx)
            .await,
        Err(QueryError::Decode)
    );

    assert_live_decimal_scale_change_rejection(rw, ddl, &tbl, ts, ctx).await;

    ddl.query(&format!("DROP TABLE IF EXISTS {tbl}"))
        .execute()
        .await
        .expect("drop typed table");
}

#[tokio::test]
async fn live_clickhouse_26_8_full_hardened_runtime_suite() {
    let mut builder = PrometheusBuilder::new();
    for &(name, buckets) in HISTOGRAMS {
        builder = builder
            .set_buckets_for_metric(Matcher::Full(name.to_owned()), buckets)
            .expect("buckets");
    }
    let recorder = builder.build_recorder();
    let _recorder_guard = metrics::set_default_local_recorder(&recorder);

    let url = env_or_panic("CLICKHOUSE_URL");
    let db = std::env::var("CLICKHOUSE_DATABASE").unwrap_or_else(|_| "app".to_owned());
    let user = std::env::var("CLICKHOUSE_USER").unwrap_or_else(|_| "app".to_owned());
    let password = std::env::var("CLICKHOUSE_PASSWORD").unwrap_or_else(|_| "app".to_owned());
    let ro_user = std::env::var("CLICKHOUSE_READONLY_USER").unwrap_or_else(|_| "app_ro".to_owned());
    let ro_password =
        std::env::var("CLICKHOUSE_READONLY_PASSWORD").unwrap_or_else(|_| "app_ro".to_owned());

    let table = format!("t3_live_{}", uuid::Uuid::new_v4().simple());
    let ddl = clickhouse::Client::default()
        .with_url(&url)
        .with_database(&db)
        .with_user(&user)
        .with_password(&password);

    ddl.query(&format!(
        "CREATE TABLE {table} (
            id UInt64,
            kind String,
            trace_id UUID,
            day Date,
            ts DateTime64(3, 'UTC')
        ) ENGINE = MergeTree ORDER BY id SETTINGS non_replicated_deduplication_window = 100"
    ))
    .execute()
    .await
    .expect("create table");

    let rw = ClickHouse::start(options_for(
        url.clone(),
        db.clone(),
        user.clone(),
        password.clone(),
        client_query_settings(),
    ))
    .await
    .expect("start rw");

    let startup_scrape = recorder.handle().render();
    assert!(
        startup_scrape.contains("clickhouse_operations_in_flight 0"),
        "expected startup in_flight gauge 0 in {startup_scrape}"
    );

    let ctx = OperationContext::with_timeout(Duration::from_secs(5));
    assert_param_select(&rw, &ctx).await;
    assert_decimal_datetime64_and_timezone_params_live(&rw, &ddl, &ctx).await;
    assert_rw_batch_async_dedup_and_empty(&rw, &ctx, &table).await;
    assert_dedup_override_under_disabled_profile(url.clone(), db.clone(), &table, &ctx).await;
    assert_read_bounds_cardinality_and_cancellation(&rw, &ctx, &table).await;
    assert_readonly_behavior(url.clone(), db.clone(), ro_user, ro_password, &table, &ctx).await;

    assert_eq!(
        rw.close(Duration::from_secs(2)).await,
        CloseOutcome::Complete
    );

    assert_rolling_schema_evolution_and_busy_shedding(url, db, user, password, &ddl, &ctx).await;

    let final_scrape = recorder.handle().render();
    assert!(
        final_scrape.contains("clickhouse_operations_in_flight 0"),
        "in_flight leaked: {final_scrape}"
    );
    assert!(final_scrape.contains("clickhouse_insert_rows_total{mode=\"batch\"}"));
    assert!(final_scrape.contains("clickhouse_insert_rows_total{mode=\"async\"}"));
    assert!(final_scrape.contains("clickhouse_read_rows_total"));
    assert!(final_scrape.contains("clickhouse_read_bytes_total"));
    assert!(final_scrape.contains("clickhouse_admission_refused_total"));

    ddl.query(&format!("DROP TABLE IF EXISTS {table}"))
        .execute()
        .await
        .expect("drop table");
}
