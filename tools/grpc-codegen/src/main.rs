//! Generates committed Rust bindings from Buf's descriptor set.
//!
//! This tool calls prost_build::Config::compile_fds directly. Tonic's
//! compile_fds_with_config helper replaces a configured service generator,
//! which would bypass the composite policy generator.

mod policy;

use std::{
    env,
    error::Error,
    fs,
    path::{Path, PathBuf},
};

use prost::Message;
use prost_types::FileDescriptorSet;

const OWNED_PROTO_PREFIX: &str = "example/";

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
    let owned_descriptor_set = FileDescriptorSet {
        file: descriptor_set
            .file
            .into_iter()
            .filter(|file| file.name().starts_with(OWNED_PROTO_PREFIX))
            .collect(),
    };

    if owned_descriptor_set.file.is_empty() {
        return Err(format!(
            "descriptor set contains no owned protobuf files under {OWNED_PROTO_PREFIX:?}"
        )
        .into());
    }

    let mut config = prost_build::Config::new();
    config.out_dir(output_dir);
    policy::configure(&mut config, &owned_descriptor_set);
    config.compile_fds(owned_descriptor_set)?;
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
