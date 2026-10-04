//! The one insert path. It runs on the caller's open transaction and never
//! opens, commits, or rolls one back.

use std::sync::Mutex;
use std::time::Duration;

use infra_postgres::{Tx, observed};
use tokio::time::Instant;

use crate::kind::{self, JobId, JobKind};
use crate::trace_context;

/// The longest JSON payload enqueue accepts: 256 KiB.
pub const MAX_PAYLOAD_BYTES: usize = 262_144;
/// The longest unique key enqueue accepts.
pub(crate) const MAX_UNIQUE_KEY_BYTES: usize = 255;
/// The longest delay enqueue accepts: 36500 days.
pub const MAX_DELAY: Duration = Duration::from_hours(36_500 * 24);

/// Delay and uniqueness for one enqueue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EnqueueOptions<'a> {
    /// How long the job waits before it may be claimed. The sub-microsecond
    /// part is dropped. At most [`MAX_DELAY`].
    pub delay: Duration,
    /// 1 to 255 bytes of UTF-8 without control characters, when set.
    pub unique_key: Option<&'a str>,
}

/// The outcome of one enqueue that did not fail.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enqueued {
    /// The job was inserted. The id is stable.
    Created(JobId),
    /// A live job already holds this kind and unique key.
    Duplicate,
}

/// The result of comparing a proposed payload with the live holder of a key.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LivePayloadComparison {
    /// The live holder has the same JSONB payload.
    Same,
    /// The live holder has a different JSONB payload.
    Different,
    /// No matching live holder remained when the comparison locked the row.
    NoLongerLive,
}

/// A delay outside the queue's supported range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("delay exceeds the maximum of 36500 days")]
pub struct InvalidDelay;

/// Why an enqueue did not insert a job.
#[derive(Debug, thiserror::Error)]
pub enum EnqueueError {
    /// The kind name is outside the kind-name grammar.
    #[error("job kind {0:?} is not a valid kind name")]
    InvalidKind(&'static str),
    /// The unique key is empty, too long, or contains a control character.
    #[error("unique key must be 1 to 255 bytes of UTF-8 without control characters")]
    InvalidUniqueKey,
    /// The delay is longer than [`MAX_DELAY`].
    #[error("delay exceeds the maximum of 36500 days")]
    InvalidDelay,
    /// The JSON payload decodes a NUL in a string or object key.
    #[error("payload contains a decoded NUL character")]
    PayloadContainsNul,
    /// The payload did not serialize as JSON.
    #[error("payload could not be serialized as JSON")]
    Serialize(#[source] serde_json::Error),
    /// The serialized payload is over [`MAX_PAYLOAD_BYTES`].
    #[error("payload is {bytes} bytes; the maximum is 262144")]
    PayloadTooLarge {
        /// The serialized size that was refused.
        bytes: usize,
    },
    /// The statement failed; the caller's transaction is aborted.
    ///
    /// Its SQLSTATE is [`infra_postgres::sqlstate`].
    #[error("the enqueue statement failed")]
    Database(#[source] sqlx::Error),
}

/// Write one job inside the caller's open transaction.
///
/// Validation failures send nothing and leave the transaction usable. A
/// [`EnqueueError::Database`] error means the caller's transaction is aborted;
/// [`infra_postgres::sqlstate`] names why. With a unique
/// key, a live job of the same kind and key yields [`Enqueued::Duplicate`]
/// and leaves the transaction usable. Under `REPEATABLE READ` or
/// `SERIALIZABLE`, a holder written after the caller's snapshot fails the
/// statement with `40001` instead; the uniqueness table in
/// `docs/background-jobs.md` gives every case.
///
/// # Errors
///
/// [`EnqueueError`] when the kind name, unique key, delay, or payload is
/// refused, or when the statement fails.
#[cfg_attr(feature = "hotpath", hotpath::measure)]
pub async fn enqueue<K: JobKind>(
    tx: &mut Tx<'_>,
    payload: &K,
    options: EnqueueOptions<'_>,
) -> Result<Enqueued, EnqueueError> {
    let prepared = prepare(payload, options)?;
    let (traceparent, tracestate) = trace_context::capture();
    let id = JobId::new_v7();
    // One insert on the caller's connection. A conflict with a live unique key
    // returns no row.
    let row = observed(
        "enqueue job",
        sqlx::query!(
            "INSERT INTO background_jobs (id, kind, payload, unique_key, not_before, trace_context, trace_state) \
             VALUES ($7, $1, $2::text::jsonb, $3::text COLLATE \"C\", \
                     statement_timestamp() + ($4::bigint * interval '1 microsecond'), $5, $6) \
             ON CONFLICT (kind, unique_key) WHERE unique_key IS NOT NULL AND state IN ('pending', 'running') \
             DO NOTHING \
             RETURNING 1 AS created",
            K::NAME,
            prepared.payload,
            prepared.unique_key,
            prepared.delay_micros,
            traceparent.as_deref(),
            tracestate.as_deref(),
            id.0,
        )
        .fetch_optional(&mut *tx),
    )
    .await
    .map_err(EnqueueError::Database)?;
    let Some(_created) = row else {
        return Ok(Enqueued::Duplicate);
    };
    if prepared.delay_micros == 0 && wake_due(K::NAME, Instant::now()) {
        // Wake idle workers of this kind when the caller's transaction commits.
        observed(
            "wake job workers",
            sqlx::query!(
                "SELECT pg_notify($1, $2)",
                crate::claim::WAKE_CHANNEL,
                K::NAME
            )
            .execute(&mut *tx),
        )
        .await
        .map_err(EnqueueError::Database)?;
    }
    Ok(Enqueued::Created(id))
}

/// Compare a proposed payload with the live holder of `unique_key`.
///
/// Call only after [`enqueue`] returns [`Enqueued::Duplicate`] for the same
/// kind, key, and payload. The selected row stays locked until the caller
/// commits or rolls back `tx`; [`LivePayloadComparison::NoLongerLive`] means
/// the caller must not treat the duplicate as accepted.
///
/// # Errors
///
/// [`EnqueueError`] when the kind, key, or payload is refused, or when the
/// locking statement fails.
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
    // Compare one live job's stored payload while retaining its row lock.
    let same = observed(
        "compare job payload",
        sqlx::query_scalar!(
            "SELECT payload = $3::text::jsonb AS \"same!\" \
             FROM background_jobs \
             WHERE kind = $1 \
               AND unique_key = $2::text COLLATE \"C\" \
               AND state IN ('pending', 'running') \
             FOR UPDATE",
            K::NAME,
            prepared.unique_key,
            prepared.payload,
        )
        .fetch_optional(&mut *tx),
    )
    .await
    .map_err(EnqueueError::Database)?;
    Ok(match same {
        Some(true) => LivePayloadComparison::Same,
        Some(false) => LivePayloadComparison::Different,
        None => LivePayloadComparison::NoLongerLive,
    })
}

/// At most one wake notification per kind and process in this interval, the
/// claim cooldown: a job whose notification was skipped becomes due while the
/// worker woken by the previous one keeps claiming.
const WAKE_INTERVAL: Duration = Duration::from_millis(25);

/// Whether this enqueue sends the wake notification. Every notifying commit
/// takes PostgreSQL's notify queue lock, so notifying each enqueue would
/// serialize concurrent committers (measured: -50% at 16 connections).
///
/// The interval is kept per kind because a worker wakes only for the kinds it
/// registers: one shared interval would let a notification for one kind
/// suppress the next kind's, and that kind's worker would wait for its poll.
fn wake_due(kind: &'static str, now: Instant) -> bool {
    #[cfg(not(feature = "hotpath"))]
    static LAST: Mutex<Vec<(&'static str, Instant)>> = Mutex::new(Vec::new());
    #[cfg(feature = "hotpath")]
    static LAST: std::sync::LazyLock<
        hotpath::wrap::std::sync::Mutex<Vec<(&'static str, Instant)>>,
    > = std::sync::LazyLock::new(|| hotpath::mutex!(Mutex::new(Vec::new()), label = "jobs-wake"));
    let mut last = LAST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match last.iter_mut().find(|(name, _)| *name == kind) {
        Some((_, at)) if now.saturating_duration_since(*at) < WAKE_INTERVAL => false,
        Some((_, at)) => {
            *at = now;
            true
        }
        None => {
            last.push((kind, now));
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

/// Validate and bind. A failure has sent nothing.
#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn prepare<'a, K: JobKind>(
    payload: &K,
    options: EnqueueOptions<'a>,
) -> Result<Prepared<'a>, EnqueueError> {
    if !kind::is_valid_kind_name(K::NAME) {
        return Err(EnqueueError::InvalidKind(K::NAME));
    }
    let unique_key = match options.unique_key {
        Some(key) if valid_unique_key(key) => Some(key),
        Some(_) => return Err(EnqueueError::InvalidUniqueKey),
        None => None,
    };
    let delay_micros =
        checked_delay_micros(options.delay).map_err(|_| EnqueueError::InvalidDelay)?;
    let payload = serde_json::to_string(payload).map_err(EnqueueError::Serialize)?;
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(EnqueueError::PayloadTooLarge {
            bytes: payload.len(),
        });
    }
    if contains_decoded_nul(payload.as_bytes()) {
        return Err(EnqueueError::PayloadContainsNul);
    }
    Ok(Prepared {
        payload,
        unique_key,
        delay_micros,
    })
}

fn valid_unique_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= MAX_UNIQUE_KEY_BYTES && !key.chars().any(char::is_control)
}

/// Returns the exact PostgreSQL microsecond delay, dropping sub-microseconds.
///
/// The queue accepts zero through [`MAX_DELAY`] inclusive.
pub(crate) fn checked_delay_micros(duration: Duration) -> Result<i64, InvalidDelay> {
    if duration > MAX_DELAY {
        return Err(InvalidDelay);
    }
    i64::try_from(duration.as_micros()).map_err(|_| InvalidDelay)
}

/// Whether serializer-produced JSON contains an escape that decodes to NUL.
///
/// A backslash always consumes its following escape byte. Thus the second
/// backslash in `\\\\u0000` is consumed and its following `u0000` remains literal.
fn contains_decoded_nul(payload: &[u8]) -> bool {
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
    use std::time::Duration;

    use super::{
        EnqueueError, EnqueueOptions, InvalidDelay, MAX_DELAY, MAX_PAYLOAD_BYTES,
        MAX_UNIQUE_KEY_BYTES, WAKE_INTERVAL, checked_delay_micros, prepare, wake_due,
    };
    use crate::JobKind;
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
        assert!(matches!(err, EnqueueError::InvalidKind("Bad Name")));
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

        assert!(matches!(
            prepare(
                &widget(),
                EnqueueOptions {
                    delay: Duration::ZERO,
                    unique_key: Some(""),
                },
            ),
            Err(EnqueueError::InvalidUniqueKey)
        ));
        assert!(matches!(
            prepare(
                &widget(),
                EnqueueOptions {
                    delay: Duration::ZERO,
                    unique_key: Some(&"k".repeat(MAX_UNIQUE_KEY_BYTES + 1)),
                },
            ),
            Err(EnqueueError::InvalidUniqueKey)
        ));
        let crossing = format!("{}é", "a".repeat(254));
        assert_eq!(crossing.len(), 256);
        assert!(matches!(
            prepare(
                &widget(),
                EnqueueOptions {
                    delay: Duration::ZERO,
                    unique_key: Some(&crossing),
                },
            ),
            Err(EnqueueError::InvalidUniqueKey)
        ));
    }

    #[test]
    fn unique_key_rejects_control_characters() {
        for key in ["\n", "\u{0}", "\u{7f}", "\u{85}"] {
            assert!(
                matches!(
                    prepare(
                        &widget(),
                        EnqueueOptions {
                            delay: Duration::ZERO,
                            unique_key: Some(key),
                        },
                    ),
                    Err(EnqueueError::InvalidUniqueKey)
                ),
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
        assert!(matches!(
            prepare(
                &widget(),
                EnqueueOptions {
                    delay: MAX_DELAY + Duration::from_nanos(1),
                    unique_key: None,
                },
            ),
            Err(EnqueueError::InvalidDelay)
        ));
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

        let too_big = Raw("a".repeat(MAX_PAYLOAD_BYTES - 1));
        let err = prepare(&too_big, EnqueueOptions::default()).unwrap_err();
        assert!(matches!(
            err,
            EnqueueError::PayloadTooLarge { bytes } if bytes == MAX_PAYLOAD_BYTES + 1
        ));
    }

    #[test]
    fn payload_rejects_decoded_nul_but_keeps_literal_backslash_u0000() {
        assert!(matches!(
            prepare(&Duplicate, EnqueueOptions::default()),
            Err(EnqueueError::PayloadContainsNul)
        ));
        assert!(matches!(
            prepare(&Raw("\0".to_owned()), EnqueueOptions::default()),
            Err(EnqueueError::PayloadContainsNul)
        ));
        assert!(matches!(
            prepare(
                &Keyed(BTreeMap::from([("\0".to_owned(), "value".to_owned())])),
                EnqueueOptions::default(),
            ),
            Err(EnqueueError::PayloadContainsNul)
        ));
        assert!(prepare(&Raw(r"\u0000".to_owned()), EnqueueOptions::default()).is_ok());
    }

    #[test]
    fn wake_is_debounced_per_kind() {
        let now = tokio::time::Instant::now();
        assert!(wake_due("wake.first", now));
        assert!(!wake_due("wake.first", now + WAKE_INTERVAL / 2));
        // Another kind's worker is not woken by the first kind's notification.
        assert!(wake_due("wake.second", now + WAKE_INTERVAL / 2));
        assert!(wake_due("wake.first", now + WAKE_INTERVAL));
    }

    #[test]
    fn serialize_failure() {
        let err = prepare(&Refuse, EnqueueOptions::default()).unwrap_err();
        assert!(matches!(err, EnqueueError::Serialize(_)));
    }
}
