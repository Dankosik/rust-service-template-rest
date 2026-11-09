//! The stored `state` and `failure_reason` values of a job row.
//!
//! The migration's CHECK constraints and the SQL literals in the statements
//! name the same strings.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobState {
    Pending,
    Running,
    Completed,
    Failed,
}

impl JobState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        [Self::Pending, Self::Running, Self::Completed, Self::Failed]
            .into_iter()
            .find(|state| state.as_str() == value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureReason {
    Permanent,
    Exhausted,
}

impl FailureReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Permanent => "permanent",
            Self::Exhausted => "exhausted",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        [Self::Permanent, Self::Exhausted]
            .into_iter()
            .find(|reason| reason.as_str() == value)
    }
}
