//! The one insert path. It runs on the caller's open transaction and never
//! opens, commits, or rolls one back.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::io::{self, Write};
use std::sync::Mutex;
use std::time::Duration;

use infra_postgres::{Tx, observed};
use tokio::time::Instant;

use crate::kind::{self, InvalidDelay, JobId, JobKind, checked_delay_micros};
use crate::trace_context;

pub const MAX_PAYLOAD_BYTES: usize = 262_144;
const MAX_UNIQUE_KEY_BYTES: usize = 255;
/// `serde_json` writes one token per call, so doubling from an empty buffer
/// costs about log2(len) reallocations per job; typical payloads fit here.
const MIN_PAYLOAD_CAPACITY: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EnqueueOptions<'a> {
    /// How long the job waits before it may be claimed. The sub-microsecond
    /// part is dropped. At most [`crate::MAX_DELAY`].
    pub delay: Duration,
    /// 1 to 255 bytes of UTF-8 without control characters, when set.
    pub unique_key: Option<&'a str>,
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
    /// No matching live holder remained when the comparison locked the row.
    NoLongerLive,
}

#[derive(Debug, thiserror::Error)]
pub enum EnqueueError {
    #[error("job kind {0:?} is not a valid kind name")]
    InvalidKind(&'static str),
    #[error(
        "unique key must be 1 to {MAX_UNIQUE_KEY_BYTES} bytes of UTF-8 without control characters"
    )]
    InvalidUniqueKey,
    #[error(transparent)]
    InvalidDelay(#[from] InvalidDelay),
    #[error("payload contains a decoded NUL character")]
    PayloadContainsNul,
    #[error("payload could not be serialized as JSON")]
    Serialize(#[source] serde_json::Error),
    #[error("payload does not decode back as its job kind")]
    PayloadDoesNotDecode(#[source] serde_json::Error),
    #[error("payload is {bytes} bytes; the maximum is {MAX_PAYLOAD_BYTES}")]
    PayloadTooLarge { bytes: usize },
    /// The statement failed; the caller's transaction is aborted.
    ///
    /// Its SQLSTATE is [`infra_postgres::sqlstate`].
    #[error("the jobs statement failed")]
    Database(#[from] sqlx::Error),
}

/// Count the bytes `serde_json` emits without retaining the JSON. Preflights
/// own their work budget and apply the size limit to the complete payload.
///
/// # Errors
///
/// Returns an error if serialization or byte counting fails.
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

/// Validation failures send nothing and leave the transaction usable. A
/// [`EnqueueError::Database`] error means the caller's transaction is aborted;
/// [`infra_postgres::sqlstate`] names why. With a unique
/// key, a live job of the same kind and key yields [`Enqueued::Duplicate`]
/// and leaves the transaction usable. Under `REPEATABLE READ` or
/// `SERIALIZABLE`, a holder written after the caller's snapshot fails the
/// statement with `40001` instead.
///
/// Serialization runs once and synchronously before database access. Retained
/// JSON is bounded by [`MAX_PAYLOAD_BYTES`], while all successful output is
/// counted so a later serializer error still wins over size or decoded NUL.
/// Custom serializers must be finite, trusted, nonblocking code. Callers own
/// source size, collection cardinality and concurrent calls within their
/// transaction deadline; that deadline cannot preempt a serializer callback.
///
/// # Errors
///
/// [`EnqueueError`] when the kind name, unique key, delay, or payload is
/// refused, including a payload whose JSON does not decode back as `K`, or
/// when the statement fails.
#[cfg_attr(feature = "hotpath", hotpath::measure)]
pub async fn enqueue<K: JobKind>(
    tx: &mut Tx<'_>,
    payload: &K,
    options: EnqueueOptions<'_>,
) -> Result<Enqueued, EnqueueError> {
    let prepared = prepare(payload, options)?;
    let trace = trace_context::capture();
    let id = JobId::new_v7();
    // The conflict target, its WHERE, `COLLATE "C"`, and the state literals
    // repeat the partial unique index `background_jobs_live_unique_key` word
    // for word: PostgreSQL infers the arbiter index only from a match, and
    // `compare_live_payload` must find the row that index holds.
    let row = observed(
        "enqueue job",
        sqlx::query!(
            "INSERT INTO background_jobs (id, kind, payload, payload_text, unique_key, not_before, trace_context, trace_state) \
             VALUES ($7, $1, $2::text::jsonb, $2, $3::text COLLATE \"C\", \
                     statement_timestamp() + ($4::bigint * interval '1 microsecond'), $5, $6) \
             ON CONFLICT (kind, unique_key) WHERE unique_key IS NOT NULL AND state IN ('pending', 'running') \
             DO NOTHING \
             RETURNING 1 AS created",
            K::NAME,
            prepared.payload,
            prepared.unique_key,
            prepared.delay_micros,
            trace.trace_parent(),
            trace.trace_state(),
            id.0,
        )
        .fetch_optional(&mut *tx),
    )
    .await?;
    let Some(_created) = row else {
        return Ok(Enqueued::Duplicate);
    };
    if prepared.delay_micros == 0 && take_wake_slot(K::NAME, Instant::now()) {
        observed(
            "wake job workers",
            sqlx::query!(
                "SELECT pg_notify($1, $2)",
                crate::listener::WAKE_CHANNEL,
                K::NAME
            )
            .execute(&mut *tx),
        )
        .await?;
    }
    Ok(Enqueued::Created(id))
}

/// Compare a proposed payload with the live holder of `unique_key`.
///
/// Call only after [`enqueue`] returns [`Enqueued::Duplicate`] for the same
/// kind, key, and payload. The selected row stays locked until the caller
/// commits or rolls back `tx`; [`LivePayloadComparison::NoLongerLive`] means
/// the caller must not treat the duplicate as accepted.
/// Equality compares serialized job payloads byte for byte. A row from an
/// older writer is compared by re-serializing what its handler would decode.
/// Preparation has the same once-only serialization, late-error precedence
/// and finite source, callback, concurrency and deadline duties as [`enqueue`].
///
/// # Errors
///
/// [`EnqueueError`] when the kind, key, or proposed payload is refused, when
/// the locking statement fails, or when a legacy payload cannot be decoded
/// or re-serialized. A legacy payload error leaves the transaction usable.
pub async fn compare_live_payload<K: JobKind>(
    tx: &mut Tx<'_>,
    unique_key: &str,
    payload: &K,
) -> Result<LivePayloadComparison, EnqueueError> {
    let prepared = prepare(
        payload,
        EnqueueOptions {
            delay: Duration::ZERO,
            unique_key: Some(unique_key),
        },
    )?;
    // Matches `background_jobs_live_unique_key` as the insert in [`enqueue`]
    // does, so it reads the same live holder.
    let stored = observed(
        "compare job payload",
        sqlx::query!(
            "SELECT payload_text, CASE WHEN payload_text IS NULL THEN payload::text END AS legacy_payload \
             FROM background_jobs \
             WHERE kind = $1 \
               AND unique_key = $2::text COLLATE \"C\" \
               AND state IN ('pending', 'running') \
             FOR UPDATE",
            K::NAME,
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
        // The query projects the non-null JSONB payload here. Treat an absent
        // projection as invalid JSON too.
        let legacy = stored.legacy_payload.as_deref().unwrap_or_default();
        let decoded =
            serde_json::from_str::<K>(legacy).map_err(EnqueueError::PayloadDoesNotDecode)?;
        let mut writer = PayloadWriter::default();
        serde_json::to_writer(&mut writer, &decoded).map_err(EnqueueError::Serialize)?;
        writer.total == prepared.payload.len() && writer.retained == prepared.payload.as_bytes()
    };
    Ok(if same {
        LivePayloadComparison::Same
    } else {
        LivePayloadComparison::Different
    })
}

/// Reserve at most one wake per kind and process per claim cooldown. The slot
/// is spent before commit, so a skipped wake may leave the job to polling.
const WAKE_INTERVAL: Duration = crate::claim::CLAIM_COOLDOWN;

/// Reserve this kind's wake slot and report whether the enqueue should send the
/// notification. Every notifying commit takes PostgreSQL's notify queue lock,
/// so notifying each enqueue would serialize concurrent committers (measured:
/// -50% at 16 connections).
///
/// The table holds one entry per kind because a worker wakes only for the kinds
/// it registers: one shared interval would let a notification for one kind
/// suppress the next kind's, and that kind's worker would wait for its poll.
fn take_wake_slot(kind: &'static str, now: Instant) -> bool {
    #[cfg(not(feature = "hotpath"))]
    static LAST: Mutex<BTreeMap<&'static str, Instant>> = Mutex::new(BTreeMap::new());
    #[cfg(feature = "hotpath")]
    static LAST: std::sync::LazyLock<
        hotpath::wrap::std::sync::Mutex<BTreeMap<&'static str, Instant>>,
    > = std::sync::LazyLock::new(|| {
        // The expression arm avoids 0.28.4's unsafe export_name uniqueness check.
        hotpath::mutex!(Mutex::new(BTreeMap::new()), label = { "jobs-wake" })
    });
    let mut last = LAST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match last.entry(kind) {
        Entry::Occupied(entry) if now.saturating_duration_since(*entry.get()) < WAKE_INTERVAL => {
            false
        }
        entry => {
            entry.and_modify(|at| *at = now).or_insert(now);
            true
        }
    }
}

#[derive(Debug)]
struct Prepared<'a> {
    payload: String,
    unique_key: Option<&'a str>,
    delay_micros: i64,
}

/// Retain only the admitted prefix, but let serialization finish so a later
/// serializer error takes precedence over the exact oversized byte count.
#[derive(Default)]
struct PayloadWriter {
    retained: Vec<u8>,
    total: usize,
}

impl Write for PayloadWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.total = checked_serialized_len(self.total, bytes.len())?;
        let keep = bytes.len().min(MAX_PAYLOAD_BYTES - self.retained.len());
        let needed = self.retained.len() + keep;
        if needed > self.retained.capacity() {
            let capacity = self
                .retained
                .capacity()
                .saturating_mul(2)
                .max(needed)
                .clamp(MIN_PAYLOAD_CAPACITY, MAX_PAYLOAD_BYTES);
            self.retained.reserve_exact(capacity - self.retained.len());
        }
        self.retained.extend_from_slice(&bytes[..keep]);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn checked_serialized_len(total: usize, chunk_len: usize) -> io::Result<usize> {
    total
        .checked_add(chunk_len)
        .ok_or_else(|| io::Error::other("serialized payload length overflow"))
}

/// Validate and bind. A failure has sent nothing.
#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn prepare<'a, K: JobKind>(
    payload: &K,
    options: EnqueueOptions<'a>,
) -> Result<Prepared<'a>, EnqueueError> {
    if !kind::is_valid_kind_name(K::NAME) {
        return Err(EnqueueError::InvalidKind(K::NAME));
    }
    if options
        .unique_key
        .is_some_and(|key| !is_valid_unique_key(key))
    {
        return Err(EnqueueError::InvalidUniqueKey);
    }
    let delay_micros = checked_delay_micros(options.delay)?;
    let mut writer = PayloadWriter::default();
    json_payload::to_writer(&mut writer, payload).map_err(EnqueueError::Serialize)?;
    if writer.total > MAX_PAYLOAD_BYTES {
        return Err(EnqueueError::PayloadTooLarge {
            bytes: writer.total,
        });
    }
    // A successful serde_json serialization emits UTF-8; reuse its allocation.
    let payload = String::from_utf8(writer.retained).map_err(|error| {
        EnqueueError::Serialize(serde_json::Error::io(io::Error::new(
            io::ErrorKind::InvalidData,
            error,
        )))
    })?;
    if contains_decoded_nul(&payload) {
        return Err(EnqueueError::PayloadContainsNul);
    }
    serde_json::from_str::<K>(&payload).map_err(EnqueueError::PayloadDoesNotDecode)?;
    Ok(Prepared {
        payload,
        unique_key: options.unique_key,
        delay_micros,
    })
}

fn is_valid_unique_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= MAX_UNIQUE_KEY_BYTES && !key.chars().any(char::is_control)
}

/// Whether serializer-produced JSON contains an escape that decodes to NUL.
///
/// A backslash always consumes its following escape byte. In the JSON text
/// `\\u0000` (an escaped backslash, then `u0000`) the second backslash is
/// consumed by the first, and its following `u0000` stays literal.
fn contains_decoded_nul(payload: &str) -> bool {
    // Any decoded NUL is spelled `\u0000`; the substring search is several
    // times faster than the walk below, which then only runs for a payload
    // that contains that text.
    if !payload.contains(r"\u0000") {
        return false;
    }
    let payload = payload.as_bytes();
    let mut index = 0;
    while index < payload.len() {
        if payload[index] == b'\\' {
            if payload.get(index + 1..index + 6) == Some(b"u0000") {
                return true;
            }
            index += 2;
        } else {
            index += 1;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::io::Write as _;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::{
        EnqueueError, EnqueueOptions, MAX_PAYLOAD_BYTES, MAX_UNIQUE_KEY_BYTES, PayloadWriter,
        WAKE_INTERVAL, contains_decoded_nul, prepare, take_wake_slot,
    };
    use crate::JobKind;
    use crate::kind::{InvalidDelay, MAX_DELAY, checked_delay_micros};
    use serde::ser::Error as _;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    struct Widget {
        name: String,
    }

    impl JobKind for Widget {
        const NAME: &'static str = "widget";
    }

    #[derive(Serialize, Deserialize)]
    struct Raw(String);

    impl JobKind for Raw {
        const NAME: &'static str = "raw";
    }

    #[derive(Serialize, Deserialize)]
    struct Keyed(BTreeMap<String, String>);

    impl JobKind for Keyed {
        const NAME: &'static str = "keyed";
    }

    #[derive(Serialize, Deserialize)]
    struct BadKind;

    impl JobKind for BadKind {
        const NAME: &'static str = "Bad Name";
    }

    #[derive(Deserialize)]
    struct Duplicate;

    impl Serialize for Duplicate {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeMap as _;
            let mut map = serializer.serialize_map(Some(2))?;
            map.serialize_entry("key", "\0")?;
            map.serialize_entry("key", "later value")?;
            map.end()
        }
    }

    impl JobKind for Duplicate {
        const NAME: &'static str = "duplicate";
    }

    #[derive(Deserialize)]
    struct Refuse;

    impl Serialize for Refuse {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let _ = (self, serializer);
            Err(S::Error::custom("refused"))
        }
    }

    impl JobKind for Refuse {
        const NAME: &'static str = "refuse";
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
        const NAME: &'static str = "streamed";
    }

    fn widget() -> Widget {
        Widget {
            name: "x".to_owned(),
        }
    }

    #[test]
    fn invalid_kind_wins_over_a_bad_key() {
        let err = prepare(
            &BadKind,
            EnqueueOptions {
                delay: Duration::ZERO,
                unique_key: Some(""),
            },
        )
        .unwrap_err();
        std::assert_matches!(err, EnqueueError::InvalidKind("Bad Name"));
    }

    #[derive(Serialize, Deserialize)]
    struct Reading {
        value: f64,
    }

    impl JobKind for Reading {
        const NAME: &'static str = "reading";
    }

    #[test]
    fn non_finite_floats_are_serialization_errors_before_typed_decode() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let err = prepare(&Reading { value }, EnqueueOptions::default()).unwrap_err();
            std::assert_matches!(err, EnqueueError::Serialize(_), "{value}");
        }
        let prepared = prepare(&Reading { value: 1.5 }, EnqueueOptions::default()).unwrap();
        assert_eq!(prepared.payload, r#"{"value":1.5}"#);
    }

    #[test]
    fn key_and_delay_validation_precede_non_finite_serialization() {
        let payload = Reading { value: f64::NAN };
        for (key, invalid_key) in [("", true), ("valid", false)] {
            let err = prepare(
                &payload,
                EnqueueOptions {
                    delay: MAX_DELAY + Duration::from_nanos(1),
                    unique_key: Some(key),
                },
            )
            .unwrap_err();
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
        let err = prepare(&payload, EnqueueOptions::default()).unwrap_err();
        std::assert_matches!(err, EnqueueError::PayloadDoesNotDecode(_));
        assert_eq!(payload.calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn unique_key_bounds() {
        let accepted = "k".repeat(MAX_UNIQUE_KEY_BYTES);
        let prepared = prepare(
            &widget(),
            EnqueueOptions {
                delay: Duration::ZERO,
                unique_key: Some(&accepted),
            },
        )
        .unwrap();
        assert_eq!(prepared.unique_key, Some(accepted.as_str()));

        std::assert_matches!(
            prepare(
                &widget(),
                EnqueueOptions {
                    delay: Duration::ZERO,
                    unique_key: Some(""),
                },
            ),
            Err(EnqueueError::InvalidUniqueKey)
        );
        std::assert_matches!(
            prepare(
                &widget(),
                EnqueueOptions {
                    delay: Duration::ZERO,
                    unique_key: Some(&"k".repeat(MAX_UNIQUE_KEY_BYTES + 1)),
                },
            ),
            Err(EnqueueError::InvalidUniqueKey)
        );
        let crossing = format!("{}é", "a".repeat(254));
        assert_eq!(crossing.len(), 256);
        std::assert_matches!(
            prepare(
                &widget(),
                EnqueueOptions {
                    delay: Duration::ZERO,
                    unique_key: Some(&crossing),
                },
            ),
            Err(EnqueueError::InvalidUniqueKey)
        );
    }

    #[test]
    fn unique_key_rejects_control_characters() {
        for key in ["\n", "\u{0}", "\u{7f}", "\u{85}"] {
            std::assert_matches!(
                prepare(
                    &widget(),
                    EnqueueOptions {
                        delay: Duration::ZERO,
                        unique_key: Some(key),
                    },
                ),
                Err(EnqueueError::InvalidUniqueKey),
                "{key:?}"
            );
        }
    }

    #[test]
    fn delay_bounds() {
        assert!(
            prepare(
                &widget(),
                EnqueueOptions {
                    delay: MAX_DELAY,
                    unique_key: None,
                },
            )
            .is_ok()
        );
        std::assert_matches!(
            prepare(
                &widget(),
                EnqueueOptions {
                    delay: MAX_DELAY + Duration::from_nanos(1),
                    unique_key: None,
                },
            ),
            Err(EnqueueError::InvalidDelay(InvalidDelay))
        );
    }

    #[test]
    fn checked_delay_drops_sub_microseconds() {
        let prepared = prepare(
            &widget(),
            EnqueueOptions {
                delay: Duration::new(1, 999_999_999),
                unique_key: None,
            },
        )
        .unwrap();
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
        let prepared = prepare(&exact, EnqueueOptions::default()).unwrap();
        assert_eq!(prepared.payload.len(), MAX_PAYLOAD_BYTES);
        assert_eq!(prepared.payload, format!("\"{}\"", exact.0));

        for length in [MAX_PAYLOAD_BYTES - 1, MAX_PAYLOAD_BYTES * 8] {
            let too_big = Raw("a".repeat(length));
            let err = prepare(&too_big, EnqueueOptions::default()).unwrap_err();
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
        let prepared = prepare(&payload, EnqueueOptions::default()).unwrap();
        assert_eq!(prepared.payload, r#"{"a":"é\n\"\\","b":"🦀"}"#);
    }

    #[test]
    fn streamed_payload_serializes_once_with_size_before_nul_and_late_error_first() {
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
            let err = prepare(&payload, EnqueueOptions::default()).unwrap_err();
            match failure {
                StreamFailure::Custom => {
                    let EnqueueError::Serialize(error) = err else {
                        panic!("late serialization failure must precede size and NUL");
                    };
                    assert_eq!(error.to_string(), "late refusal");
                }
                StreamFailure::NonFinite => {
                    std::assert_matches!(err, EnqueueError::Serialize(_));
                }
                StreamFailure::None => {
                    // Each encoded NUL string is eight bytes, plus one comma;
                    // the surrounding brackets replace the missing first comma.
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

    #[test]
    fn serialization_retains_only_the_payload_limit_despite_large_size_hint() {
        let mut writer = PayloadWriter::default();
        serde_json::to_writer(&mut writer, &0).unwrap();
        assert!(writer.retained.capacity() < MAX_PAYLOAD_BYTES);

        let mut writer = PayloadWriter::default();
        let payload = Streamed {
            elements: MAX_PAYLOAD_BYTES,
            failure: StreamFailure::None,
            calls: AtomicUsize::new(0),
        };
        serde_json::to_writer(&mut writer, &payload).unwrap();
        assert_eq!(writer.retained.len(), MAX_PAYLOAD_BYTES);
        assert!(writer.retained.capacity() <= MAX_PAYLOAD_BYTES);
        assert_eq!(writer.total, payload.elements * 9 + 1);

        let chunk = vec![b'x'; MAX_PAYLOAD_BYTES * 4];
        let mut writer = PayloadWriter::default();
        writer.write_all(&chunk).unwrap();
        assert_eq!(writer.retained.len(), MAX_PAYLOAD_BYTES);
        assert!(writer.retained.capacity() <= MAX_PAYLOAD_BYTES);
        assert_eq!(writer.total, chunk.len());
    }

    #[test]
    fn payload_rejects_decoded_nul_but_keeps_literal_backslash_u0000() {
        std::assert_matches!(
            prepare(&Duplicate, EnqueueOptions::default()),
            Err(EnqueueError::PayloadContainsNul)
        );
        std::assert_matches!(
            prepare(&Raw("\0".to_owned()), EnqueueOptions::default()),
            Err(EnqueueError::PayloadContainsNul)
        );
        std::assert_matches!(
            prepare(
                &Keyed(BTreeMap::from([("\0".to_owned(), "value".to_owned())])),
                EnqueueOptions::default(),
            ),
            Err(EnqueueError::PayloadContainsNul)
        );
        assert!(prepare(&Raw(r"\u0000".to_owned()), EnqueueOptions::default()).is_ok());
    }

    #[test]
    fn a_backslash_consumes_the_byte_after_it_when_looking_for_a_decoded_nul() {
        for (text, decoded) in [
            (r"\u0000", true),
            (r"x\u0000", true),
            (r"\\u0000", false),
            (r"\\\u0000", true),
            (r"\n\u0000", true),
            (r"\u000", false),
            (r"abc\", false),
            (r"\u0000\", true),
            (r"\\u0000\", false),
            ("", false),
        ] {
            assert_eq!(contains_decoded_nul(text), decoded, "{text}");
        }
    }

    #[test]
    fn wake_slot_is_debounced_per_kind() {
        let now = tokio::time::Instant::now();
        assert!(take_wake_slot("wake.first", now));
        assert!(!take_wake_slot("wake.first", now + WAKE_INTERVAL / 2));
        // Another kind's worker is not woken by the first kind's notification.
        assert!(take_wake_slot("wake.second", now + WAKE_INTERVAL / 2));
        // A refused slot keeps the earlier instant, so the interval still holds.
        assert!(!take_wake_slot("wake.second", now + WAKE_INTERVAL / 2));
        assert!(take_wake_slot("wake.first", now + WAKE_INTERVAL));
    }

    #[test]
    fn serialize_failure() {
        let err = prepare(&Refuse, EnqueueOptions::default()).unwrap_err();
        std::assert_matches!(err, EnqueueError::Serialize(_));
    }
}
