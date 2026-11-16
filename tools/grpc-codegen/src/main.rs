//! Buf owns schema compilation and builds the set with its imports: prost
//! resolves an imported message type from the set. This tool runs stock tonic
//! codegen over it and does not invoke protoc. Well-known types map to
//! `prost_types` and emit nothing; any other imported package is generated
//! beside the owned ones unless an `extern_path` names the crate that owns it.

use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow, bail};
use prost::Message;
use prost_types::FileDescriptorSet;

fn main() -> Result<()> {
    run(env::args_os())
}

#[expect(
    clippy::disallowed_methods,
    reason = "offline build-time codegen CLI without an async runtime"
)]
fn run(arguments: impl IntoIterator<Item = OsString>) -> Result<()> {
    let mut arguments = arguments.into_iter();
    let program = arguments.next().unwrap_or_default();
    let descriptor_path = required_path(&mut arguments, "descriptor-set path")?;
    let output_dir = required_path(&mut arguments, "output directory")?;

    if arguments.next().is_some() {
        bail!(
            "usage: {} <descriptor-set> <output-directory>",
            Path::new(&program).display()
        );
    }

    fs::create_dir_all(&output_dir)
        .with_context(|| format!("create output directory {}", output_dir.display()))?;
    let descriptor_bytes = fs::read(&descriptor_path)
        .with_context(|| format!("read descriptor set {}", descriptor_path.display()))?;
    let descriptor_set = FileDescriptorSet::decode(descriptor_bytes.as_slice())
        .with_context(|| format!("decode descriptor set {}", descriptor_path.display()))?;

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .build_transport(false)
        .codec_path("crate::codec::ContractCodec")
        .emit_rerun_if_changed(false)
        .include_file("_includes.rs")
        .out_dir(&output_dir)
        .compile_fds(descriptor_set)
        .with_context(|| {
            format!(
                "compile descriptor set {} into {}",
                descriptor_path.display(),
                output_dir.display()
            )
        })?;
    Ok(())
}

fn required_path(arguments: &mut impl Iterator<Item = OsString>, name: &str) -> Result<PathBuf> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("missing {name}"))
}

#[cfg(test)]
#[expect(
    clippy::disallowed_methods,
    reason = "synchronous filesystem fixtures for the offline codegen CLI"
)]
mod tests {
    use std::{ffi::OsString, fs};

    use prost::Message;
    use prost_types::{DescriptorProto, FileDescriptorProto, FileDescriptorSet};
    use tempfile::tempdir;

    use super::run;

    fn args<const N: usize>(items: [&str; N]) -> [OsString; N] {
        items.map(OsString::from)
    }

    #[test]
    fn rejects_missing_and_extra_cli_arguments() {
        let missing_descriptor = run(args(["grpc-codegen"])).expect_err("missing descriptor path");
        assert_eq!(
            missing_descriptor.to_string(),
            "missing descriptor-set path"
        );

        let missing_output =
            run(args(["grpc-codegen", "in.binpb"])).expect_err("missing output directory");
        assert_eq!(missing_output.to_string(), "missing output directory");

        let extra = run(args(["grpc-codegen", "in.binpb", "out", "extra"]))
            .expect_err("extra CLI argument");
        assert_eq!(
            extra.to_string(),
            "usage: grpc-codegen <descriptor-set> <output-directory>"
        );
    }

    #[test]
    fn reports_output_directory_creation_context() {
        let scratch = tempdir().expect("create scratch directory");
        let blocker = scratch.path().join("occupied");
        fs::write(&blocker, b"file").expect("create blocking file");
        let output_dir = blocker.join("nested");
        let descriptor_path = scratch.path().join("descriptors.binpb");

        let err = run([
            OsString::from("grpc-codegen"),
            descriptor_path.into_os_string(),
            output_dir.clone().into_os_string(),
        ])
        .expect_err("create_dir_all over regular file must fail");

        let rendered = format!("{err:#}");
        assert!(
            rendered.contains(&format!("create output directory {}", output_dir.display())),
            "unexpected error: {rendered}"
        );
        assert!(
            err.source().is_some(),
            "expected underlying io::Error source"
        );
    }

    #[test]
    fn reports_descriptor_read_and_decode_context() {
        let scratch = tempdir().expect("create scratch directory");
        let missing_descriptor = scratch.path().join("missing.binpb");
        let output_dir = scratch.path().join("out");

        let read_err = run([
            OsString::from("grpc-codegen"),
            missing_descriptor.clone().into_os_string(),
            output_dir.clone().into_os_string(),
        ])
        .expect_err("missing descriptor file must fail");
        let rendered_read = format!("{read_err:#}");
        assert!(
            rendered_read.contains(&format!(
                "read descriptor set {}",
                missing_descriptor.display()
            )),
            "unexpected error: {rendered_read}"
        );
        assert!(
            read_err.source().is_some(),
            "expected underlying io::Error source"
        );

        let invalid_descriptor = scratch.path().join("invalid.binpb");
        fs::write(&invalid_descriptor, [0xFF, 0x00]).expect("write invalid protobuf bytes");

        let decode_err = run([
            OsString::from("grpc-codegen"),
            invalid_descriptor.clone().into_os_string(),
            output_dir.into_os_string(),
        ])
        .expect_err("corrupt descriptor bytes must fail");
        let rendered_decode = format!("{decode_err:#}");
        assert!(
            rendered_decode.contains(&format!(
                "decode descriptor set {}",
                invalid_descriptor.display()
            )),
            "unexpected error: {rendered_decode}"
        );
        assert!(
            decode_err.source().is_some(),
            "expected underlying prost::DecodeError source"
        );
    }

    #[test]
    fn reports_codegen_context_with_output_directory() {
        let scratch = tempdir().expect("create scratch directory");
        let descriptor_path = scratch.path().join("valid.binpb");
        let output_dir = scratch.path().join("generated");
        fs::create_dir_all(output_dir.join("_includes.rs"))
            .expect("create conflicting directory at include file path");

        let valid = FileDescriptorSet {
            file: vec![FileDescriptorProto {
                name: Some("ping.proto".to_owned()),
                package: Some("ping.v1".to_owned()),
                syntax: Some("proto3".to_owned()),
                message_type: vec![DescriptorProto {
                    name: Some("Ping".to_owned()),
                    ..DescriptorProto::default()
                }],
                ..FileDescriptorProto::default()
            }],
        };
        fs::write(&descriptor_path, valid.encode_to_vec()).expect("write descriptor set");

        let err = run([
            OsString::from("grpc-codegen"),
            descriptor_path.clone().into_os_string(),
            output_dir.clone().into_os_string(),
        ])
        .expect_err("writing _includes.rs over directory must fail codegen");

        let rendered = format!("{err:#}");
        assert!(
            rendered.contains(&format!(
                "compile descriptor set {} into {}",
                descriptor_path.display(),
                output_dir.display()
            )),
            "unexpected error: {rendered}"
        );
        assert!(
            err.source().is_some(),
            "expected underlying codegen error source"
        );
    }
}
