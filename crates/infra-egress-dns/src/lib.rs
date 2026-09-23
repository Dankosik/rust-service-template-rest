//! DNS resolution that admits only public connection addresses.
//!
//! Consumers provide process-owned task tracking and cancellation. This crate
//! never owns HTTP policy, credentials, provider parsing, or bootstrap wiring.

mod address;
mod runtime;

use std::{error::Error, fmt};

pub use address::admit_address;
pub use runtime::PublicAddressResolver;

/// Content-free DNS outcomes for consumers that need typed error mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResolveError {
    /// System resolver configuration could not be read or applied.
    Configuration,
    /// DNS lookup could not produce an answer set.
    Lookup,
    /// The answer set was empty or included a non-public address.
    Denied,
    /// The owning lookup or runtime was cancelled.
    Cancelled,
}

impl fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Configuration => "egress DNS configuration failed",
            Self::Lookup => "egress DNS lookup failed",
            Self::Denied => "egress DNS address denied",
            Self::Cancelled => "egress DNS cancelled",
        };
        formatter.write_str(label)
    }
}

impl Error for ResolveError {}
