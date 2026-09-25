//! The one insert path. It runs on the caller's open transaction and never
//! opens, commits, or rolls one back.

use std::time::Duration;

use infra_postgres::{Tx, connection};
use sqlx::Row;

use crate::kind::{self, JobId, JobKind};
use crate::traceparent;

/// The longest JSON payload enqueue accepts: 256 KiB.
pub const MAX_PAYLOAD_BYTES: usize = 262_144;
/// The longest unique key enqueue accepts.
pub const MAX_UNIQUE_KEY_BYTES: usize = 255;
/// The longest delay enqueue accepts: 36500 days.
pub const MAX_DELAY: Duration = Duration::from_hours(36_500 * 24);

/// One insert on the caller's connection. A conflict with a live unique key
/// returns no row.
const ENQUEUE: &str = "INSERT INTO background_jobs (kind, payload, unique_key, not_before, trace_context) \
     VALUES ($1, $2, $3, statement_timestamp() + $4, $5) \
     ON CONFLICT (kind, unique_key) WHERE unique_key IS NOT NULL AND state IN ('pending', 'running') \
     DO NOTHING \
     RETURNING id::text AS id";

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
    /// Classify with [`infra_postgres::retryable`].
    #[error("the enqueue statement failed")]
    Database(#[source] sqlx::Error),
}

/// Write one job inside the caller's open transaction.
///
/// Validation failures send nothing and leave the transaction usable. A
/// [`EnqueueError::Database`] error means the caller's transaction is aborted;
/// the caller classifies it with [`infra_postgres::retryable`]. With a unique
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
pub async fn enqueue<K: JobKind>(
    tx: &mut Tx<'_>,
    payload: &K,
    options: EnqueueOptions<'_>,
) -> Result<Enqueued, EnqueueError> {
    let prepared = prepare(payload, options)?;
    let traceparent = traceparent::current();
    let row = sqlx::query(ENQUEUE)
        .bind(K::NAME)
        .bind(prepared.payload.as_slice())
        .bind(prepared.unique_key)
        .bind(prepared.delay)
        .bind(traceparent.as_deref())
        .fetch_optional(&mut *connection(tx))
        .await
        .map_err(EnqueueError::Database)?;
    let Some(row) = row else {
        return Ok(Enqueued::Duplicate);
    };
    let id: String = row.try_get("id").map_err(EnqueueError::Database)?;
    let Some(id) = JobId::parse(&id) else {
        return Err(EnqueueError::Database(sqlx::Error::Decode(
            "job id is not a uuid".into(),
        )));
    };
    Ok(Enqueued::Created(id))
}

#[derive(Debug)]
struct Prepared<'a> {
    payload: Vec<u8>,
    unique_key: Option<&'a [u8]>,
    delay: Duration,
}

/// Validate and bind. A failure has sent nothing.
fn prepare<'a, K: JobKind>(
    payload: &K,
    options: EnqueueOptions<'a>,
) -> Result<Prepared<'a>, EnqueueError> {
    if !kind::is_valid_kind_name(K::NAME) {
        return Err(EnqueueError::InvalidKind(K::NAME));
    }
    let unique_key = match options.unique_key {
        Some(key) if valid_unique_key(key) => Some(key.as_bytes()),
        Some(_) => return Err(EnqueueError::InvalidUniqueKey),
        None => None,
    };
    if options.delay > MAX_DELAY {
        return Err(EnqueueError::InvalidDelay);
    }
    let delay = whole_micros(options.delay);
    let payload = serde_json::to_vec(payload).map_err(EnqueueError::Serialize)?;
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(EnqueueError::PayloadTooLarge {
            bytes: payload.len(),
        });
    }
    Ok(Prepared {
        payload,
        unique_key,
        delay,
    })
}

fn valid_unique_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= MAX_UNIQUE_KEY_BYTES && !key.chars().any(char::is_control)
}

/// `duration` without its sub-microsecond part. `sqlx` refuses to bind a
/// `Duration` with one as an `interval`.
fn whole_micros(duration: Duration) -> Duration {
    Duration::new(duration.as_secs(), duration.subsec_micros() * 1_000)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde::ser::Error as _;
    use serde::{Deserialize, Serialize};
    use sqlx::postgres::types::PgInterval;

    use super::{
        EnqueueError, EnqueueOptions, MAX_DELAY, MAX_PAYLOAD_BYTES, MAX_UNIQUE_KEY_BYTES, prepare,
    };
    use crate::JobKind;

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
    struct BadKind;

    impl JobKind for BadKind {
        const NAME: &'static str = "Bad Name";
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
        assert_eq!(prepared.unique_key, Some(accepted.as_bytes()));

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
    fn delay_drops_sub_microsecond_and_binds() {
        let prepared = prepare(
            &widget(),
            EnqueueOptions {
                delay: Duration::new(1, 999_999_999),
                unique_key: None,
            },
        )
        .unwrap();
        assert_eq!(prepared.delay, Duration::new(1, 999_999_000));
        assert!(PgInterval::try_from(prepared.delay).is_ok());
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
    fn serialize_failure() {
        let err = prepare(&Refuse, EnqueueOptions::default()).unwrap_err();
        assert!(matches!(err, EnqueueError::Serialize(_)));
    }
}
