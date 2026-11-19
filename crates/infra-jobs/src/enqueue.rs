use std::io::{self, Write};
use std::time::Duration;

use infra_postgres::{Tx, TxMode, observed};

use crate::kind::{self, InvalidDelay, JobId, JobKind, checked_delay_micros};
use crate::trace_context;

// template:begin outbox:jobs-outbox-module
pub mod outbox;
// template:end outbox:jobs-outbox-module

pub const MAX_PAYLOAD_BYTES: usize = 262_144;
const MAX_UNIQUE_KEY_BYTES: usize = 255;
const MIN_PAYLOAD_CAPACITY: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EnqueueOptions<'a> {
    pub delay: Duration,
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
    NoLongerLive,
}

#[derive(Debug, thiserror::Error)]
pub enum EnqueueError {
    // template:begin outbox:jobs-outbox-enqueue-error
    #[error("{} requires the dedicated outbox enqueue operation", outbox::KIND)]
    OutboxKind,
    #[error("payload contains a decoded NUL character")]
    PayloadContainsNul,
    // template:end outbox:jobs-outbox-enqueue-error
    #[error("job kind {0:?} is not a valid kind name")]
    InvalidKind(&'static str),
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
    options: EnqueueOptions<'_>,
) -> Result<Enqueued, EnqueueError> {
    // template:begin outbox:jobs-outbox-enqueue-guard
    if K::NAME == outbox::KIND {
        return Err(EnqueueError::OutboxKind);
    }
    // template:end outbox:jobs-outbox-enqueue-guard
    let prepared = prepare(payload, options)?;
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
            K::NAME,
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
        notify_workers_after_commit(tx, K::NAME);
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
    let prepared = prepare(
        payload,
        EnqueueOptions {
            delay: Duration::ZERO,
            unique_key: Some(unique_key),
        },
    )?;
    let stored = observed!(
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
    let mut writer = PayloadWriter::default();
    serde_json::to_writer(&mut writer, &decoded).map_err(EnqueueError::Serialize)?;
    Ok(writer.total == expected.len() && writer.retained == expected.as_bytes())
}

#[derive(Debug)]
struct Prepared<'a> {
    payload: String,
    unique_key: Option<&'a str>,
    delay_micros: i64,
}

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
    let payload = encode_payload(payload)?;
    serde_json::from_str::<K>(&payload).map_err(EnqueueError::PayloadDoesNotDecode)?;
    Ok(Prepared {
        payload,
        unique_key: options.unique_key,
        delay_micros,
    })
}

fn encode_payload(payload: &impl serde::Serialize) -> Result<String, EnqueueError> {
    let mut writer = PayloadWriter::default();
    json_payload::to_writer(&mut writer, payload).map_err(EnqueueError::Serialize)?;
    if writer.total > MAX_PAYLOAD_BYTES {
        return Err(EnqueueError::PayloadTooLarge {
            bytes: writer.total,
        });
    }
    String::from_utf8(writer.retained).map_err(|error| {
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
    use std::io::Write as _;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::{
        EnqueueError, EnqueueOptions, MAX_PAYLOAD_BYTES, MAX_UNIQUE_KEY_BYTES, PayloadWriter,
        prepare,
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
            let err = prepare(&payload, EnqueueOptions::default()).unwrap_err();
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
}
