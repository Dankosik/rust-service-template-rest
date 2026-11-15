pub mod codec;

pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!("generated/file_descriptor_set.binpb");

// Keep generated-code lint exceptions off the handwritten codec. Re-export
// packages at the crate root so callers still use their protobuf package paths.
#[allow(
    missing_docs,
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
