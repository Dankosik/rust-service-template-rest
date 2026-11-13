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
    const ALL: [Self; 4] = [Self::Pending, Self::Running, Self::Completed, Self::Failed];

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
        Self::ALL.into_iter().find(|state| state.as_str() == value)
    }

    /// Whether the row is live, as the migration defines it: pending or running.
    #[must_use]
    pub(crate) const fn is_live(self) -> bool {
        match self {
            Self::Pending | Self::Running => true,
            Self::Completed | Self::Failed => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureReason {
    Permanent,
    Exhausted,
}

impl FailureReason {
    const ALL: [Self; 2] = [Self::Permanent, Self::Exhausted];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Permanent => "permanent",
            Self::Exhausted => "exhausted",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|reason| reason.as_str() == value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_state_names_one_stored_string() {
        assert_eq!(
            JobState::ALL.map(JobState::as_str),
            ["pending", "running", "completed", "failed"]
        );
        for state in JobState::ALL {
            assert_eq!(JobState::parse(state.as_str()), Some(state));
        }
        assert_eq!(JobState::parse("unknown"), None);
    }

    #[test]
    fn every_failure_reason_names_one_stored_string() {
        assert_eq!(
            FailureReason::ALL.map(FailureReason::as_str),
            ["permanent", "exhausted"]
        );
        for reason in FailureReason::ALL {
            assert_eq!(FailureReason::parse(reason.as_str()), Some(reason));
        }
        assert_eq!(FailureReason::parse("unknown"), None);
    }

    #[test]
    fn only_pending_and_running_are_live() {
        for state in JobState::ALL {
            assert_eq!(
                state.is_live(),
                matches!(state, JobState::Pending | JobState::Running),
                "{state:?}"
            );
        }
    }
}
