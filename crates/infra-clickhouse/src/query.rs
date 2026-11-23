use std::borrow::Cow;
use std::num::{NonZeroU64, NonZeroUsize};
use std::time::Duration;

use clickhouse::types::{MAX_DECIMAL_SCALE, serialize_decimal_param};
use operation_context::OperationContext;
use serde::Serialize;
use smallvec::SmallVec;

use crate::error::{
    FailureReason, ObservedFailure, QueryError, classify_query_error, is_auth_failure,
};
use crate::observe::{Operation, OperationGuard};
use crate::{
    ClickHouse, Decimal32, Decimal64, Decimal128, QuerySettings, format_execution_timeout_secs,
    within_bounded,
};

const NANOS_PER_SEC: u128 = 1_000_000_000;

pub trait ReadRow: clickhouse::RowOwned + clickhouse::RowRead + Send {}

impl<R: clickhouse::RowOwned + clickhouse::RowRead + Send> ReadRow for R {}

#[derive(Clone, derive_more::Debug)]
#[debug("Param(..)")]
pub struct Param(ParamValue);

impl Param {
    #[must_use]
    pub const fn datetime_secs(value: time::OffsetDateTime) -> Self {
        Self(ParamValue::DateTimeSecs(value))
    }

    #[must_use]
    pub const fn decimal(unscaled: i128, scale: u8) -> Self {
        Self(ParamValue::Decimal(unscaled, scale))
    }
}

#[derive(Clone)]
enum ParamValue {
    Bool(bool),
    I8(i8),
    I16(i16),
    I32(i32),
    I64(i64),
    I128(i128),
    Isize(isize),
    U8(u8),
    U16(u16),
    U32(u32),
    U64(u64),
    U128(u128),
    Usize(usize),
    F32(f32),
    F64(f64),
    Str(String),
    Uuid(uuid::Uuid),
    Date(time::Date),
    DateTime(time::OffsetDateTime),
    DateTimeSecs(time::OffsetDateTime),
    Decimal(i128, u8),
    None,
    Array(Vec<Self>),
}

impl Serialize for ParamValue {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Bool(v) => v.serialize(serializer),
            Self::I8(v) => v.serialize(serializer),
            Self::I16(v) => v.serialize(serializer),
            Self::I32(v) => v.serialize(serializer),
            Self::I64(v) => v.serialize(serializer),
            Self::I128(v) => v.serialize(serializer),
            Self::Isize(v) => v.serialize(serializer),
            Self::U8(v) => v.serialize(serializer),
            Self::U16(v) => v.serialize(serializer),
            Self::U32(v) => v.serialize(serializer),
            Self::U64(v) => v.serialize(serializer),
            Self::U128(v) => v.serialize(serializer),
            Self::Usize(v) => v.serialize(serializer),
            Self::F32(v) => v.serialize(serializer),
            Self::F64(v) => v.serialize(serializer),
            Self::Str(v) => v.serialize(serializer),
            Self::Uuid(v) => {
                let mut buf = uuid::Uuid::encode_buffer();
                serializer.serialize_str(v.as_hyphenated().encode_lower(&mut buf))
            }
            Self::Date(v) => serializer.collect_str(&format_args!(
                "{:04}-{:02}-{:02}",
                v.year(),
                u8::from(v.month()),
                v.day()
            )),
            Self::DateTime(v) => serialize_datetime_param(*v, true, serializer),
            Self::DateTimeSecs(v) => serialize_datetime_param(*v, false, serializer),
            Self::Decimal(unscaled, scale) => {
                serialize_decimal_param(*unscaled, *scale, serializer)
            }
            Self::None => serializer.serialize_none(),
            Self::Array(items) => items.serialize(serializer),
        }
    }
}

fn serialize_datetime_param<S: serde::Serializer>(
    v: time::OffsetDateTime,
    include_subsec: bool,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    if !include_subsec {
        let secs = v.unix_timestamp();
        return if secs < 0 {
            serializer.collect_str(&format_args!("-{:010}", secs.unsigned_abs()))
        } else {
            serializer.collect_str(&format_args!("{secs:010}"))
        };
    }
    let total_ns = v.unix_timestamp_nanos();
    if (-999_999_999..0).contains(&total_ns) {
        return serializer.collect_str(&format_args!("1969-12-31 23:59:59.{:09}", v.nanosecond()));
    }
    let sign = if total_ns < 0 { "-" } else { "" };
    let abs = total_ns.unsigned_abs();
    let whole = abs / NANOS_PER_SEC;
    let frac = abs % NANOS_PER_SEC;
    if frac == 0 {
        serializer.collect_str(&format_args!("{sign}{whole:010}"))
    } else {
        serializer.collect_str(&format_args!("{sign}{whole:010}.{frac:09}"))
    }
}

macro_rules! impl_param_from {
    ($($ty:ty => $variant:ident),* $(,)?) => {
        $(
            impl From<$ty> for Param {
                fn from(value: $ty) -> Self {
                    Self(ParamValue::$variant(value))
                }
            }
        )*
    };
}

impl_param_from!(
    bool => Bool,
    i8 => I8,
    i16 => I16,
    i32 => I32,
    i64 => I64,
    i128 => I128,
    isize => Isize,
    u8 => U8,
    u16 => U16,
    u32 => U32,
    u64 => U64,
    u128 => U128,
    usize => Usize,
    f32 => F32,
    f64 => F64,
    String => Str,
    uuid::Uuid => Uuid,
    time::Date => Date,
    time::OffsetDateTime => DateTime,
);

impl<const SCALE: u8> From<Decimal32<SCALE>> for Param {
    fn from(value: Decimal32<SCALE>) -> Self {
        Self::decimal(i128::from(value.unscaled()), value.scale())
    }
}

impl<const SCALE: u8> From<Decimal64<SCALE>> for Param {
    fn from(value: Decimal64<SCALE>) -> Self {
        Self::decimal(i128::from(value.unscaled()), value.scale())
    }
}

impl<const SCALE: u8> From<Decimal128<SCALE>> for Param {
    fn from(value: Decimal128<SCALE>) -> Self {
        Self::decimal(value.unscaled(), value.scale())
    }
}

impl From<&str> for Param {
    fn from(value: &str) -> Self {
        Self(ParamValue::Str(value.to_owned()))
    }
}

impl From<&String> for Param {
    fn from(value: &String) -> Self {
        Self::from(value.as_str())
    }
}

impl From<Cow<'_, str>> for Param {
    fn from(value: Cow<'_, str>) -> Self {
        Self(ParamValue::Str(value.into_owned()))
    }
}

impl From<time::UtcDateTime> for Param {
    fn from(value: time::UtcDateTime) -> Self {
        time::OffsetDateTime::from(value).into()
    }
}

impl<T: Into<Self>> From<Option<T>> for Param {
    fn from(value: Option<T>) -> Self {
        match value {
            Some(inner) => inner.into(),
            None => Self(ParamValue::None),
        }
    }
}

impl<T: Into<Self>> From<Vec<T>> for Param {
    fn from(values: Vec<T>) -> Self {
        Self(ParamValue::Array(
            values.into_iter().map(|value| value.into().0).collect(),
        ))
    }
}

impl<T: Clone + Into<Self>> From<&[T]> for Param {
    fn from(values: &[T]) -> Self {
        Self(ParamValue::Array(
            values.iter().cloned().map(|value| value.into().0).collect(),
        ))
    }
}

impl<T: Into<Self>, const N: usize> From<[T; N]> for Param {
    fn from(values: [T; N]) -> Self {
        Self(ParamValue::Array(
            values.into_iter().map(|value| value.into().0).collect(),
        ))
    }
}

impl<T: Clone + Into<Self>, const N: usize> From<&[T; N]> for Param {
    fn from(values: &[T; N]) -> Self {
        Self::from(values.as_slice())
    }
}

impl<T: Clone + Into<Self>> From<&Vec<T>> for Param {
    fn from(values: &Vec<T>) -> Self {
        Self::from(values.as_slice())
    }
}

#[must_use]
#[derive(Debug)]
pub struct Query<'a> {
    client: &'a ClickHouse,
    summary: &'static str,
    sql: &'a str,
    params: SmallVec<[(Cow<'a, str>, Param); 4]>,
    timeout: Option<Duration>,
    max_result_rows: Option<NonZeroU64>,
    max_result_bytes: Option<NonZeroUsize>,
}

impl<'a> Query<'a> {
    pub(crate) fn new(client: &'a ClickHouse, summary: &'static str, sql: &'a str) -> Self {
        Self {
            client,
            summary,
            sql,
            params: SmallVec::new(),
            timeout: None,
            max_result_rows: None,
            max_result_bytes: None,
        }
    }

    pub fn param(mut self, name: impl Into<Cow<'a, str>>, value: impl Into<Param>) -> Self {
        self.params.push((name.into(), value.into()));
        self
    }

    pub const fn timeout(mut self, limit: Duration) -> Self {
        self.timeout = Some(limit);
        self
    }

    pub const fn max_result_rows(mut self, limit: NonZeroU64) -> Self {
        self.max_result_rows = Some(limit);
        self
    }

    pub const fn max_result_bytes(mut self, limit: NonZeroUsize) -> Self {
        self.max_result_bytes = Some(limit);
        self
    }

    pub async fn fetch_one<R: ReadRow>(self, ctx: &OperationContext) -> Result<R, QueryError> {
        self.execute_bounded::<R, _>(ctx, Operation::FetchOne, Some(2), |mut rows| {
            match rows.len() {
                0 => Err(ObservedFailure::with_reason(
                    QueryError::NotFound,
                    FailureReason::NotFound,
                )),
                1 => Ok(rows.remove(0)),
                _ => Err(ObservedFailure::with_reason(
                    QueryError::TooManyRows,
                    FailureReason::TooManyRows,
                )),
            }
        })
        .await
    }

    pub async fn fetch_optional<R: ReadRow>(
        self,
        ctx: &OperationContext,
    ) -> Result<Option<R>, QueryError> {
        self.execute_bounded::<R, _>(
            ctx,
            Operation::FetchOptional,
            Some(2),
            |mut rows| match rows.len() {
                0 => Ok(None),
                1 => Ok(Some(rows.remove(0))),
                _ => Err(ObservedFailure::with_reason(
                    QueryError::TooManyRows,
                    FailureReason::TooManyRows,
                )),
            },
        )
        .await
    }

    pub async fn fetch_all<R: ReadRow>(self, ctx: &OperationContext) -> Result<Vec<R>, QueryError> {
        self.execute_bounded::<R, _>(ctx, Operation::FetchAll, None, Ok)
            .await
    }

    async fn execute_bounded<R: ReadRow, T>(
        self,
        ctx: &OperationContext,
        operation: Operation,
        cardinality_cap: Option<usize>,
        finish_rows: impl FnOnce(Vec<R>) -> Result<T, ObservedFailure<QueryError>>,
    ) -> Result<T, QueryError> {
        let shared = &self.client.inner.shared;
        if shared.is_closing() {
            return Err(QueryError::Unavailable);
        }
        ctx.check().map_err(QueryError::Stopped)?;
        validate_query_inputs(self.sql, &self.params, self.timeout)?;

        let Ok(_permit) = shared.read_permits.try_acquire() else {
            shared.metrics.record_admission_refused(operation);
            return Err(QueryError::Busy);
        };
        let _task_token = shared.tasks.token();
        if shared.is_closing() {
            return Err(QueryError::Unavailable);
        }

        let mut guard =
            OperationGuard::start(&shared.metrics, operation, self.summary, &shared.endpoint);
        let result = async {
            let policy = &shared.policy;
            let effective_timeout = policy
                .query_timeout
                .min(self.timeout.unwrap_or(policy.query_timeout));
            let effective_max_rows = policy
                .max_result_rows
                .min(self.max_result_rows.unwrap_or(policy.max_result_rows))
                .get();
            let server_max_rows = cardinality_cap.map_or(effective_max_rows, |cap| {
                effective_max_rows.min(u64::try_from(cap).unwrap_or(u64::MAX))
            });
            let effective_max_bytes = policy
                .max_result_bytes
                .min(self.max_result_bytes.unwrap_or(policy.max_result_bytes))
                .get();
            let max_bytes_u64 = u64::try_from(effective_max_bytes).unwrap_or(u64::MAX);

            let bounded = ctx.bounded(effective_timeout);
            bounded.check().map_err(ObservedFailure::from_stop)?;

            let creds = shared.credentials.load();
            let mut client = creds.read_client.clone();
            if policy.propagate_trace_context {
                client = guard.inject_trace_context(client);
            }
            let (ch_query, query_id) = build_clickhouse_query(
                &client,
                shared,
                self.sql,
                self.params,
                bounded.remaining(),
                server_max_rows,
                effective_max_bytes,
            );
            guard.record_query_id(query_id);
            bounded.check().map_err(ObservedFailure::from_stop)?;
            let server_cardinality_capped =
                policy.query_settings == QuerySettings::Client && cardinality_cap.is_some();
            let cursor = ch_query
                .fetch::<R>()
                .map_err(|err| classify_query_error(&err, server_cardinality_capped))?
                .with_max_decoded_bytes(max_bytes_u64);

            consume_bounded_rows(
                cursor,
                &bounded,
                effective_max_rows,
                cardinality_cap,
                server_cardinality_capped,
                finish_rows,
            )
            .await
        }
        .await;
        if let Err(failure) = &result
            && is_auth_failure(failure.detail)
        {
            shared.notify_auth_failure();
        }
        guard.finish_query(result)
    }
}

fn build_clickhouse_query(
    client: &clickhouse::Client,
    shared: &crate::Shared,
    sql: &str,
    params: SmallVec<[(Cow<'_, str>, Param); 4]>,
    remaining: Duration,
    effective_max_rows: u64,
    effective_max_bytes: usize,
) -> (clickhouse::query::Query, String) {
    let buffer_size = shared
        .policy
        .http_response_buffer_size
        .get()
        .min(effective_max_bytes);
    let query_id = uuid::Uuid::new_v4().to_string();
    let mut buffer_buf = std::fmt::NumBuffer::new();
    let mut ch_query = client
        .query_raw(sql)
        .with_setting("wait_end_of_query", "1")
        .with_setting("buffer_size", buffer_size.format_into(&mut buffer_buf))
        .with_setting("query_id", &query_id);

    if shared.policy.query_settings == QuerySettings::Client {
        let timeout_secs = format_execution_timeout_secs(remaining);
        let mut rows_buf = std::fmt::NumBuffer::new();
        let mut bytes_buf = std::fmt::NumBuffer::new();
        ch_query = shared
            .apply_client_budget(ch_query, &timeout_secs, |query, key, value| {
                query.with_setting(key, value)
            })
            .with_setting("result_overflow_mode", "throw")
            .with_setting(
                "max_result_rows",
                effective_max_rows.format_into(&mut rows_buf),
            )
            .with_setting(
                "max_result_bytes",
                effective_max_bytes.format_into(&mut bytes_buf),
            )
            .with_setting("readonly", "2")
            .with_setting("cancel_http_readonly_queries_on_client_close", "1")
            .with_setting("session_timezone", "UTC");
    }

    for (name, value) in params {
        ch_query = ch_query.param(name.as_ref(), &value.0);
    }
    (ch_query, query_id)
}

async fn consume_bounded_rows<R: ReadRow, T>(
    mut cursor: clickhouse::query::RowCursor<R>,
    bounded: &operation_context::BoundedContext,
    effective_max_rows: u64,
    cardinality_cap: Option<usize>,
    server_cardinality_capped: bool,
    finish_rows: impl FnOnce(Vec<R>) -> Result<T, ObservedFailure<QueryError>>,
) -> Result<(T, u64, u64), ObservedFailure<QueryError>> {
    let mut rows = Vec::new();
    loop {
        let next = within_bounded(bounded, cursor.next())
            .await
            .map_err(ObservedFailure::from_stop)?;
        match next {
            Ok(Some(row)) => {
                if cursor.returned_rows() > effective_max_rows {
                    return Err(ObservedFailure::with_reason(
                        QueryError::TooLarge,
                        FailureReason::ResultLimit,
                    ));
                }
                rows.push(row);
                if let Some(cap) = cardinality_cap
                    && rows.len() >= cap
                {
                    let returned_rows = cursor.returned_rows();
                    let decoded_bytes = cursor.decoded_bytes();
                    return finish_rows(rows).map(|value| (value, returned_rows, decoded_bytes));
                }
            }
            Ok(None) => {
                let returned_rows = cursor.returned_rows();
                let decoded_bytes = cursor.decoded_bytes();
                return finish_rows(rows).map(|value| (value, returned_rows, decoded_bytes));
            }
            Err(err) => {
                if let Some(stop) = bounded.stopped() {
                    return Err(ObservedFailure::from_stop(stop));
                }
                return Err(classify_query_error(&err, server_cardinality_capped));
            }
        }
    }
}

fn validate_query_inputs(
    sql: &str,
    params: &[(Cow<'_, str>, Param)],
    timeout: Option<Duration>,
) -> Result<(), QueryError> {
    if sql.trim().is_empty() || timeout.is_some_and(|limit| limit.is_zero()) {
        return Err(QueryError::Rejected);
    }
    for (idx, (name, value)) in params.iter().enumerate() {
        if !is_valid_param_name(name) || !is_valid_param_value(&value.0) {
            return Err(QueryError::Rejected);
        }
        if params[..idx].iter().any(|(prev, _)| prev == name) {
            return Err(QueryError::Rejected);
        }
    }
    Ok(())
}

fn is_valid_param_value(value: &ParamValue) -> bool {
    match value {
        ParamValue::Decimal(_, scale) => *scale <= MAX_DECIMAL_SCALE,
        ParamValue::Array(items) => items.iter().all(is_valid_param_value),
        _ => true,
    }
}

fn is_valid_param_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 || name.starts_with("param_") {
        return false;
    }
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'a'..=b'z' | b'_'))
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
