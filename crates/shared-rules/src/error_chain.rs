//! An error's chain of causes: as one line of text, and as the errors a
//! caller can test by type.

use std::error::Error;
use std::fmt;

/// `error`, then each error it was caused by, for a caller that looks for a
/// cause of a given type.
///
/// A [`std::io::Error`] that wraps a custom error returns that error's source
/// from `source()`, skipping the wrapped error itself; a TLS connector hands
/// over a `rustls::Error` that way. This walk yields that payload before its sources.
/// [`DisplayErrorChain`] keeps the plain `source()` chain, because an I/O error already
/// prints its payload's text.
pub fn causes<'a>(
    error: &'a (dyn Error + 'static),
) -> impl Iterator<Item = &'a (dyn Error + 'static)> {
    std::iter::successors(Some(error), |&error| {
        match error
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::get_ref)
        {
            Some(payload) => Some(payload),
            None => error.source(),
        }
    })
}

/// Displays an error followed by each of its sources, separated by `: `.
///
/// `std::error::Report` renders the same line once it is stable.
#[derive(Clone, Copy, Debug)]
pub struct DisplayErrorChain<'a>(pub &'a (dyn Error + 'static));

impl fmt::Display for DisplayErrorChain<'_> {
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
    use super::{DisplayErrorChain, causes};
    use std::error::Error;
    use std::fmt;
    use std::io;

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
            DisplayErrorChain(&error).to_string(),
            "request failed: connection reset: broken pipe"
        );
    }

    #[test]
    fn an_error_without_sources_is_its_own_message() {
        let error = Layer {
            message: "timed out",
            source: None,
        };
        assert_eq!(DisplayErrorChain(&error).to_string(), "timed out");
    }

    #[test]
    fn causes_start_at_the_error_and_follow_each_source() {
        let error = Layer {
            message: "request failed",
            source: Some(Box::new(Layer {
                message: "connection reset",
                source: None,
            })),
        };
        let messages: Vec<String> = causes(&error).map(ToString::to_string).collect();
        assert_eq!(messages, ["request failed", "connection reset"]);
    }

    #[test]
    fn causes_reach_the_payload_an_io_error_hides_from_source() {
        let payload = Layer {
            message: "certificate expired",
            source: Some(Box::new(Layer {
                message: "clock",
                source: None,
            })),
        };
        let io = io::Error::other(payload);
        assert!(
            io.source()
                .is_some_and(|source| source.to_string() == "clock"),
            "std skips the payload, which is why the walk steps into it"
        );
        let messages: Vec<String> = causes(&io).map(ToString::to_string).collect();
        // The I/O error prints its payload's text, then the payload follows.
        assert_eq!(
            messages,
            ["certificate expired", "certificate expired", "clock"]
        );
        assert!(causes(&io).any(<dyn Error>::is::<Layer>));
    }
}
