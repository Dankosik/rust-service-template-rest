//! Committed native gRPC contracts generated from `api/proto` by
//! `make grpc-generate`. Modules mirror protobuf packages, so `example.v1` is
//! `example::v1`.
//!
//! Change schemas in `api/proto`, regenerate with `make grpc-generate`, and
//! keep schema and generated Rust together. `make grpc-check` verifies drift
//! and compatibility. Normal service builds use the committed Rust.
//! Handlers own input validation; `infra-grpc` owns listener and middleware policy.

pub mod codec;

/// The encoded `FileDescriptorSet` the Rust was generated from, with its
/// imports, for `infra_grpc::Services::add_reflection`.
pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!("generated/file_descriptor_set.binpb");

// Keep generated-code lint exceptions off the handwritten codec. Re-export
// packages at the crate root so callers still use their protobuf package paths.
#[allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    rustdoc::all,
    reason = "prost and tonic output is regenerated, checked for drift and never edited by hand"
)]
#[path = "generated/_includes.rs"]
mod generated;

pub use generated::*;
