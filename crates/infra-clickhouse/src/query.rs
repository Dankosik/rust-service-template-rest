use std::num::{NonZeroU64, NonZeroUsize};
use std::sync::atomic::Ordering;
use std::time::Duration;

use operation_context::{BoundedStop, OperationContext};
use serde::Serialize;
use smallvec::SmallVec;
use tracing::Instrument as _;

use crate::error::{QueryError, classify_query_error};
use crate::observe::{Operation, OperationGuard};
use crate::{ClickHouse, QuerySettings, within_bounded};

mod sealed {
    pub trait ReadRowSealed {}
    impl<R> ReadRowSealed for R where R: clickhouse::RowOwned + clickhouse::RowRead + Send + 'static {}
}

pub trait ReadRow:
    sealed::ReadRowSealed + clickhouse::RowOwned + clickhouse::RowRead + Send + 'static
{
}

impl<R> ReadRow for R where R: clickhouse::RowOwned + clickhouse::RowRead + Send + 'static {}

#[derive(Clone, derive_more::Debug)]
#[debug("Param(..)")]
pub struct Param(ParamValue);

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
    None,
    Array(Vec<Param>),
}

impl Serialize for Param {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
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
            Self::Uuid(v) => v.to_string().serialize(serializer),
            Self::Date(v) => {
                let formatted =
                    format!("{:04}-{:02}-{:02}", v.year(), u8::from(v.month()), v.day());
                formatted.serialize(serializer)
            }
            Self::DateTime(v) => {
                let utc = v.to_offset(time::UtcOffset::UTC);
                let ns = utc.nanosecond();
                let formatted = if ns == 0 {
                    format!(
                        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
                        utc.year(),
                        u8::from(utc.month()),
                        utc.day(),
                        utc.hour(),
                        utc.minute(),
                        utc.second(),
                    )
                } else {
                    format!(
                        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:09}",
                        utc.year(),
                        u8::from(utc.month()),
                        utc.day(),
                        utc.hour(),
                        utc.minute(),
                        utc.second(),
                        ns,
                    )
                };
                formatted.serialize(serializer)
            }
            Self::None => serializer.serialize_none(),
            Self::Array(items) => items.serialize(serializer),
        }
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

impl From<&str> for Param {
    fn from(value: &str) -> Self {
        Self(ParamValue::Str(value.to_owned()))
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
            values.into_iter().map(Into::into).collect(),
        ))
    }
}

impl<T: Clone + Into<Self>> From<&[T]> for Param {
    fn from(values: &[T]) -> Self {
        Self(ParamValue::Array(
            values.iter().cloned().map(Into::into).collect(),
        ))
    }
}

#[must_use]
#[derive(Debug)]
pub struct Query<'a> {
    client: &'a ClickHouse,
    summary: &'static str,
    sql: &'a str,
    params: SmallVec<[(&'a str, Param); 4]>,
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

    pub fn param(mut self, name: &'a str, value: impl Into<Param>) -> Self {
        self.params.push((name, value.into()));
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
                0 => Err(QueryError::NotFound),
                1 => Ok(rows.remove(0)),
                _ => Err(QueryError::TooManyRows),
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
                _ => Err(QueryError::TooManyRows),
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
        finish_rows: impl FnOnce(Vec<R>) -> Result<T, QueryError>,
    ) -> Result<T, QueryError> {
        let shared = &self.client.inner.shared;
        if shared.closing.load(Ordering::Acquire) {
            return Err(QueryError::Unavailable);
        }
        ctx.check().map_err(QueryError::Stopped)?;
        validate_query_inputs(self.sql, &self.params, self.timeout)?;

        let Ok(_permit) = shared.permits.clone().try_acquire_owned() else {
            shared.metrics.record_admission_refused(operation);
            return Err(QueryError::Busy);
        };
        let _task_token = shared.tasks.token();

        let guard =
            OperationGuard::start(&shared.metrics, operation, self.summary, &shared.endpoint);
        let span = guard.span();
        let result = async {
            let effective_timeout = shared
                .query_timeout
                .min(self.timeout.unwrap_or(shared.query_timeout));
            let effective_max_rows = shared.max_result_rows.min(
                self.max_result_rows
                    .map_or(shared.max_result_rows, NonZeroU64::get),
            );
            let effective_max_bytes = shared.max_result_bytes.min(
                self.max_result_bytes
                    .map_or(shared.max_result_bytes, NonZeroUsize::get),
            );

            let bounded = ctx.bounded(effective_timeout);
            bounded.check().map_err(stop_to_query_error)?;

            let creds = shared.credentials.load();
            let mut client = creds.read_client.clone();
            if shared.propagate_trace_context {
                client = guard.inject_trace_context(client);
            }
            let ch_query = build_clickhouse_query(
                &client,
                shared,
                self.sql,
                self.params,
                bounded.remaining(),
                effective_max_rows,
                effective_max_bytes,
            );
            bounded.check().map_err(stop_to_query_error)?;
            let cursor = ch_query
                .fetch::<R>()
                .map_err(|err| classify_query_error(&err))?;

            consume_bounded_rows(
                cursor,
                &bounded,
                effective_max_rows,
                effective_max_bytes,
                cardinality_cap,
                finish_rows,
            )
            .await
        }
        .instrument(span)
        .await;
        guard.finish_query(result)
    }
}

fn build_clickhouse_query(
    client: &clickhouse::Client,
    shared: &crate::Shared,
    sql: &str,
    params: SmallVec<[(&str, Param); 4]>,
    remaining: Duration,
    effective_max_rows: u64,
    effective_max_bytes: usize,
) -> clickhouse::query::Query {
    let buffer_size = shared.http_response_buffer_size.min(effective_max_bytes);
    let query_id = uuid::Uuid::new_v4().to_string();
    let mut ch_query = client
        .query_raw(sql)
        .with_setting("wait_end_of_query", "1")
        .with_setting("buffer_size", buffer_size.to_string())
        .with_setting("query_id", query_id);

    if shared.query_settings == QuerySettings::Client {
        let max_exec_secs = remaining
            .as_secs()
            .saturating_add(u64::from(remaining.subsec_nanos() > 0))
            .max(1);
        ch_query = ch_query
            .with_setting("max_execution_time", max_exec_secs.to_string())
            .with_setting("timeout_overflow_mode", "throw")
            .with_setting("result_overflow_mode", "throw")
            .with_setting("max_result_rows", effective_max_rows.to_string())
            .with_setting("max_result_bytes", effective_max_bytes.to_string())
            .with_setting("cancel_http_readonly_queries_on_client_close", "1");
        if let Some(mem) = shared.max_memory_usage {
            ch_query = ch_query.with_setting("max_memory_usage", mem.to_string());
        }
    }

    for (name, value) in params {
        ch_query = ch_query.param(name, value);
    }
    ch_query
}

async fn consume_bounded_rows<R: ReadRow, T>(
    mut cursor: clickhouse::query::RowCursor<R>,
    bounded: &operation_context::BoundedContext,
    effective_max_rows: u64,
    effective_max_bytes: usize,
    cardinality_cap: Option<usize>,
    finish_rows: impl FnOnce(Vec<R>) -> Result<T, QueryError>,
) -> Result<(T, u64, u64), QueryError> {
    let max_bytes_u64 = u64::try_from(effective_max_bytes).unwrap_or(u64::MAX);
    let mut rows = Vec::new();
    loop {
        let next = within_bounded(bounded, cursor.next())
            .await
            .map_err(stop_to_query_error)?;
        match next {
            Ok(Some(row)) => {
                if cursor.decoded_bytes() > max_bytes_u64
                    || cursor.returned_rows() > effective_max_rows
                {
                    return Err(QueryError::TooLarge);
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
                if cursor.decoded_bytes() > max_bytes_u64 {
                    return Err(QueryError::TooLarge);
                }
                let returned_rows = cursor.returned_rows();
                let decoded_bytes = cursor.decoded_bytes();
                return finish_rows(rows).map(|value| (value, returned_rows, decoded_bytes));
            }
            Err(err) => {
                if let Some(stop) = bounded.stopped() {
                    return Err(stop_to_query_error(stop));
                }
                return Err(classify_query_error(&err));
            }
        }
    }
}

fn validate_query_inputs(
    sql: &str,
    params: &[(&str, Param)],
    timeout: Option<Duration>,
) -> Result<(), QueryError> {
    if sql.trim().is_empty() || timeout.is_some_and(|limit| limit.is_zero()) {
        return Err(QueryError::Rejected);
    }
    for (idx, (name, _)) in params.iter().enumerate() {
        if !is_valid_param_name(name) {
            return Err(QueryError::Rejected);
        }
        if params[..idx].iter().any(|(prev, _)| prev == name) {
            return Err(QueryError::Rejected);
        }
    }
    Ok(())
}

fn is_valid_param_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 || name.starts_with("param_") {
        return false;
    }
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b'A'..=b'Z' | b'a'..=b'z' | b'_'))
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

const fn stop_to_query_error(stop: BoundedStop) -> QueryError {
    match stop {
        BoundedStop::Parent(stopped) => QueryError::Stopped(stopped),
        BoundedStop::Timeout => QueryError::Unavailable,
    }
}
