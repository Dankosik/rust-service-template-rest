pub mod codec;

pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!("generated/file_descriptor_set.binpb");

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
pub use prost_types;
