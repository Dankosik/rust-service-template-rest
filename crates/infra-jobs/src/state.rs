use std::fmt;

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

    #[must_use]
    pub(crate) const fn is_live(self) -> bool {
        match self {
            Self::Pending | Self::Running => true,
            Self::Completed | Self::Failed => false,
        }
    }
}

impl fmt::Display for JobState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AsRef<str> for JobState {
    fn as_ref(&self) -> &str {
        self.as_str()
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

impl fmt::Display for FailureReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl AsRef<str> for FailureReason {
    fn as_ref(&self) -> &str {
        self.as_str()
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
