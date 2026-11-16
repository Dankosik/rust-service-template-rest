use std::borrow::Cow;

/// Only the five-byte uppercase-alphanumeric format, so the value is safe as a
/// log field or metric label.
#[must_use]
pub fn sqlstate(err: &sqlx::Error) -> Option<Cow<'_, str>> {
    err.as_database_error()?.code().filter(|code| {
        code.len() == 5
            && code
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    })
}

/// Transient: with a SQLSTATE, classes `08` and `53`, `40001`, `40P01`, `40003`,
/// `57P01`-`57P03`, `57014`, `55P03`, `25P03`, `25P04`, `25006`; without one, a
/// timed-out or closed pool and failed socket or TLS sessions.
#[must_use]
pub fn is_transient(err: &sqlx::Error) -> bool {
    match sqlstate(err) {
        Some(code) => {
            code.starts_with("08")
                || code.starts_with("53")
                || matches!(
                    code.as_ref(),
                    "40001"
                        | "40003"
                        | "40P01"
                        | "57P01"
                        | "57P02"
                        | "57P03"
                        | "57014"
                        | "55P03"
                        | "25P03"
                        | "25P04"
                        | "25006"
                )
        }
        None => matches!(
            err,
            sqlx::Error::PoolTimedOut
                | sqlx::Error::PoolClosed
                | sqlx::Error::Io(_)
                | sqlx::Error::Tls(_)
        ),
    }
}

/// Pair with [`sqlstate`]; `database` covers any server-reported error.
#[must_use]
pub const fn failure_cause(err: &sqlx::Error) -> &'static str {
    match err {
        sqlx::Error::Database(_) => "database",
        sqlx::Error::PoolTimedOut => "pool_timeout",
        sqlx::Error::PoolClosed => "pool_closed",
        sqlx::Error::Io(_) => "io",
        sqlx::Error::Tls(_) => "tls",
        sqlx::Error::Protocol(_) => "protocol",
        sqlx::Error::ColumnDecode { .. } | sqlx::Error::Decode(_) => "decode",
        sqlx::Error::WorkerCrashed => "worker_crashed",
        _ => "driver",
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn transient_covers_the_conditions_a_later_attempt_may_not_meet() {
        assert!(is_transient(&database("40001")));
        assert!(is_transient(&database("40P01")));
        assert!(is_transient(&database("40003")));
        assert!(is_transient(&database("08006")));
        assert!(is_transient(&database("57014")));
        assert!(is_transient(&database("25P03")));
        assert!(is_transient(&sqlx::Error::PoolTimedOut));
        assert!(!is_transient(&database("25P02")));
        assert!(!is_transient(&sqlx::Error::Protocol(
            "driver misuse".into()
        )));
    }

    #[test]
    fn diagnostics_expose_only_validated_codes() {
        assert_eq!(sqlstate(&database("23505")).as_deref(), Some("23505"));
        assert_eq!(sqlstate(&database("invalid")).as_deref(), None);
        assert_eq!(
            failure_cause(&sqlx::Error::Protocol("private".into())),
            "protocol"
        );
    }

    pub(crate) fn database(code: &'static str) -> sqlx::Error {
        sqlx::Error::Database(Box::new(TestDatabaseError { code }))
    }

    #[derive(Debug, thiserror::Error)]
    #[error("database")]
    struct TestDatabaseError {
        code: &'static str,
    }

    impl sqlx::error::DatabaseError for TestDatabaseError {
        fn message(&self) -> &'static str {
            "database"
        }
        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.code))
        }
        fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
            self
        }
        fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
            self
        }
        fn kind(&self) -> sqlx::error::ErrorKind {
            sqlx::error::ErrorKind::Other
        }
    }
}
