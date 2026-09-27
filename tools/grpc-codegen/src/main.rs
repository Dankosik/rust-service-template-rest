//! Generates committed Rust bindings from Buf's descriptor set.
//!
//! Stock tonic codegen runs over the filtered owned descriptor set. Buf owns
//! schema compilation; this tool does not invoke protoc.

use std::{
    collections::BTreeSet,
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
    let source_root = required_path(&mut arguments, "owned protobuf source root")?;

    if arguments.next().is_some() {
        return Err(format!(
            "usage: {} <descriptor-set> <output-directory> <owned-protobuf-source-root>",
            Path::new(&program).display()
        )
        .into());
    }

    fs::create_dir_all(&output_dir)?;
    let descriptor_set = FileDescriptorSet::decode(fs::read(descriptor_path)?.as_slice())?;
    let owned_proto_names = owned_proto_names(&source_root)?;
    let owned_descriptor_set = FileDescriptorSet {
        file: descriptor_set
            .file
            .into_iter()
            .filter(|file| owned_proto_names.contains(file.name()))
            .collect(),
    };

    let generated_names = owned_descriptor_set
        .file
        .iter()
        .map(|file| file.name())
        .collect::<BTreeSet<_>>();
    let missing = owned_proto_names
        .iter()
        .filter(|name| !generated_names.contains(name.as_str()))
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(format!(
            "descriptor set omits owned protobuf source files: {}",
            missing
                .iter()
                .map(|name| name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
        .into());
    }

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .build_transport(false)
        .emit_rerun_if_changed(false)
        .out_dir(&output_dir)
        .compile_fds(owned_descriptor_set)?;
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

fn owned_proto_names(source_root: &Path) -> Result<BTreeSet<String>, Box<dyn Error>> {
    let mut names = BTreeSet::new();
    visit_proto_files(source_root, source_root, &mut names)?;
    if names.is_empty() {
        return Err(format!(
            "owned protobuf source root contains no .proto files: {}",
            source_root.display()
        )
        .into());
    }
    Ok(names)
}

fn visit_proto_files(
    source_root: &Path,
    directory: &Path,
    names: &mut BTreeSet<String>,
) -> Result<(), Box<dyn Error>> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_dir() {
            visit_proto_files(source_root, &path, names)?;
        } else if file_type.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "proto")
        {
            let relative = path.strip_prefix(source_root)?;
            names.insert(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}
