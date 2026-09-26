//! Real-PostgreSQL proof of the forward jobs storage conversion against the
//! historical schema. The runner's migration-history behavior remains owned by
//! the migration suite; these cases isolate the representation contract.

use infra_postgres::PgPool;
use sqlx::Row;

const HISTORICAL: &str =
    include_str!("../../../migrations/20260924000001_create_background_jobs.sql");
const FORWARD: &str =
    include_str!("../../../migrations/20260925000001_simplify_background_jobs.sql");

async fn historical_schema(pool: &PgPool) {
    sqlx::raw_sql(HISTORICAL)
        .execute(pool)
        .await
        .expect("the historical jobs schema");
}

async fn insert_historical_row(pool: &PgPool, payload: &[u8], unique_key: &[u8]) -> String {
    sqlx::query_scalar(
        "INSERT INTO background_jobs \
         (kind, payload, unique_key, state, attempts, claim_generation, not_before, \
          claim_expires_at, trace_context) \
         VALUES ('test.migration', $1, $2, 'running', 7, 13, \
                 TIMESTAMPTZ '2030-01-02 03:04:05+00', \
                 TIMESTAMPTZ '2030-01-02 04:04:05+00', 'legacy-parent') \
         RETURNING id::text",
    )
    .bind(payload)
    .bind(unique_key)
    .fetch_one(pool)
    .await
    .expect("the historical row")
}

async fn assert_historical_shape(pool: &PgPool) {
    let types: (String, String) = sqlx::query_as(
        "SELECT pg_typeof(payload)::text, pg_typeof(unique_key)::text \
         FROM background_jobs LIMIT 1",
    )
    .fetch_one(pool)
    .await
    .expect("the historical column types");
    assert_eq!(types, ("bytea".to_owned(), "bytea".to_owned()));

    let trace_state_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name = 'background_jobs' \
         AND column_name = 'trace_state')",
    )
    .fetch_one(pool)
    .await
    .expect("the historical columns");
    assert!(
        !trace_state_exists,
        "the forward column was added after a refusal"
    );

    let old_running_index: String = sqlx::query_scalar(
        "SELECT string_agg(attribute.attname, ',' ORDER BY key.ordinality) \
         FROM pg_index AS index \
         JOIN LATERAL unnest(index.indkey) WITH ORDINALITY AS key(attnum, ordinality) ON true \
         JOIN pg_attribute AS attribute \
           ON attribute.attrelid = index.indrelid AND attribute.attnum = key.attnum \
         WHERE index.indexrelid = 'background_jobs_running'::regclass",
    )
    .fetch_one(pool)
    .await
    .expect("the historical running index");
    assert_eq!(old_running_index, "claim_expires_at");
}

#[sqlx::test(migrations = false)]
async fn e8_e9_forward_conversion_preserves_compatible_row_semantics_and_metadata(pool: PgPool) {
    historical_schema(&pool).await;
    let id = insert_historical_row(
        &pool,
        br#"{"duplicate": 1, "duplicate": 2, "nested": {"ok": true}}"#,
        "é".as_bytes(),
    )
    .await;

    sqlx::raw_sql(FORWARD)
        .execute(&pool)
        .await
        .expect("the compatible forward conversion");

    let row = sqlx::query(
        "SELECT payload::text AS payload, unique_key, state, attempts, claim_generation, \
                not_before = TIMESTAMPTZ '2030-01-02 03:04:05+00' AS not_before_kept, \
                claim_expires_at = TIMESTAMPTZ '2030-01-02 04:04:05+00' AS claim_expires_kept, \
                trace_context, trace_state IS NULL AS trace_state_is_null, \
                pg_typeof(payload)::text AS payload_type, \
                pg_typeof(unique_key)::text AS unique_key_type \
         FROM background_jobs WHERE id::text = $1",
    )
    .bind(&id)
    .fetch_one(&pool)
    .await
    .expect("the converted row");
    let payload: String = row.try_get("payload").expect("payload");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&payload).expect("JSONB payload"),
        serde_json::json!({"duplicate": 2, "nested": {"ok": true}})
    );
    assert_eq!(
        row.try_get::<String, _>("unique_key").expect("unique_key"),
        "é"
    );
    assert_eq!(row.try_get::<String, _>("state").expect("state"), "running");
    assert_eq!(row.try_get::<i16, _>("attempts").expect("attempts"), 7);
    assert_eq!(
        row.try_get::<i64, _>("claim_generation")
            .expect("claim_generation"),
        13
    );
    assert!(
        row.try_get::<bool, _>("not_before_kept")
            .expect("not_before_kept")
    );
    assert!(
        row.try_get::<bool, _>("claim_expires_kept")
            .expect("claim_expires_kept")
    );
    assert_eq!(
        row.try_get::<Option<String>, _>("trace_context")
            .expect("trace_context"),
        Some("legacy-parent".to_owned())
    );
    assert!(
        row.try_get::<bool, _>("trace_state_is_null")
            .expect("trace_state_is_null")
    );
    assert_eq!(
        row.try_get::<String, _>("payload_type")
            .expect("payload_type"),
        "jsonb"
    );
    assert_eq!(
        row.try_get::<String, _>("unique_key_type")
            .expect("unique_key_type"),
        "text"
    );

    let collation: String = sqlx::query_scalar(
        "SELECT collation_name FROM information_schema.columns \
         WHERE table_schema = 'public' AND table_name = 'background_jobs' \
         AND column_name = 'unique_key'",
    )
    .fetch_one(&pool)
    .await
    .expect("the unique-key collation");
    assert_eq!(collation, "C");

    let running_index: String = sqlx::query_scalar(
        "SELECT string_agg(attribute.attname, ',' ORDER BY key.ordinality) \
         FROM pg_index AS index \
         JOIN LATERAL unnest(index.indkey) WITH ORDINALITY AS key(attnum, ordinality) ON true \
         JOIN pg_attribute AS attribute \
           ON attribute.attrelid = index.indrelid AND attribute.attnum = key.attnum \
         WHERE index.indexrelid = 'background_jobs_running'::regclass",
    )
    .fetch_one(&pool)
    .await
    .expect("the replacement running index");
    assert_eq!(running_index, "kind,claim_expires_at,not_before,id");
    super::close(&[&pool]).await;
}

#[sqlx::test(migrations = false)]
async fn e9_forward_conversion_refuses_each_incompatible_legacy_representation(pool: PgPool) {
    historical_schema(&pool).await;
    for (case, payload, unique_key) in [
        ("invalid JSON", b"not-json".as_slice(), b"key".as_slice()),
        (
            "invalid UTF8 payload",
            b"\xff".as_slice(),
            b"key".as_slice(),
        ),
        (
            "decoded NUL payload",
            br#""\u0000""#.as_slice(),
            b"key".as_slice(),
        ),
        ("numeric range", b"1e1000000".as_slice(), b"key".as_slice()),
        (
            "invalid UTF8 unique key",
            b"{}".as_slice(),
            b"\xff".as_slice(),
        ),
        ("NUL unique key", b"{}".as_slice(), b"\0".as_slice()),
    ] {
        let id = insert_historical_row(&pool, payload, unique_key).await;
        let error = sqlx::raw_sql(FORWARD).execute(&pool).await.expect_err(case);
        assert!(error.as_database_error().is_some(), "{case}: {error}");
        assert_historical_shape(&pool).await;
        let unchanged: bool = sqlx::query_scalar(
            "SELECT payload = $2 AND unique_key = $3 FROM background_jobs WHERE id::text = $1",
        )
        .bind(&id)
        .bind(payload)
        .bind(unique_key)
        .fetch_one(&pool)
        .await
        .expect("the refused row remains byte-for-byte unchanged");
        assert!(unchanged, "{case}: the refused row changed");
        sqlx::query("DELETE FROM background_jobs WHERE id::text = $1")
            .bind(id)
            .execute(&pool)
            .await
            .expect("remove the refused fixture row");
    }
    super::close(&[&pool]).await;
}
