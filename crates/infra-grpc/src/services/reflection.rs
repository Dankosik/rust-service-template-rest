use bytes::Bytes;
use foldhash::HashMap;
use futures_util::StreamExt as _;
use http::Request;
use http_body_util::{BodyStream, StreamBody};
use prost::Message;
use prost::encoding::{DecodeContext, decode_key, skip_field};
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
            .map_err(|_| Error::InvalidFileDescriptorSet)?;
        for file in set.file {
            let descriptor = FileDescriptorProto::decode(file.clone())
                .map_err(|_| Error::InvalidFileDescriptorSet)?;
            let name = descriptor.name.ok_or(Error::InvalidFileDescriptorSet)?;
            if let Some(first) = files.get(&name) {
                if &file != first && definition_fields(&file)? != definition_fields(first)? {
                    return Err(Error::ConflictingFileDescription(name));
                }
            } else {
                files.insert(name, file);
            }
        }
    }
    Ok(())
}

fn definition_fields(mut input: &[u8]) -> Result<Vec<(u32, &[u8])>, Error> {
    const SOURCE_CODE_INFO_TAG: u32 = 9;

    let mut fields = Vec::new();
    while !input.is_empty() {
        let field = input;
        let (tag, wire_type) =
            decode_key(&mut input).map_err(|_| Error::InvalidFileDescriptorSet)?;
        skip_field(wire_type, tag, &mut input, DecodeContext::default())
            .map_err(|_| Error::InvalidFileDescriptorSet)?;
        if tag != SOURCE_CODE_INFO_TAG {
            fields.push((tag, &field[..field.len() - input.len()]));
        }
    }
    fields.sort_by_key(|(tag, _)| *tag);
    Ok(fields)
}

pub(super) fn cancel_request_body(request: Request<Body>) -> Request<Body> {
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
    fn reflection_rejects_conflicting_options_on_a_shared_import() {
        const NAME: &[u8] = b"\x0a\x0cshared.proto";
        const OPTION_ENABLED: &[u8] = b"\x42\x04\x80\xb5\x18\x01";
        const OPTION_DISABLED: &[u8] = b"\x42\x04\x80\xb5\x18\x00";
        const SOURCE_INFO: &[u8] = b"\x4a\x02\x0a\x00";

        let original = descriptor_set(&[NAME, OPTION_ENABLED]);
        for (duplicate, expected) in [
            (original, Ok(())),
            (descriptor_set(&[SOURCE_INFO, OPTION_ENABLED, NAME]), Ok(())),
            (
                descriptor_set(&[NAME, OPTION_DISABLED]),
                Err(Error::ConflictingFileDescription("shared.proto".to_owned())),
            ),
        ] {
            let mut services = Services::new();
            services.describe(original).unwrap();
            services.describe(duplicate).unwrap();
            services.add_reflection().unwrap();
            assert_eq!(services.attach_reflection(), expected);
        }
    }
}
