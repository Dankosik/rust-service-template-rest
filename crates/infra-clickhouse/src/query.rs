use std::borrow::Cow;
use std::net::Ipv4Addr;
use std::num::{NonZeroU64, NonZeroUsize};
use std::time::Duration;

use clickhouse::types::{MAX_DECIMAL_SCALE, serialize_decimal_param};
use operation_context::OperationContext;
use serde::Serialize;
use smallvec::SmallVec;

use crate::client::{
    AdmitFailure, ClickHouse, QuerySettings, RuntimePolicy, Shared, apply_client_budget_settings,
    format_execution_timeout_secs, within_bounded,
};
use crate::error::{FailureReason, ObservedFailure, QueryError, QueryRowCap, classify_query_error};
use crate::observe::{ReadKind, ReadStats};
use crate::{Decimal32, Decimal64, Decimal128};

const NANOS_PER_SEC: u128 = 1_000_000_000;

pub trait ReadRow: clickhouse::RowOwned + clickhouse::RowRead + Send {}

impl<R: clickhouse::RowOwned + clickhouse::RowRead + Send> ReadRow for R {}

#[derive(Clone, derive_more::Debug)]
#[debug("Param(..)")]
pub struct Param<'a>(ParamValue<'a>);

impl Param<'_> {
    #[must_use]
    pub fn datetime_secs(value: impl Into<time::OffsetDateTime>) -> Self {
        Self(ParamValue::Scalar(ParamScalar::DateTimeSecs(value.into())))
    }

    #[must_use]
    pub const fn try_decimal(unscaled: i128, scale: u8) -> Option<Self> {
        if scale <= MAX_DECIMAL_SCALE {
            Some(Self(ParamValue::Scalar(ParamScalar::Decimal(
                unscaled, scale,
            ))))
        } else {
            None
        }
    }
}

#[derive(Clone, Copy)]
enum ParamScalar {
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
    Uuid(uuid::Uuid),
    Ipv4(Ipv4Addr),
    Date(time::Date),
    DateTime(time::OffsetDateTime),
    DateTimeSecs(time::OffsetDateTime),
    Decimal(i128, u8),
}

impl Serialize for ParamScalar {
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
            Self::Uuid(v) => {
                let mut buf = uuid::Uuid::encode_buffer();
                serializer.serialize_str(v.as_hyphenated().encode_lower(&mut buf))
            }
            Self::Ipv4(v) => serializer.collect_str(v),
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
        }
    }
}

#[derive(Clone)]
enum ParamValue<'a> {
    Scalar(ParamScalar),
    Str(Cow<'a, str>),
    None,
    Array(Vec<Self>),
}

impl Serialize for ParamValue<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Scalar(v) => v.serialize(serializer),
            Self::Str(v) => v.serialize(serializer),
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

impl ParamValue<'_> {
    fn borrow(&self) -> ParamValue<'_> {
        match self {
            Self::Scalar(v) => ParamValue::Scalar(*v),
            Self::Str(v) => ParamValue::Str(Cow::Borrowed(v.as_ref())),
            Self::None => ParamValue::None,
            Self::Array(items) => ParamValue::Array(items.iter().map(Self::borrow).collect()),
        }
    }
}

macro_rules! impl_param_from {
    ($($ty:ty => $variant:ident),* $(,)?) => {
        $(
            impl From<$ty> for Param<'_> {
                fn from(value: $ty) -> Self {
                    Self(ParamValue::Scalar(ParamScalar::$variant(value)))
                }
            }

            impl From<&$ty> for Param<'_> {
                fn from(value: &$ty) -> Self {
                    Self(ParamValue::Scalar(ParamScalar::$variant(*value)))
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
    uuid::Uuid => Uuid,
    Ipv4Addr => Ipv4,
    time::Date => Date,
    time::OffsetDateTime => DateTime,
);

macro_rules! impl_decimal_param_from {
    ($($ty:ident => |$v:ident| $unscaled:expr),* $(,)?) => {
        $(
            impl<const SCALE: u8> From<$ty<SCALE>> for Param<'_> {
                fn from($v: $ty<SCALE>) -> Self {
                    Self(ParamValue::Scalar(ParamScalar::Decimal(
                        $unscaled,
                        $v.scale(),
                    )))
                }
            }

            impl<const SCALE: u8> From<&$ty<SCALE>> for Param<'_> {
                fn from(value: &$ty<SCALE>) -> Self {
                    Self::from(*value)
                }
            }
        )*
    };
}

impl_decimal_param_from!(
    Decimal32 => |v| i128::from(v.unscaled()),
    Decimal64 => |v| i128::from(v.unscaled()),
    Decimal128 => |v| v.unscaled(),
);

impl From<String> for Param<'_> {
    fn from(value: String) -> Self {
        Self(ParamValue::Str(Cow::Owned(value)))
    }
}

impl<'a> From<&'a str> for Param<'a> {
    fn from(value: &'a str) -> Self {
        Self(ParamValue::Str(Cow::Borrowed(value)))
    }
}

impl<'a> From<&&'a str> for Param<'a> {
    fn from(value: &&'a str) -> Self {
        Self(ParamValue::Str(Cow::Borrowed(*value)))
    }
}

impl<'a> From<&'a String> for Param<'a> {
    fn from(value: &'a String) -> Self {
        Self(ParamValue::Str(Cow::Borrowed(value.as_str())))
    }
}

impl<'a> From<Cow<'a, str>> for Param<'a> {
    fn from(value: Cow<'a, str>) -> Self {
        Self(ParamValue::Str(value))
    }
}

impl<'a> From<&'a Cow<'_, str>> for Param<'a> {
    fn from(value: &'a Cow<'_, str>) -> Self {
        Self(ParamValue::Str(Cow::Borrowed(value.as_ref())))
    }
}

impl<'a> From<&'a Param<'_>> for Param<'a> {
    fn from(value: &'a Param<'_>) -> Self {
        Self(value.0.borrow())
    }
}

impl From<time::UtcDateTime> for Param<'_> {
    fn from(value: time::UtcDateTime) -> Self {
        Self(ParamValue::Scalar(ParamScalar::DateTime(
            time::OffsetDateTime::from(value),
        )))
    }
}

impl From<&time::UtcDateTime> for Param<'_> {
    fn from(value: &time::UtcDateTime) -> Self {
        Self::from(*value)
    }
}

impl<'a, T: Into<Param<'a>>> From<Option<T>> for Param<'a> {
    fn from(value: Option<T>) -> Self {
        match value {
            Some(inner) => inner.into(),
            None => Self(ParamValue::None),
        }
    }
}

mod sealed {
    use super::Param;

    pub trait BorrowParam<'b, 'a>: 'b {
        fn borrow_param(&'b self) -> Param<'a>;
    }

    impl<'a, 'b, T: 'b> BorrowParam<'b, 'a> for T
    where
        &'b T: Into<Param<'a>>,
    {
        fn borrow_param(&'b self) -> Param<'a> {
            self.into()
        }
    }
}

impl<'a, 'b, T: sealed::BorrowParam<'b, 'a>> From<&'b Option<T>> for Param<'a> {
    fn from(value: &'b Option<T>) -> Self {
        match value {
            Some(inner) => inner.borrow_param(),
            None => Self(ParamValue::None),
        }
    }
}

impl<'a, T: Into<Param<'a>>> From<Vec<T>> for Param<'a> {
    fn from(values: Vec<T>) -> Self {
        values.into_iter().collect()
    }
}

impl<'a, 'b, T: sealed::BorrowParam<'b, 'a>> From<&'b [T]> for Param<'a> {
    fn from(values: &'b [T]) -> Self {
        values
            .iter()
            .map(sealed::BorrowParam::borrow_param)
            .collect()
    }
}

impl<'a, 'b, T: sealed::BorrowParam<'b, 'a>> From<&&'b [T]> for Param<'a> {
    fn from(values: &&'b [T]) -> Self {
        Self::from(*values)
    }
}

impl<'a, T: Into<Param<'a>>, const N: usize> From<[T; N]> for Param<'a> {
    fn from(values: [T; N]) -> Self {
        values.into_iter().collect()
    }
}

impl<'a, 'b, T: sealed::BorrowParam<'b, 'a>, const N: usize> From<&'b [T; N]> for Param<'a> {
    fn from(values: &'b [T; N]) -> Self {
        Self::from(values.as_slice())
    }
}

impl<'a, 'b, T: sealed::BorrowParam<'b, 'a>> From<&'b Vec<T>> for Param<'a> {
    fn from(values: &'b Vec<T>) -> Self {
        Self::from(values.as_slice())
    }
}

impl<'a, T: Into<Param<'a>>> FromIterator<T> for Param<'a> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self(ParamValue::Array(
            iter.into_iter().map(|value| value.into().0).collect(),
        ))
    }
}

#[derive(Clone, Copy)]
enum ReadCardinality {
    All,
    AtMostOne { server_capped: bool },
}

impl ReadCardinality {
    const fn cap(self) -> Option<usize> {
        match self {
            Self::All => None,
            Self::AtMostOne { .. } => Some(2),
        }
    }

    const fn row_cap(self) -> QueryRowCap {
        match self {
            Self::AtMostOne {
                server_capped: true,
            } => QueryRowCap::ServerAtMostOne,
            Self::All
            | Self::AtMostOne {
                server_capped: false,
            } => QueryRowCap::Uncapped,
        }
    }
}

struct QueryLimits {
    timeout: Duration,
    max_rows: u64,
    server_max_rows: u64,
    max_bytes: usize,
    cardinality: ReadCardinality,
}

#[must_use]
#[derive(Debug)]
pub struct Query<'a> {
    client: &'a ClickHouse,
    summary: &'static str,
    sql: Cow<'a, str>,
    params: SmallVec<[(Cow<'a, str>, Param<'a>); 4]>,
    timeout: Option<Duration>,
    max_result_rows: Option<NonZeroU64>,
    max_result_bytes: Option<NonZeroUsize>,
}

impl<'a> Query<'a> {
    pub(crate) fn new(client: &'a ClickHouse, summary: &'static str, sql: Cow<'a, str>) -> Self {
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

    pub fn param(mut self, name: impl Into<Cow<'a, str>>, value: impl Into<Param<'a>>) -> Self {
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
        self.execute_bounded::<R, _>(ctx, ReadKind::One, |rows| {
            let mut iter = rows.into_iter();
            match (iter.next(), iter.next()) {
                (None, _) => Err(ObservedFailure::with_reason(
                    QueryError::NotFound,
                    FailureReason::NotFound,
                )),
                (Some(row), None) => Ok(row),
                (Some(_), Some(_)) => Err(ObservedFailure::with_reason(
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
        self.execute_bounded::<R, _>(ctx, ReadKind::Optional, |rows| {
            let mut iter = rows.into_iter();
            match (iter.next(), iter.next()) {
                (None, _) => Ok(None),
                (Some(row), None) => Ok(Some(row)),
                (Some(_), Some(_)) => Err(ObservedFailure::with_reason(
                    QueryError::TooManyRows,
                    FailureReason::TooManyRows,
                )),
            }
        })
        .await
    }

    pub async fn fetch_all<R: ReadRow>(self, ctx: &OperationContext) -> Result<Vec<R>, QueryError> {
        self.execute_bounded::<R, _>(ctx, ReadKind::All, Ok).await
    }

    fn resolve_limits(&self, policy: &RuntimePolicy, kind: ReadKind) -> QueryLimits {
        let timeout = policy
            .query_timeout
            .min(self.timeout.unwrap_or(policy.query_timeout));
        let max_rows = policy
            .max_result_rows
            .min(self.max_result_rows.unwrap_or(policy.max_result_rows))
            .get();
        let cardinality = match kind {
            ReadKind::All => ReadCardinality::All,
            ReadKind::One | ReadKind::Optional => ReadCardinality::AtMostOne {
                server_capped: matches!(policy.query_settings, QuerySettings::Client { .. }),
            },
        };
        let server_max_rows = cardinality.cap().map_or(max_rows, |cap| {
            max_rows.min(u64::try_from(cap).unwrap_or(u64::MAX))
        });
        let max_bytes = policy
            .max_result_bytes
            .min(self.max_result_bytes.unwrap_or(policy.max_result_bytes))
            .get();
        QueryLimits {
            timeout,
            max_rows,
            server_max_rows,
            max_bytes,
            cardinality,
        }
    }

    async fn execute_bounded<R: ReadRow, T>(
        self,
        ctx: &OperationContext,
        kind: ReadKind,
        finish_rows: impl FnOnce(Vec<R>) -> Result<T, ObservedFailure<QueryError>>,
    ) -> Result<T, QueryError> {
        let shared = self.client.shared();
        if shared.is_closing() {
            return Err(QueryError::Unavailable);
        }
        ctx.check().map_err(QueryError::Stopped)?;
        validate_query_inputs(&self.sql, &self.params, self.timeout)?;

        let mut admitted =
            shared
                .admit_read(kind, self.summary)
                .map_err(|failure| match failure {
                    AdmitFailure::Busy => QueryError::Busy,
                    AdmitFailure::Closing => QueryError::Unavailable,
                })?;
        let result = async {
            let limits = self.resolve_limits(&shared.policy, kind);
            let bounded = ctx.bounded(limits.timeout);
            bounded.check()?;

            let creds = shared.credentials.load();
            let mut client = creds.read_client.clone();
            if shared.policy.propagate_trace_context {
                client = admitted.guard().inject_trace_context(client);
            }
            let (ch_query, query_id) = build_clickhouse_query(
                &client,
                shared,
                &self.sql,
                &self.params,
                bounded.remaining(),
                &limits,
            );
            admitted.guard_mut().record_query_id(query_id);
            bounded.check()?;
            let cursor = ch_query
                .fetch::<R>()
                .map_err(|err| classify_query_error(&err, limits.cardinality.row_cap()))?
                .with_max_decoded_bytes(u64::try_from(limits.max_bytes).unwrap_or(u64::MAX));

            consume_bounded_rows(cursor, &bounded, &limits, finish_rows).await
        }
        .await;
        admitted.finish(result)
    }
}

fn build_clickhouse_query(
    client: &clickhouse::Client,
    shared: &Shared,
    sql: &str,
    params: &[(Cow<'_, str>, Param<'_>)],
    remaining: Duration,
    limits: &QueryLimits,
) -> (clickhouse::query::Query, String) {
    let buffer_size = shared
        .policy
        .http_response_buffer_size
        .get()
        .min(limits.max_bytes);
    let query_id = uuid::Uuid::new_v4().to_string();
    let mut buffer_buf = std::fmt::NumBuffer::new();
    let mut ch_query = client
        .query_raw(sql)
        .with_setting("wait_end_of_query", "1")
        .with_setting("buffer_size", buffer_size.format_into(&mut buffer_buf))
        .with_setting("query_id", &query_id);

    if let QuerySettings::Client { max_memory_usage } = shared.policy.query_settings {
        let timeout_secs = format_execution_timeout_secs(remaining);
        let mut rows_buf = std::fmt::NumBuffer::new();
        let mut bytes_buf = std::fmt::NumBuffer::new();
        ch_query = apply_client_budget_settings(
            ch_query,
            max_memory_usage,
            &timeout_secs,
            |query, key, value| query.with_setting(key, value),
        )
        .with_setting("result_overflow_mode", "throw")
        .with_setting(
            "max_result_rows",
            limits.server_max_rows.format_into(&mut rows_buf),
        )
        .with_setting(
            "max_result_bytes",
            limits.max_bytes.format_into(&mut bytes_buf),
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
    limits: &QueryLimits,
    finish_rows: impl FnOnce(Vec<R>) -> Result<T, ObservedFailure<QueryError>>,
) -> Result<(T, ReadStats), ObservedFailure<QueryError>> {
    let mut rows = Vec::new();
    loop {
        let next = within_bounded(bounded, cursor.next()).await?;
        match next {
            Ok(Some(row)) => {
                if cursor.returned_rows() > limits.max_rows {
                    return Err(ObservedFailure::with_reason(
                        QueryError::TooLarge,
                        FailureReason::ResultLimit,
                    ));
                }
                rows.push(row);
                if let Some(cap) = limits.cardinality.cap()
                    && rows.len() >= cap
                {
                    let stats = ReadStats {
                        rows: cursor.returned_rows(),
                        decoded_bytes: cursor.decoded_bytes(),
                    };
                    return finish_rows(rows).map(|value| (value, stats));
                }
            }
            Ok(None) => {
                let stats = ReadStats {
                    rows: cursor.returned_rows(),
                    decoded_bytes: cursor.decoded_bytes(),
                };
                return finish_rows(rows).map(|value| (value, stats));
            }
            Err(err) => {
                if let Some(stop) = bounded.stopped() {
                    return Err(stop.into());
                }
                return Err(classify_query_error(&err, limits.cardinality.row_cap()));
            }
        }
    }
}

fn validate_query_inputs(
    sql: &str,
    params: &[(Cow<'_, str>, Param<'_>)],
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

#[cfg(test)]
impl Param<'_> {
    pub(crate) fn borrowed_str_ptrs(&self) -> Vec<*const u8> {
        let mut ptrs = Vec::new();
        self.0.collect_borrowed_str_ptrs(&mut ptrs);
        ptrs
    }
}

#[cfg(test)]
impl ParamValue<'_> {
    fn collect_borrowed_str_ptrs(&self, out: &mut Vec<*const u8>) {
        match self {
            Self::Str(Cow::Borrowed(s)) => out.push(s.as_ptr()),
            Self::Array(items) => {
                for item in items {
                    item.collect_borrowed_str_ptrs(out);
                }
            }
            _ => {}
        }
    }
}
