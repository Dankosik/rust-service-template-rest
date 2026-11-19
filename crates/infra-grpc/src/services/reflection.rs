use std::collections::hash_map::Entry;

use bytes::Bytes;
use foldhash::HashMap;
use futures_util::StreamExt as _;
use http::Request;
use http_body_util::{BodyStream, StreamBody};
use prost::Message;
use prost_reflect::{DynamicMessage, ReflectMessage as _};
use prost_types::FileDescriptorProto;
use tonic::body::Body;

use crate::{Error, ResponseContext};

#[derive(Message)]
struct EncodedFileDescriptorSet {
    #[prost(bytes = "bytes", repeated, tag = "1")]
    file: Vec<Bytes>,
}

pub(super) fn check_files(sets: impl IntoIterator<Item = &'static [u8]>) -> Result<(), Error> {
    let mut files: HashMap<String, Bytes> = HashMap::default();
    for encoded in sets {
        let set = EncodedFileDescriptorSet::decode(Bytes::from_static(encoded))
            .map_err(Error::InspectReflectionFile)?;
        for file in set.file {
            let descriptor =
                FileDescriptorProto::decode(file.as_ref()).map_err(Error::InspectReflectionFile)?;
            let name = descriptor.name.ok_or(Error::MissingReflectionFileName)?;
            match files.entry(name) {
                Entry::Occupied(entry) => {
                    let first = entry.get();
                    if file != first && definition(&file)? != definition(first)? {
                        return Err(Error::ConflictingFileDescription(entry.remove_entry().0));
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert(file);
                }
            }
        }
    }
    Ok(())
}

fn definition(file: &[u8]) -> Result<DynamicMessage, Error> {
    let mut descriptor = DynamicMessage::decode(FileDescriptorProto::default().descriptor(), file)
        .map_err(Error::InspectReflectionFile)?;
    descriptor.clear_field_by_name("source_code_info");
    Ok(descriptor)
}

pub(super) fn stop_request_body_on_cancellation(request: Request<Body>) -> Request<Body> {
    let Some(response) = request.extensions().get::<ResponseContext>() else {
        return request;
    };
    let cancelled = response
        .operation()
        .cancellation()
        .clone()
        .cancelled_owned();
    request.map(|body| Body::new(StreamBody::new(BodyStream::new(body).take_until(cancelled))))
}

#[cfg(test)]
mod tests {
    use super::EncodedFileDescriptorSet;
    use crate::{Error, Services};
    use bytes::Bytes;
    use prost::Message as _;

    fn descriptor_set(fields: &[&[u8]]) -> &'static [u8] {
        EncodedFileDescriptorSet {
            file: vec![Bytes::from(fields.concat())],
        }
        .encode_to_vec()
        .leak()
    }

    #[test]
    fn reflection_identifies_a_decoded_file_without_a_name() {
        let mut services = Services::new();
        services.describe(descriptor_set(&[])).unwrap();
        services.add_reflection().unwrap();
        std::assert_matches!(
            services.attach_reflection(),
            Err(Error::MissingReflectionFileName)
        );
    }

    #[test]
    fn reflection_rejects_conflicting_options_on_a_shared_import() {
        const NAME: &[u8] = b"\x0a\x0cshared.proto";
        const OPTION_ENABLED: &[u8] = b"\x42\x09\x0a\x01x\x50\x01\x80\xb5\x18\x01";
        const REORDERED_OPTIONS: &[u8] = b"\x42\x09\x50\x01\x80\xb5\x18\x01\x0a\x01x";
        const OPTION_DISABLED: &[u8] = b"\x42\x09\x0a\x01x\x50\x01\x80\xb5\x18\x00";
        const SOURCE_INFO: &[u8] = b"\x4a\x02\x0a\x00";

        let original = descriptor_set(&[NAME, OPTION_ENABLED]);
        for (duplicate, conflicting) in [
            (original, false),
            (
                descriptor_set(&[SOURCE_INFO, REORDERED_OPTIONS, NAME]),
                false,
            ),
            (descriptor_set(&[NAME, OPTION_DISABLED]), true),
        ] {
            let mut services = Services::new();
            services.describe(original).unwrap();
            services.describe(duplicate).unwrap();
            services.add_reflection().unwrap();
            let result = services.attach_reflection();
            if conflicting {
                std::assert_matches!(result, Err(Error::ConflictingFileDescription(name)) if name == "shared.proto");
            } else {
                result.unwrap();
            }
        }
    }
}
