use std::fmt;
use std::str::FromStr;

const WORKFLOW_ID_MAX_BYTES: usize = 1000;
const SHORT_ID_MAX_BYTES: usize = 255;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum IdentifierError {
    #[error("identifier must not be blank")]
    Empty,
    #[error("identifier exceeds maximum length")]
    TooLong,
    #[error("identifier contains invalid whitespace or control characters")]
    InvalidCharacters,
}

fn validate_identifier(value: &str, max_bytes: usize) -> Result<(), IdentifierError> {
    if value.trim().is_empty() {
        return Err(IdentifierError::Empty);
    }
    if value.len() > max_bytes {
        return Err(IdentifierError::TooLong);
    }
    if value.trim() != value || value.chars().any(char::is_control) {
        return Err(IdentifierError::InvalidCharacters);
    }
    Ok(())
}

macro_rules! validated_id {
    ($name:ident, $max_bytes:expr) => {
        #[derive(Clone, Debug, PartialEq, Eq, Hash)]
        pub struct $name(String);

        impl $name {
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            #[must_use]
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = IdentifierError;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                validate_identifier(&value, $max_bytes)?;
                Ok(Self(value))
            }
        }

        impl TryFrom<&str> for $name {
            type Error = IdentifierError;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                validate_identifier(value, $max_bytes)?;
                Ok(Self(value.to_owned()))
            }
        }

        impl FromStr for $name {
            type Err = IdentifierError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::try_from(value)
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

validated_id!(WorkflowId, WORKFLOW_ID_MAX_BYTES);
validated_id!(RunId, SHORT_ID_MAX_BYTES);
validated_id!(UpdateId, SHORT_ID_MAX_BYTES);
validated_id!(NonBlankName, SHORT_ID_MAX_BYTES);
validated_id!(RequestKey, SHORT_ID_MAX_BYTES);
