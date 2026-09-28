//! Generates committed Rust bindings from Buf's descriptor set.
//!
//! Buf owns schema compilation and builds the set with `--exclude-imports`, so
//! it holds only the owned files. This tool runs stock tonic codegen over it
//! and does not invoke protoc.

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use prost::Message;
use prost_types::FileDescriptorSet;

fn main() -> Result<(), Box<dyn Error>> {
    let mut arguments = env::args_os();
    let program = arguments.next().unwrap_or_default();
    let descriptor_path = required_path(&mut arguments, "descriptor-set path")?;
    let output_dir = required_path(&mut arguments, "output directory")?;

    if arguments.next().is_some() {
        return Err(format!(
            "usage: {} <descriptor-set> <output-directory>",
            Path::new(&program).display()
        )
        .into());
    }

    fs::create_dir_all(&output_dir)?;
    let descriptor_set = FileDescriptorSet::decode(fs::read(descriptor_path)?.as_slice())?;

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .build_transport(false)
        // Stock prost codec with smaller per-call buffers; see `grpc_contracts::codec`.
        .codec_path("crate::codec::ContractCodec")
        .emit_rerun_if_changed(false)
        // Nests each package's file in its module path: `example.v1` becomes `example::v1`.
        .include_file("_includes.rs")
        .out_dir(&output_dir)
        .compile_fds(descriptor_set)?;
    Ok(())
}

fn required_path(
    arguments: &mut impl Iterator<Item = std::ffi::OsString>,
    name: &str,
) -> Result<PathBuf, Box<dyn Error>> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("missing {name}").into())
}
