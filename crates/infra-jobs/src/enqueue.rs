use std::io::{self, Write};
use std::time::Duration;

use infra_postgres::{Tx, TxMode, observed};

use crate::kind::{InvalidDelay, JobId, JobKind, checked_delay_micros};
use crate::trace_context;

// template:begin outbox:jobs-outbox-module
pub mod outbox;
// template:end outbox:jobs-outbox-module

pub const MAX_PAYLOAD_BYTES: usize = 262_144;
const MAX_UNIQUE_KEY_BYTES: usize = 255;
const JSON_STRING_QUOTES_BYTES: usize = "\"\"".len();

#[derive(Debug)]
pub struct PayloadSizer {
    bytes: usize,
    poll_budget: usize,
}

impl PayloadSizer {
    pub const BYTES_PER_POLL: usize = 4096;

    #[must_use]
    pub const fn with_base(bytes: usize) -> Self {
        Self {
            bytes,
            poll_budget: Self::BYTES_PER_POLL,
        }
    }

    pub fn from_skeleton<T: serde::Serialize + ?Sized>(value: &T) -> Result<Self, EnqueueError> {
        Ok(Self::with_base(
            json_payload_len(value).map_err(EnqueueError::Serialize)?,
        ))
    }

    pub async fn add_str_with<E: From<EnqueueError>>(
        &mut self,
        field: &str,
        mut admit: impl FnMut(&str) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut rest = field;
        while !rest.is_empty() {
            let end = rest.floor_char_boundary(self.poll_budget);
            if end == 0 {
                tokio::task::yield_now().await;
                self.poll_budget = Self::BYTES_PER_POLL;
                continue;
            }
            let (fragment, tail) = rest.split_at(end);
            admit(fragment)?;
            let escaped = json_payload_len(fragment).map_err(EnqueueError::Serialize)?
                - JSON_STRING_QUOTES_BYTES;
            self.bytes = checked_serialized_len(self.bytes, escaped)
                .map_err(|error| EnqueueError::Serialize(serde_json::Error::io(error)))?;
            self.poll_budget -= end;
            rest = tail;
        }
        Ok(())
    }

    pub async fn add_str(&mut self, field: &str) -> Result<(), EnqueueError> {
        self.add_str_with(field, |_| Ok(())).await
    }

    #[must_use]
    pub const fn total_bytes(&self) -> usize {
        self.bytes
    }

    pub const fn check_limit(&self) -> Result<(), EnqueueError> {
        if self.bytes > MAX_PAYLOAD_BYTES {
            return Err(EnqueueError::PayloadTooLarge { bytes: self.bytes });
        }
        Ok(())
    }
}

#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enqueued {
    Created(JobId),
    Duplicate,
}

#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LivePayloadComparison {
    Same,
    Different,
    NoLongerLive,
}

#[derive(Debug, thiserror::Error)]
pub enum EnqueueError {
    #[error(
        "unique key must be 1 to {MAX_UNIQUE_KEY_BYTES} bytes of UTF-8 without control characters"
    )]
    InvalidUniqueKey,
    #[error(transparent)]
    InvalidDelay(#[from] InvalidDelay),
    #[error("payload could not be serialized as JSON")]
    Serialize(#[source] serde_json::Error),
    #[error("payload does not decode back as its job kind")]
    PayloadDoesNotDecode(#[source] serde_json::Error),
    #[error("payload is {bytes} bytes; the maximum is {MAX_PAYLOAD_BYTES}")]
    PayloadTooLarge { bytes: usize },
    #[error("the jobs statement failed")]
    Database(#[from] sqlx::Error),
}

pub fn json_payload_len<T: serde::Serialize + ?Sized>(
    value: &T,
) -> Result<usize, serde_json::Error> {
    struct ByteCount(usize);

    impl Write for ByteCount {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = checked_serialized_len(self.0, bytes.len())?;
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let mut count = ByteCount(0);
    serde_json::to_writer(&mut count, value)?;
    Ok(count.0)
}

#[cfg_attr(feature = "hotpath", hotpath::measure)]
pub async fn enqueue<K: JobKind>(
    tx: &mut Tx<'_, impl TxMode>,
    payload: &K,
) -> Result<JobId, EnqueueError> {
    enqueue_unkeyed(tx, payload, Duration::ZERO).await
}

#[cfg_attr(feature = "hotpath", hotpath::measure)]
pub async fn enqueue_delayed<K: JobKind>(
    tx: &mut Tx<'_, impl TxMode>,
    payload: &K,
    delay: Duration,
) -> Result<JobId, EnqueueError> {
    enqueue_unkeyed(tx, payload, delay).await
}

async fn enqueue_unkeyed<K: JobKind>(
    tx: &mut Tx<'_, impl TxMode>,
    payload: &K,
    delay: Duration,
) -> Result<JobId, EnqueueError> {
    match enqueue_inner(tx, payload, delay, None).await? {
        Enqueued::Created(id) => Ok(id),
        Enqueued::Duplicate => Err(EnqueueError::Database(sqlx::Error::RowNotFound)),
    }
}

#[cfg_attr(feature = "hotpath", hotpath::measure)]
pub async fn enqueue_unique<K: JobKind>(
    tx: &mut Tx<'_, impl TxMode>,
    payload: &K,
    unique_key: &str,
    delay: Duration,
) -> Result<Enqueued, EnqueueError> {
    enqueue_inner(tx, payload, delay, Some(unique_key)).await
}

async fn enqueue_inner<K: JobKind>(
    tx: &mut Tx<'_, impl TxMode>,
    payload: &K,
    delay: Duration,
    unique_key: Option<&str>,
) -> Result<Enqueued, EnqueueError> {
    let prepared = prepare(payload, delay, unique_key)?;
    let trace = trace_context::capture();
    let id = JobId::new_v7();
    let inserted = observed!(
        "enqueue job",
        sqlx::query!(
            "INSERT INTO background_jobs (id, kind, payload_text, unique_key, not_before, trace_context, trace_state) \
             VALUES ($6, $1, $7, $2::text COLLATE \"C\", \
                     statement_timestamp() + ($3::bigint * interval '1 microsecond'), $4, $5) \
             ON CONFLICT (kind, unique_key) WHERE unique_key IS NOT NULL AND state IN ('pending', 'running') \
             DO NOTHING",
            K::NAME.as_str(),
            prepared.unique_key,
            prepared.delay_micros,
            trace.trace_parent(),
            trace.trace_state(),
            id.0,
            prepared.payload,
        )
        .execute(&mut *tx),
    )
    .await?;
    if inserted.rows_affected() == 0 {
        return Ok(Enqueued::Duplicate);
    }
    if prepared.delay_micros == 0 {
        notify_workers_after_commit(tx, K::NAME.as_str());
    }
    Ok(Enqueued::Created(id))
}

fn notify_workers_after_commit(tx: &mut Tx<'_, impl TxMode>, kind: &'static str) {
    tx.notify_after_commit(
        crate::listener::WAKE_CHANNEL,
        kind,
        crate::claim::CLAIM_COOLDOWN,
    );
}

pub async fn compare_live_payload<K: JobKind>(
    tx: &mut Tx<'_, impl TxMode>,
    unique_key: &str,
    payload: &K,
) -> Result<LivePayloadComparison, EnqueueError> {
    let prepared = prepare(payload, Duration::ZERO, Some(unique_key))?;
    let stored = observed!(
        "compare job payload",
        sqlx::query!(
            "SELECT payload_text, CASE WHEN payload_text IS NULL THEN payload::text END AS legacy_payload \
             FROM background_jobs \
             WHERE kind = $1 \
               AND unique_key = $2::text COLLATE \"C\" \
               AND state IN ('pending', 'running') \
             FOR UPDATE",
            K::NAME.as_str(),
            prepared.unique_key,
        )
        .fetch_optional(&mut *tx),
    )
    .await?;
    let Some(stored) = stored else {
        return Ok(LivePayloadComparison::NoLongerLive);
    };
    let same = if let Some(payload_text) = stored.payload_text {
        payload_text == prepared.payload
    } else {
        let legacy = stored.legacy_payload.as_deref().unwrap_or_default();
        legacy_payload_matches::<K>(legacy, &prepared.payload)?
    };
    Ok(if same {
        LivePayloadComparison::Same
    } else {
        LivePayloadComparison::Different
    })
}

fn legacy_payload_matches<K: JobKind>(legacy: &str, expected: &str) -> Result<bool, EnqueueError> {
    let decoded = serde_json::from_str::<K>(legacy).map_err(EnqueueError::PayloadDoesNotDecode)?;
    match json_payload::to_vec_bounded(&decoded, MAX_PAYLOAD_BYTES) {
        Ok(encoded) => Ok(encoded == expected.as_bytes()),
        Err(json_payload::EncodeError::TooLarge { .. }) => Ok(false),
        Err(json_payload::EncodeError::Serialize(error)) => Err(EnqueueError::Serialize(error)),
    }
}

#[derive(Debug)]
struct Prepared<'a> {
    payload: String,
    unique_key: Option<&'a str>,
    delay_micros: i64,
}

fn checked_serialized_len(total: usize, chunk_len: usize) -> io::Result<usize> {
    total
        .checked_add(chunk_len)
        .ok_or_else(|| io::Error::other("serialized payload length overflow"))
}

#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn prepare<'a, K: JobKind>(
    payload: &K,
    delay: Duration,
    unique_key: Option<&'a str>,
) -> Result<Prepared<'a>, EnqueueError> {
    if unique_key.is_some_and(|key| !is_valid_unique_key(key)) {
        return Err(EnqueueError::InvalidUniqueKey);
    }
    let delay_micros = checked_delay_micros(delay)?;
    let payload = encode_payload(payload)?;
    serde_json::from_str::<K>(&payload).map_err(EnqueueError::PayloadDoesNotDecode)?;
    Ok(Prepared {
        payload,
        unique_key,
        delay_micros,
    })
}

fn encode_payload(payload: &impl serde::Serialize) -> Result<String, EnqueueError> {
    let encoded =
        json_payload::to_vec_bounded(payload, MAX_PAYLOAD_BYTES).map_err(|error| match error {
            json_payload::EncodeError::Serialize(error) => EnqueueError::Serialize(error),
            json_payload::EncodeError::TooLarge { bytes } => {
                EnqueueError::PayloadTooLarge { bytes }
            }
        })?;
    String::from_utf8(encoded).map_err(|error| {
        EnqueueError::Serialize(serde_json::Error::io(io::Error::new(
            io::ErrorKind::InvalidData,
            error,
        )))
    })
}

fn is_valid_unique_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= MAX_UNIQUE_KEY_BYTES && !key.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::{EnqueueError, MAX_PAYLOAD_BYTES, MAX_UNIQUE_KEY_BYTES, PayloadSizer, prepare};
    use crate::kind::{InvalidDelay, MAX_DELAY, checked_delay_micros};
    use crate::{JobKind, KindName};
    use serde::ser::Error as _;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    struct Widget {
        name: String,
    }

    impl JobKind for Widget {
        const NAME: KindName = KindName::new("widget");
    }

    #[derive(Serialize, Deserialize)]
    struct Raw(String);

    impl JobKind for Raw {
        const NAME: KindName = KindName::new("raw");
    }

    #[derive(Serialize, Deserialize)]
    struct Keyed(BTreeMap<String, String>);

    impl JobKind for Keyed {
        const NAME: KindName = KindName::new("keyed");
    }

    #[derive(Deserialize)]
    struct Streamed {
        elements: usize,
        failure: StreamFailure,
        #[serde(skip)]
        calls: AtomicUsize,
    }

    #[derive(Clone, Copy, Deserialize)]
    enum StreamFailure {
        None,
        Custom,
        NonFinite,
    }

    impl Serialize for Streamed {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeSeq as _;
            self.calls.fetch_add(1, Ordering::Relaxed);
            let mut sequence = serializer.serialize_seq(Some(usize::MAX))?;
            for _ in 0..self.elements {
                sequence.serialize_element("\0")?;
            }
            match self.failure {
                StreamFailure::None => {}
                StreamFailure::Custom => return Err(S::Error::custom("late refusal")),
                StreamFailure::NonFinite => sequence.serialize_element(&f64::INFINITY)?,
            }
            sequence.end()
        }
    }

    impl JobKind for Streamed {
        const NAME: KindName = KindName::new("streamed");
    }

    fn widget() -> Widget {
        Widget {
            name: "x".to_owned(),
        }
    }

    #[derive(Serialize, Deserialize)]
    struct Reading {
        value: f64,
    }

    impl JobKind for Reading {
        const NAME: KindName = KindName::new("reading");
    }

    #[test]
    fn key_and_delay_validation_precede_non_finite_serialization() {
        let payload = Reading { value: f64::NAN };
        for (key, invalid_key) in [("", true), ("valid", false)] {
            let err =
                prepare(&payload, MAX_DELAY + Duration::from_nanos(1), Some(key)).unwrap_err();
            if invalid_key {
                std::assert_matches!(err, EnqueueError::InvalidUniqueKey);
            } else {
                std::assert_matches!(err, EnqueueError::InvalidDelay(_));
            }
        }
    }

    #[test]
    fn a_serializable_payload_the_worker_cannot_decode_is_refused() {
        let payload = Streamed {
            elements: 0,
            failure: StreamFailure::None,
            calls: AtomicUsize::new(0),
        };
        let err = prepare(&payload, Duration::ZERO, None).unwrap_err();
        std::assert_matches!(err, EnqueueError::PayloadDoesNotDecode(_));
        assert_eq!(payload.calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn unique_key_bounds() {
        let accepted = "k".repeat(MAX_UNIQUE_KEY_BYTES);
        let prepared = prepare(&widget(), Duration::ZERO, Some(&accepted)).unwrap();
        assert_eq!(prepared.unique_key, Some(accepted.as_str()));

        std::assert_matches!(
            prepare(&widget(), Duration::ZERO, Some("")),
            Err(EnqueueError::InvalidUniqueKey)
        );
        std::assert_matches!(
            prepare(
                &widget(),
                Duration::ZERO,
                Some(&"k".repeat(MAX_UNIQUE_KEY_BYTES + 1)),
            ),
            Err(EnqueueError::InvalidUniqueKey)
        );
        let crossing = format!("{}é", "a".repeat(254));
        assert_eq!(crossing.len(), 256);
        std::assert_matches!(
            prepare(&widget(), Duration::ZERO, Some(&crossing)),
            Err(EnqueueError::InvalidUniqueKey)
        );
    }

    #[test]
    fn unique_key_rejects_control_characters() {
        for key in ["\n", "\u{0}", "\u{7f}", "\u{85}"] {
            std::assert_matches!(
                prepare(&widget(), Duration::ZERO, Some(key)),
                Err(EnqueueError::InvalidUniqueKey),
                "{key:?}"
            );
        }
    }

    #[test]
    fn delay_bounds() {
        assert!(prepare(&widget(), MAX_DELAY, None).is_ok());
        std::assert_matches!(
            prepare(&widget(), MAX_DELAY + Duration::from_nanos(1), None),
            Err(EnqueueError::InvalidDelay(InvalidDelay))
        );
    }

    #[test]
    fn checked_delay_drops_sub_microseconds() {
        let prepared = prepare(&widget(), Duration::new(1, 999_999_999), None).unwrap();
        assert_eq!(prepared.delay_micros, 1_999_999);
        assert_eq!(checked_delay_micros(Duration::ZERO), Ok(0));
        assert_eq!(checked_delay_micros(MAX_DELAY), Ok(3_153_600_000_000_000));
        assert_eq!(
            checked_delay_micros(MAX_DELAY + Duration::from_nanos(1)),
            Err(InvalidDelay)
        );
    }

    #[test]
    fn payload_size_bound() {
        let exact = Raw("a".repeat(MAX_PAYLOAD_BYTES - 2));
        let prepared = prepare(&exact, Duration::ZERO, None).unwrap();
        assert_eq!(prepared.payload.len(), MAX_PAYLOAD_BYTES);
        assert_eq!(prepared.payload, format!("\"{}\"", exact.0));

        for length in [MAX_PAYLOAD_BYTES - 1, MAX_PAYLOAD_BYTES * 8] {
            let too_big = Raw("a".repeat(length));
            let err = prepare(&too_big, Duration::ZERO, None).unwrap_err();
            std::assert_matches!(
                err,
                EnqueueError::PayloadTooLarge { bytes } if bytes == length + 2
            );
        }
    }

    #[test]
    fn payload_preserves_compact_json_bytes() {
        let payload = Keyed(BTreeMap::from([
            ("a".to_owned(), "é\n\"\\".to_owned()),
            ("b".to_owned(), "🦀".to_owned()),
        ]));
        let prepared = prepare(&payload, Duration::ZERO, None).unwrap();
        assert_eq!(prepared.payload, r#"{"a":"é\n\"\\","b":"🦀"}"#);
    }

    #[test]
    fn streamed_payload_serializes_once_and_late_errors_precede_the_size_check() {
        for failure in [
            StreamFailure::None,
            StreamFailure::Custom,
            StreamFailure::NonFinite,
        ] {
            let payload = Streamed {
                elements: MAX_PAYLOAD_BYTES,
                failure,
                calls: AtomicUsize::new(0),
            };
            let err = prepare(&payload, Duration::ZERO, None).unwrap_err();
            match failure {
                StreamFailure::Custom => {
                    let EnqueueError::Serialize(error) = err else {
                        panic!("late serialization failure must precede size");
                    };
                    assert_eq!(error.to_string(), "late refusal");
                }
                StreamFailure::NonFinite => {
                    std::assert_matches!(err, EnqueueError::Serialize(_));
                }
                StreamFailure::None => {
                    std::assert_matches!(
                        err,
                        EnqueueError::PayloadTooLarge { bytes }
                            if bytes == payload.elements * 9 + 1
                    );
                }
            }
            assert_eq!(payload.calls.load(Ordering::Relaxed), 1);
        }
    }

    #[derive(Default)]
    struct WakeCount(AtomicUsize);

    impl std::task::Wake for WakeCount {
        fn wake(self: std::sync::Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &std::sync::Arc<Self>) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn poll_to_completion<T>(future: impl Future<Output = T>) -> (T, usize) {
        let wakes = std::sync::Arc::new(WakeCount::default());
        let waker = std::task::Waker::from(std::sync::Arc::clone(&wakes));
        let mut context = std::task::Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        for polls in 1..=1024 {
            let previous = wakes.0.load(Ordering::Relaxed);
            match future.as_mut().poll(&mut context) {
                std::task::Poll::Ready(value) => return (value, polls),
                std::task::Poll::Pending => assert!(
                    wakes.0.load(Ordering::Relaxed) > previous,
                    "a bounded preflight poll must schedule its next wakeup"
                ),
            }
        }
        panic!("bounded future did not complete");
    }

    #[test]
    fn payload_sizer_matches_json_bytes_across_utf8_boundaries_and_shared_budget() {
        #[derive(Serialize)]
        struct Envelope<'a> {
            first: &'a str,
            second: &'a str,
        }

        let first = format!(
            "{}é😀\\\"\u{0001}",
            "x".repeat(PayloadSizer::BYTES_PER_POLL - 1)
        );
        let second = "y".repeat(256);
        let expected = serde_json::to_vec(&Envelope {
            first: &first,
            second: &second,
        })
        .unwrap()
        .len();

        let (result, polls) = poll_to_completion(async {
            let mut sizer = PayloadSizer::from_skeleton(&Envelope {
                first: "",
                second: "",
            })?;
            sizer.add_str(&first).await?;
            sizer.add_str(&second).await?;
            Ok::<_, EnqueueError>(sizer)
        });
        let sizer = result.unwrap();
        assert_eq!(sizer.total_bytes(), expected);
        assert!(polls >= 2);
        assert!(sizer.check_limit().is_ok());

        assert!(
            PayloadSizer::with_base(MAX_PAYLOAD_BYTES)
                .check_limit()
                .is_ok()
        );
        std::assert_matches!(
            PayloadSizer::with_base(MAX_PAYLOAD_BYTES + 1).check_limit(),
            Err(EnqueueError::PayloadTooLarge { bytes }) if bytes == MAX_PAYLOAD_BYTES + 1
        );

        let (overflow, _) = poll_to_completion(async {
            let mut sizer = PayloadSizer::with_base(usize::MAX);
            sizer.add_str("a").await
        });
        std::assert_matches!(overflow, Err(EnqueueError::Serialize(_)));
    }

    #[test]
    fn payload_sizer_admit_rejects_fragments_after_yielding() {
        #[derive(Debug)]
        enum Reject {
            Disallowed,
            Enqueue(EnqueueError),
        }

        impl From<EnqueueError> for Reject {
            fn from(error: EnqueueError) -> Self {
                Self::Enqueue(error)
            }
        }

        let field = format!("{}\n", "x".repeat(PayloadSizer::BYTES_PER_POLL * 2));
        let (result, polls) = poll_to_completion(async {
            let mut sizer = PayloadSizer::with_base(0);
            sizer
                .add_str_with(&field, |fragment| {
                    if fragment.contains('\n') {
                        Err(Reject::Disallowed)
                    } else {
                        Ok(())
                    }
                })
                .await
        });
        assert!(polls >= 3);
        std::assert_matches!(result, Err(Reject::Disallowed));
    }
}
