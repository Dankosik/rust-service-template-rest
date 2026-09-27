//! PostgreSQL error classification shared by provider consumers.

use std::borrow::Cow;

/// The SQLSTATE of a database error, when the driver supplied a valid one.
///
/// Only the PostgreSQL five-byte uppercase-alphanumeric format is returned,
/// so the value is safe as a structured log field or a metric label.
#[must_use]
pub fn sqlstate(err: &sqlx::Error) -> Option<Cow<'_, str>> {
    err.as_database_error()?.code().filter(|code| {
        code.len() == 5
            && code
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    })
}

/// Whether the same transaction work could succeed if rerun by its caller.
#[must_use]
pub fn retryable(err: &sqlx::Error) -> bool {
    sqlstate(err).is_some_and(|code| code == "40001" || code == "40P01")
}

/// Whether `err` reports a transient condition that a later attempt may not
/// meet. With a valid SQLSTATE: a lost connection (class `08`), exhausted
/// resources (class `53`), a serialization failure, deadlock, or uncertain
/// statement completion (`40001`, `40P01`, `40003`), a server shutting down or
/// starting (`57P01`, `57P02`, `57P03`), a cancelled statement (`57014`), or a
/// read-only session (`25006`). Without one: a pool that timed out or closed,
/// or a failed socket or TLS session.
#[must_use]
pub fn transient(err: &sqlx::Error) -> bool {
    match sqlstate(err) {
        Some(code) => {
            code.starts_with("08")
                || code.starts_with("53")
                || matches!(
                    code.as_ref(),
                    "40001" | "40003" | "40P01" | "57P01" | "57P02" | "57P03" | "57014" | "25006"
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

/// A bounded class for a driver error that has no valid SQLSTATE.
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
    fn policies_keep_their_distinct_sqlstate_sets() {
        assert!(retryable(&database("40001")));
        assert!(retryable(&database("40P01")));
        assert!(!retryable(&database("40003")));
        assert!(transient(&database("08006")));
        assert!(transient(&database("57014")));
        assert!(transient(&sqlx::Error::PoolTimedOut));
        assert!(!transient(&database("25P02")));
        assert!(!transient(&sqlx::Error::Protocol("driver misuse".into())));
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

    #[derive(Debug)]
    struct TestDatabaseError {
        code: &'static str,
    }

    impl std::fmt::Display for TestDatabaseError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("database")
        }
    }

    impl std::error::Error for TestDatabaseError {}

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
