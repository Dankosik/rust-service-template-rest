#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "integration tests assert ClickHouse behavior and own their fixture setup"
)]

use std::time::Duration;

use infra_clickhouse::{
    AsyncDeduplication, AsyncInsertOptions, BatchInsertOptions, ClickHouse, ClickHouseOptions,
    CloseOutcome, InsertError, InsertFailure, QueryError, QuerySettings, Row,
};
use operation_context::OperationContext;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Row)]
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

async fn assert_rw_batch_and_async_inserts(rw: &ClickHouse, ctx: &OperationContext, table: &str) {
    let uid = uuid::Uuid::new_v4();
    let day = time::Date::from_calendar_date(2026, time::Month::November, 22).expect("date");
    let ts =
        time::OffsetDateTime::from_unix_timestamp_nanos(1_763_820_000_123_000_000).expect("ts");

    let batch_rows = [EventRow {
        id: 1,
        kind: "batch".to_owned(),
        trace_id: uid,
        day,
        ts,
    }];
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

    let async_rows = [EventRow {
        id: 2,
        kind: "async".to_owned(),
        trace_id: uid,
        day,
        ts,
    }];
    let async_opts =
        AsyncInsertOptions::new().deduplication(AsyncDeduplication::ByToken("smoke-token-1"));
    let async_report = rw
        .insert_async(ctx, "smoke_async", table, &async_rows, async_opts)
        .await
        .expect("insert_async");
    assert_eq!(async_report.rows, 1);

    let fetched = rw
        .query(
            "read_back",
            &format!("SELECT id, kind, trace_id, day, ts FROM {table} ORDER BY id"),
        )
        .fetch_all::<EventRow>(ctx)
        .await
        .expect("read back rows");
    assert_eq!(fetched.len(), 2);
    assert_eq!(fetched[0], batch_rows[0]);
    assert_eq!(fetched[1], async_rows[0]);
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

#[tokio::test]
async fn live_clickhouse_26_8_smoke_rw_async_and_readonly() {
    let url = env_or_panic("CLICKHOUSE_URL");
    let db = std::env::var("CLICKHOUSE_DATABASE").unwrap_or_else(|_| "app".to_owned());
    let user = std::env::var("CLICKHOUSE_USER").unwrap_or_else(|_| "app".to_owned());
    let password = std::env::var("CLICKHOUSE_PASSWORD").unwrap_or_else(|_| "app".to_owned());
    let ro_user = std::env::var("CLICKHOUSE_READONLY_USER").unwrap_or_else(|_| "app_ro".to_owned());
    let ro_password =
        std::env::var("CLICKHOUSE_READONLY_PASSWORD").unwrap_or_else(|_| "app_ro".to_owned());

    let table = format!("t1_smoke_{}", uuid::Uuid::new_v4().simple());
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
        ) ENGINE = MergeTree ORDER BY id"
    ))
    .execute()
    .await
    .expect("create table");

    let rw = ClickHouse::start(options_for(
        url.clone(),
        db.clone(),
        user,
        password,
        QuerySettings::Client,
    ))
    .await
    .expect("start rw");

    let ctx = OperationContext::with_timeout(Duration::from_secs(5));
    assert_param_select(&rw, &ctx).await;
    assert_rw_batch_and_async_inserts(&rw, &ctx, &table).await;
    assert_readonly_behavior(url, db, ro_user, ro_password, &table, &ctx).await;

    assert_eq!(
        rw.close(Duration::from_secs(2)).await,
        CloseOutcome::Complete
    );

    ddl.query(&format!("DROP TABLE IF EXISTS {table}"))
        .execute()
        .await
        .expect("drop table");
}
