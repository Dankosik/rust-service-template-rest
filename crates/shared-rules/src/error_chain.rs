//! An error's chain of causes: as one line of text, and as the errors a
//! caller can test by type.

use std::error::Error;
use std::fmt;

const MAX_CHAIN_ENTRIES: usize = 64;

/// Up to 64 entries, starting with `error`, for a caller looking for a cause
/// of a given type.
///
/// A [`std::io::Error`] that wraps a custom error returns that error's source
/// from `source()`, skipping the wrapped error itself; a TLS connector hands
/// over a `rustls::Error` that way. This walk yields that payload before its sources.
/// [`DisplayErrorChain`] keeps the plain `source()` chain, because an I/O error already
/// prints its payload's text.
pub fn causes<'a>(
    error: &'a (dyn Error + 'static),
) -> impl Iterator<Item = &'a (dyn Error + 'static)> {
    bounded_chain(error, |error| {
        match error
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::get_ref)
        {
            Some(payload) => Some(payload),
            None => error.source(),
        }
    })
}

/// Up to 63 sources of `error`, following [`Error::source`].
pub fn sources<'a>(
    error: &'a (dyn Error + 'static),
) -> impl Iterator<Item = &'a (dyn Error + 'static)> {
    bounded_chain(error, |error| error.source()).skip(1)
}

fn bounded_chain<'a>(
    error: &'a (dyn Error + 'static),
    mut next: impl FnMut(&'a (dyn Error + 'static)) -> Option<&'a (dyn Error + 'static)>,
) -> impl Iterator<Item = &'a (dyn Error + 'static)> {
    let mut current = Some(error);
    std::iter::once(error)
        .chain(std::iter::from_fn(move || {
            current = current.and_then(&mut next);
            current
        }))
        .take(MAX_CHAIN_ENTRIES)
}

/// Displays up to 64 entries starting with the error, separated by `: `.
#[derive(Clone, Copy, Debug)]
pub struct DisplayErrorChain<'a>(pub &'a (dyn Error + 'static));

impl fmt::Display for DisplayErrorChain<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)?;
        for cause in sources(self.0) {
            write!(formatter, ": {cause}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{DisplayErrorChain, causes};
    use std::cell::Cell;
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

    #[derive(Debug)]
    struct CyclicError(Cell<usize>);

    impl fmt::Display for CyclicError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("cyclic error")
        }
    }

    impl Error for CyclicError {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            let calls = self.0.get();
            assert!(calls < 128, "walking the cycle did not stop");
            self.0.set(calls + 1);
            Some(self)
        }
    }

    #[test]
    fn lookup_and_display_stop_on_a_cyclic_source() {
        let error = CyclicError(Cell::new(0));
        assert!(!causes(&error).any(<dyn Error>::is::<Layer>));

        let error = CyclicError(Cell::new(0));
        assert!(
            DisplayErrorChain(&error)
                .to_string()
                .starts_with("cyclic error")
        );
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
