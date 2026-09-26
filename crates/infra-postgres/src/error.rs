//! PostgreSQL error classification shared by provider consumers.

use std::borrow::Cow;

/// Extract the driver's unmodified SQLSTATE, when it supplied one.
#[must_use]
pub fn raw_sqlstate(err: &sqlx::Error) -> Option<Cow<'_, str>> {
    err.as_database_error()?.code()
}

/// Extract a SQLSTATE safe to expose in structured diagnostics.
///
/// Driver-provided text remains a raw classification input for transaction
/// policy, but logs and consumer mappings accept only the PostgreSQL five-byte
/// uppercase-alphanumeric code format.
#[must_use]
pub fn sqlstate(err: &sqlx::Error) -> Option<Cow<'_, str>> {
    raw_sqlstate(err).filter(|code| {
        code.len() == 5
            && code
                .bytes()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
    })
}

/// Whether the same transaction work could succeed if rerun by its caller.
#[must_use]
pub fn retryable(err: &sqlx::Error) -> bool {
    raw_sqlstate(err).is_some_and(|code| code == "40001" || code == "40P01")
}

/// Whether PostgreSQL definitely rejected a transaction commit.
#[must_use]
pub fn commit_definitely_failed(code: &str) -> bool {
    code.starts_with("23") || (code.starts_with("40") && code != "40003")
}

/// Whether the idempotency store treats a database code as unavailable.
#[must_use]
pub fn idempotency_transient(code: &str) -> bool {
    code.starts_with("08")
        || code.starts_with("53")
        || matches!(
            code,
            "40001" | "40003" | "40P01" | "57P01" | "57P02" | "57P03" | "57014" | "25006"
        )
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
mod tests {
    use super::*;

    #[test]
    fn policies_keep_their_distinct_sqlstate_sets() {
        assert!(retryable(&database("40001")));
        assert!(retryable(&database("40P01")));
        assert!(!retryable(&database("40003")));
        assert!(commit_definitely_failed("23505"));
        assert!(commit_definitely_failed("40P01"));
        assert!(!commit_definitely_failed("40003"));
        assert!(idempotency_transient("08006"));
        assert!(idempotency_transient("57014"));
        assert!(!idempotency_transient("25P02"));
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

    fn database(code: &'static str) -> sqlx::Error {
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
