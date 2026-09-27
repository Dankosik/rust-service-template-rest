//! Committed native gRPC contract generated from the owned protobuf schema.

#![forbid(unsafe_code)]

use std::sync::LazyLock;

/// Import-complete descriptor set generated alongside the Rust contract.
pub const DESCRIPTOR_BYTES: &[u8] = include_bytes!("descriptor.bin");

/// The immutable descriptor pool used by generated reflection and registration.
#[allow(
    clippy::expect_used,
    reason = "the descriptor is committed generator output, validated by the schema drift gate"
)]
pub static DESCRIPTOR_POOL: LazyLock<prost_reflect::DescriptorPool> = LazyLock::new(|| {
    prost_reflect::DescriptorPool::decode(DESCRIPTOR_BYTES)
        .expect("committed gRPC descriptor set must be valid")
});

/// Generated protobuf messages and native tonic contracts.
#[allow(
    clippy::default_trait_access,
    clippy::doc_markdown,
    clippy::missing_errors_doc,
    clippy::too_many_lines,
    clippy::unreadable_literal,
    reason = "prost and tonic generator output is checked for drift and is not manually edited"
)]
pub mod generated {
    include!("generated/example.v1.rs");
}
