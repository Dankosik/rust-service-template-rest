use std::borrow::Cow;

#[must_use]
pub fn sqlstate(err: &sqlx::Error) -> Option<Cow<'_, str>> {
    err.as_database_error()?.code().filter(|code| {
        code.len() == 5
            && code
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    })
}

const CLASS_CONNECTION_EXCEPTION: &str = "08";
const CLASS_INSUFFICIENT_RESOURCES: &str = "53";
const SERIALIZATION_FAILURE: &str = "40001";
pub(crate) const STATEMENT_COMPLETION_UNKNOWN: &str = "40003";
const DEADLOCK_DETECTED: &str = "40P01";
const ADMIN_SHUTDOWN: &str = "57P01";
const CRASH_SHUTDOWN: &str = "57P02";
const CANNOT_CONNECT_NOW: &str = "57P03";
pub(crate) const QUERY_CANCELED: &str = "57014";
const LOCK_NOT_AVAILABLE: &str = "55P03";
const IDLE_IN_TRANSACTION_SESSION_TIMEOUT: &str = "25P03";
const TRANSACTION_TIMEOUT: &str = "25P04";
const READ_ONLY_SQL_TRANSACTION: &str = "25006";

#[must_use]
pub fn is_transient(err: &sqlx::Error) -> bool {
    match sqlstate(err) {
        Some(code) => {
            code.starts_with(CLASS_CONNECTION_EXCEPTION)
                || code.starts_with(CLASS_INSUFFICIENT_RESOURCES)
                || matches!(
                    code.as_ref(),
                    SERIALIZATION_FAILURE
                        | STATEMENT_COMPLETION_UNKNOWN
                        | DEADLOCK_DETECTED
                        | ADMIN_SHUTDOWN
                        | CRASH_SHUTDOWN
                        | CANNOT_CONNECT_NOW
                        | QUERY_CANCELED
                        | LOCK_NOT_AVAILABLE
                        | IDLE_IN_TRANSACTION_SESSION_TIMEOUT
                        | TRANSACTION_TIMEOUT
                        | READ_ONLY_SQL_TRANSACTION
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
