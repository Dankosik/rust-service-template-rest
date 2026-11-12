//! An error and its sources as one line of text.

use std::error::Error;
use std::fmt;

/// Displays an error followed by each of its sources, separated by `: `.
///
/// `std::error::Report` renders the same line once it is stable.
#[derive(Clone, Copy, Debug)]
pub struct Causes<'a>(pub &'a (dyn Error + 'static));

impl fmt::Display for Causes<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)?;
        for cause in std::iter::successors(self.0.source(), |cause| (*cause).source()) {
            write!(formatter, ": {cause}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Causes;
    use std::error::Error;
    use std::fmt;

    #[derive(Debug)]
    struct Layer {
        message: &'static str,
        source: Option<Box<Layer>>,
    }

    impl fmt::Display for Layer {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.message)
        }
    }

    impl Error for Layer {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            self.source.as_deref().map(|source| source as _)
        }
    }

    #[test]
    fn every_source_follows_the_error_in_order() {
        let error = Layer {
            message: "request failed",
            source: Some(Box::new(Layer {
                message: "connection reset",
                source: Some(Box::new(Layer {
                    message: "broken pipe",
                    source: None,
                })),
            })),
        };
        assert_eq!(
            Causes(&error).to_string(),
            "request failed: connection reset: broken pipe"
        );
    }

    #[test]
    fn an_error_without_sources_is_its_own_message() {
        let error = Layer {
            message: "timed out",
            source: None,
        };
        assert_eq!(Causes(&error).to_string(), "timed out");
    }
}
