//! Committed native gRPC contract generated from the owned protobuf schema.

#![forbid(unsafe_code)]

use std::sync::LazyLock;

/// Import-complete descriptor set generated alongside the Rust contract.
pub const DESCRIPTOR_BYTES: &[u8] = include_bytes!("descriptor.bin");

/// The immutable descriptor pool used by generated reflection and registration.
pub static DESCRIPTOR_POOL: LazyLock<prost_reflect::DescriptorPool> = LazyLock::new(|| {
    prost_reflect::DescriptorPool::decode(DESCRIPTOR_BYTES)
        .expect("committed gRPC descriptor set must be valid")
});

/// Generated protobuf messages and native tonic contracts.
pub mod generated {
    include!("generated/example.v1.rs");
}
