#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests assert ClickHouse behavior and own their fixture setup"
)]

use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::Arc;
use std::time::Duration;

use infra_clickhouse::{
    AsyncDeduplication, AsyncInsertOptions, BatchInsertOptions, ClickHouse, ClickHouseOptions,
    CloseOutcome, HISTOGRAMS, InsertError, InsertFailure, QueryError, QuerySettings, Row,
};
use metrics_exporter_prometheus::{Matcher, PrometheusBuilder};
use operation_context::{OperationContext, Stopped};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::instrument::WithSubscriber as _;
use tracing_subscriber::layer::SubscriberExt as _;

fn env_or_panic(name: &str) -> String {
    std::env::var(name)
        .unwrap_or_else(|_| panic!("{name} is unset; set ClickHouse integration env vars"))
}

fn options_for(
    url: String,
    database: String,
    user: String,
    password: String,
    query_settings: QuerySettings,
) -> ClickHouseOptions {
    ClickHouseOptions {
        url,
        database,
        username: Some(user),
        password: Some(SecretString::from(password)),
        password_file: None,
        root_ca_path: None,
        client_certificate: None,
        allow_plaintext: true,
        allow_unauthenticated: false,
        query_settings,
        query_timeout: Duration::from_secs(5),
        insert_timeout: Duration::from_secs(5),
        max_concurrency: 16,
        max_result_rows: 100_000,
        max_result_bytes: 16 * 1024 * 1024,
        http_response_buffer_size: 2 * 1024 * 1024,
        max_memory_usage: Some(512 * 1024 * 1024),
        propagate_trace_context: true,
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Row)]
#[clickhouse(crate = "infra_clickhouse::__clickhouse")]
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
#[clickhouse(crate = "infra_clickhouse::__clickhouse")]
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
#[clickhouse(crate = "infra_clickhouse::__clickhouse")]
struct SimpleRow {
    id: u64,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Row)]
#[clickhouse(crate = "infra_clickhouse::__clickhouse")]
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
        .insert_batch(
            ctx,
            "empty_batch",
            table,
            &[] as &[EventRow],
            BatchInsertOptions::new(),
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
        .insert_batch(
            ctx,
            "smoke_batch",
            table,
            &batch_rows,
            BatchInsertOptions::new(),
        )
        .await
        .expect("insert_batch");
    assert_eq!(report.rows, 1);

    let async_rows = [make_event(2, "async", uid, day, ts)];
    let async_opts =
        AsyncInsertOptions::new().deduplication(AsyncDeduplication::ByToken("smoke-token-1"));
    assert_eq!(
        rw.insert_async(ctx, "smoke_async", table, &async_rows, async_opts)
            .await
            .expect("insert_async 1")
            .rows,
        1
    );
    assert_eq!(
        rw.insert_async(ctx, "smoke_async_dup", table, &async_rows, async_opts)
            .await
            .expect("insert_async 2")
            .rows,
        1
    );

    let hash_rows = [make_event(3, "async_hash", uid, day, ts)];
    let hash_opts = AsyncInsertOptions::new().deduplication(AsyncDeduplication::ByPayloadHash);
    rw.insert_async(ctx, "smoke_hash_1", table, &hash_rows, hash_opts)
        .await
        .expect("insert_async hash 1");
    rw.insert_async(ctx, "smoke_hash_2", table, &hash_rows, hash_opts)
        .await
        .expect("insert_async hash 2");

    let disabled_rows = [make_event(4, "async_disabled", uid, day, ts)];
    for summary in ["smoke_disabled_1", "smoke_disabled_2"] {
        rw.insert_async(
            ctx,
            summary,
            table,
            &disabled_rows,
            AsyncInsertOptions::new(),
        )
        .await
        .expect(summary);
    }

    let fetched = rw
        .query(
            "read_back",
            &format!("SELECT id, kind, trace_id, day, ts FROM {table} ORDER BY id"),
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

async fn assert_read_bounds_cardinality_and_cancellation(
    rw: &ClickHouse,
    ctx: &OperationContext,
    table: &str,
) {
    // Cardinality checks.
    assert_eq!(
        rw.query("one_0", &format!("SELECT id FROM {table} WHERE id = 99999"),)
            .fetch_one::<SimpleRow>(ctx)
            .await,
        Err(QueryError::NotFound)
    );
    assert_eq!(
        rw.query("opt_0", &format!("SELECT id FROM {table} WHERE id = 99999"),)
            .fetch_optional::<SimpleRow>(ctx)
            .await,
        Ok(None)
    );
    assert_eq!(
        rw.query("one_2", &format!("SELECT id FROM {table}"))
            .fetch_one::<SimpleRow>(ctx)
            .await,
        Err(QueryError::TooManyRows)
    );
    assert_eq!(
        rw.query("opt_2", &format!("SELECT id FROM {table}"))
            .fetch_optional::<SimpleRow>(ctx)
            .await,
        Err(QueryError::TooManyRows)
    );

    // Exact == vs +1 row limit.
    let exact_two = rw
        .query("exact_two", "SELECT number AS id FROM numbers(2)")
        .max_result_rows(NonZeroU64::new(2).expect("nz"))
        .fetch_all::<SimpleRow>(ctx)
        .await
        .expect("== 2 rows succeeds");
    assert_eq!(exact_two.len(), 2);

    let overflow_three = rw
        .query("over_three", "SELECT number AS id FROM numbers(3)")
        .max_result_rows(NonZeroU64::new(2).expect("nz"))
        .fetch_all::<SimpleRow>(ctx)
        .await
        .expect_err("3 rows with cap 2 must return TooLarge");
    assert_eq!(overflow_three, QueryError::TooLarge);

    // Byte limit overflow.
    let overflow_bytes = rw
        .query(
            "over_bytes",
            "SELECT repeat('x', 1024) AS k FROM numbers(200)",
        )
        .max_result_bytes(NonZeroUsize::new(64 * 1024).expect("nz"))
        .fetch_all::<String>(ctx)
        .await
        .expect_err("byte overflow must return TooLarge");
    assert_eq!(overflow_bytes, QueryError::TooLarge);

    let unknown_col = rw
        .query("bad_col", &format!("SELECT nonexistent FROM {table}"))
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

    // OperationContext cancellation mid-query finishes promptly without waiting 2s.
    let short_ctx = OperationContext::with_timeout(Duration::from_millis(150));
    let started = std::time::Instant::now();
    let cancel_err = rw
        .query("slow_cancel", "SELECT sleep(2) AS id")
        .fetch_one::<SimpleRow>(&short_ctx)
        .await
        .expect_err("deadline stop");
    assert_eq!(cancel_err, QueryError::Stopped(Stopped::Deadline));
    assert!(started.elapsed() < Duration::from_secs(1));
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
            &format!("SELECT id, kind, trace_id, day, ts FROM {table} WHERE id = {{id:UInt64}}"),
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
        .insert_batch(ctx, "ro_write", table, &dummy, BatchInsertOptions::new())
        .await
        .expect_err("readonly insert must fail");
    assert_eq!(
        ro_insert_err,
        InsertError::NotApplied(InsertFailure::Rejected)
    );

    let ro_client_settings = ClickHouse::start(options_for(
        url,
        db,
        ro_user,
        ro_password,
        QuerySettings::Client,
    ))
    .await
    .expect("start ro client");
    let ro_client_err = ro_client_settings
        .query(
            "ro_rejected",
            &format!("SELECT id, kind, trace_id, day, ts FROM {table}"),
        )
        .fetch_all::<EventRow>(ctx)
        .await
        .expect_err("readonly with client settings must fail");
    assert_eq!(ro_client_err, QueryError::Rejected);

    assert_eq!(
        ro_server.close(Duration::from_secs(2)).await,
        CloseOutcome::Complete
    );
    assert_eq!(
        ro_client_settings.close(Duration::from_secs(2)).await,
        CloseOutcome::Complete
    );
}

async fn assert_rolling_schema_evolution(
    opts: ClickHouseOptions,
    table_a: &str,
    table_b: &str,
    ddl: &clickhouse::Client,
    ctx: &OperationContext,
) {
    let ch = Arc::new(ClickHouse::start(opts).await.expect("start ch"));

    ch.insert_batch(
        ctx,
        "prime_a",
        table_a,
        &[SimpleRow { id: 1 }],
        BatchInsertOptions::new(),
    )
    .await
    .expect("prime A");
    ch.insert_batch(
        ctx,
        "prime_b",
        table_b,
        &[SimpleRow { id: 1 }],
        BatchInsertOptions::new(),
    )
    .await
    .expect("prime B");

    ddl.query(&format!(
        "ALTER TABLE {table_a} ADD COLUMN extra UInt8 DEFAULT 0"
    ))
    .execute()
    .await
    .expect("alter table A");

    let drift_err = ch
        .insert_batch(
            ctx,
            "evolved_first",
            table_a,
            &[EvolvedRow { id: 2, extra: 7 }],
            BatchInsertOptions::new(),
        )
        .await
        .expect_err("stale slot detects schema mismatch");
    assert_eq!(drift_err, InsertError::NotApplied(InsertFailure::Encode));

    let mut handles = Vec::new();
    for i in 10..15_u64 {
        let (ch_c, t_a, ctx_c) = (Arc::clone(&ch), table_a.to_owned(), ctx.clone());
        handles.push(tokio::spawn(async move {
            ch_c.insert_batch(
                &ctx_c,
                "evolved_concurrent",
                &t_a,
                &[EvolvedRow { id: i, extra: 3 }],
                BatchInsertOptions::new(),
            )
            .await
        }));
    }
    let (ch_old, t_a_old, ctx_old) = (Arc::clone(&ch), table_a.to_owned(), ctx.clone());
    handles.push(tokio::spawn(async move {
        ch_old
            .insert_batch(
                &ctx_old,
                "old_after_expand",
                &t_a_old,
                &[SimpleRow { id: 20 }],
                BatchInsertOptions::new(),
            )
            .await
    }));
    let (ch_b, t_b, ctx_b) = (Arc::clone(&ch), table_b.to_owned(), ctx.clone());
    handles.push(tokio::spawn(async move {
        ch_b.insert_batch(
            &ctx_b,
            "table_b_ok",
            &t_b,
            &[SimpleRow { id: 30 }],
            BatchInsertOptions::new(),
        )
        .await
    }));

    for h in handles {
        h.await.expect("join").expect("concurrent evolved write ok");
    }

    let read_a = ch
        .query(
            "read_evolved_a",
            &format!("SELECT id, extra FROM {table_a} ORDER BY id"),
        )
        .fetch_all::<EvolvedRow>(ctx)
        .await
        .expect("read A");
    assert_eq!(read_a.len(), 7);
    assert_eq!(read_a[0], EvolvedRow { id: 1, extra: 0 });
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
    let mut busy_opts = options_for(url, db, user, password, QuerySettings::Client);
    busy_opts.max_concurrency = 1;
    let ch_busy = Arc::new(ClickHouse::start(busy_opts).await.expect("start busy"));
    let entered = Arc::new(Notify::new());
    let registered = entered.notified();
    tokio::pin!(registered);
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
            .insert_batch(
                ctx,
                "shed_write",
                table_b,
                &[SimpleRow { id: 99 }],
                BatchInsertOptions::new()
            )
            .await,
        Err(InsertError::NotApplied(InsertFailure::Busy))
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

    assert_rolling_schema_evolution(
        options_for(
            url.clone(),
            db.clone(),
            user.clone(),
            password.clone(),
            QuerySettings::Client,
        ),
        &table_a,
        &table_b,
        ddl,
        ctx,
    )
    .await;
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
        QuerySettings::Client,
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
            .insert_batch(
                ctx,
                summary,
                table,
                &plain_batch_rows,
                BatchInsertOptions::new(),
            )
            .await
            .expect(summary);
    }

    let batch_tok_rows = [make_event(11, "batch_tok", uid, day, ts)];
    let batch_tok_opts = BatchInsertOptions::new().deduplication_token("nodedup-batch-tok-1");
    for summary in ["nodedup_batch_1", "nodedup_batch_2"] {
        nodedup
            .insert_batch(ctx, summary, table, &batch_tok_rows, batch_tok_opts)
            .await
            .expect(summary);
    }

    let hash_rows = [make_event(12, "nodedup_hash", uid, day, ts)];
    let hash_opts = AsyncInsertOptions::new().deduplication(AsyncDeduplication::ByPayloadHash);
    for summary in ["nodedup_hash_1", "nodedup_hash_2"] {
        nodedup
            .insert_async(ctx, summary, table, &hash_rows, hash_opts)
            .await
            .expect(summary);
    }

    let async_tok_rows = [make_event(13, "nodedup_tok", uid, day, ts)];
    let async_tok_opts =
        AsyncInsertOptions::new().deduplication(AsyncDeduplication::ByToken("nodedup-async-tok-1"));
    for summary in ["nodedup_async_tok_1", "nodedup_async_tok_2"] {
        nodedup
            .insert_async(ctx, summary, table, &async_tok_rows, async_tok_opts)
            .await
            .expect(summary);
    }

    let fetched = nodedup
        .query(
            "read_back_nodedup",
            &format!("SELECT id, kind, trace_id, day, ts FROM {table} WHERE id >= 10 ORDER BY id"),
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
        QuerySettings::Client,
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
