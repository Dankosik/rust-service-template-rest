//! Rules that several crates must apply identically and that have no other
//! common owner: optional profile crates cannot depend on each other or on
//! the configuration loader.
//!
//! A rule belongs here only when all of these hold:
//!
//! - It is one piece of knowledge: changing it in one caller would have to
//!   change it in every caller. A standard (an RFC, the OpenTelemetry
//!   conventions), the way platforms write mounted secret files, the
//!   standard library, or one formula fixes it.
//! - At least two crates apply it and share no owner that could hold it. A
//!   rule from an external standard joins with its second caller; a formula
//!   of the service's own joins with its third.
//! - It is pure: no I/O, clock, randomness, global state, or configuration.
//! - It needs nothing beyond `std`; this crate declares no dependencies.
//! - It returns plain data: values, or a closed set of outcomes. Each caller
//!   keeps its own policy (windows, random sources, error codes and wording)
//!   and maps the result into its own error.
//!
//! Code that only looks alike and changes for independent reasons stays with
//! each caller. A function that gains a parameter or flag for one caller goes
//! back to its callers. A module whose remaining consumers all belong to one
//! template profile carries that profile's marker, so it leaves with them.

pub mod error_chain;
pub mod jitter;
pub mod otel_http;
pub mod rfc6749;
pub mod rfc6750;
pub mod rfc7515;
pub mod rfc7519;
pub mod secret_file;
// template:begin outbound-auth:shared-rules-uri
pub mod uri;
// template:end outbound-auth:shared-rules-uri
