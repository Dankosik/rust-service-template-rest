use http::HeaderMap;

pub(crate) fn method_path(service: &str, method: &str) -> String {
    format!("/{service}/{method}")
}

/// gRPC over HTTP/2 §Requests and RFC 9110 §§5.6.2, 5.6.4, 5.6.6.
pub(crate) fn has_native_content_type(headers: &HeaderMap) -> bool {
    let mut values = headers.get_all(http::header::CONTENT_TYPE).iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return false;
    };
    let bytes = value.as_bytes();
    bytes == tonic::metadata::GRPC_CONTENT_TYPE.as_bytes()
        || bytes == b"application/grpc+proto"
        || is_native_grpc_media_type(bytes)
}

const NATIVE_MEDIA_TYPE: &[u8] = b"application/grpc";

fn is_native_grpc_media_type(bytes: &[u8]) -> bool {
    let Some((prefix, mut rest)) = skip_ows(bytes).split_at_checked(NATIVE_MEDIA_TYPE.len()) else {
        return false;
    };
    if !prefix.eq_ignore_ascii_case(NATIVE_MEDIA_TYPE) {
        return false;
    }
    if let Some(after_plus) = rest.strip_prefix(b"+") {
        let Some(after_suffix) = skip_token(after_plus) else {
            return false;
        };
        rest = after_suffix;
    }
    loop {
        rest = skip_ows(rest);
        let Some(after_semi) = rest.strip_prefix(b";") else {
            return rest.is_empty();
        };
        rest = skip_ows(after_semi);
        if rest.is_empty() || rest.starts_with(b";") {
            continue;
        }
        let Some(after_name) = skip_token(rest) else {
            return false;
        };
        let Some(after_eq) = after_name.strip_prefix(b"=") else {
            return false;
        };
        let Some(after_value) = skip_parameter_value(after_eq) else {
            return false;
        };
        rest = after_value;
    }
}

fn skip_ows(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|&byte| !matches!(byte, b' ' | b'\t'))
        .unwrap_or(bytes.len());
    &bytes[start..]
}

fn skip_token(bytes: &[u8]) -> Option<&[u8]> {
    let end = bytes
        .iter()
        .position(|&byte| !shared_rules::rfc9110::is_tchar(byte))
        .unwrap_or(bytes.len());
    (end > 0).then(|| &bytes[end..])
}

fn skip_parameter_value(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.starts_with(b"\"") {
        skip_quoted_string(bytes)
    } else {
        skip_token(bytes)
    }
}

fn skip_quoted_string(bytes: &[u8]) -> Option<&[u8]> {
    let mut rest = bytes.strip_prefix(b"\"")?;
    while let Some((&byte, after)) = rest.split_first() {
        match byte {
            b'"' => return Some(after),
            b'\\' => {
                let (&escaped, after_escaped) = after.split_first()?;
                if !is_quoted_pair_char(escaped) {
                    return None;
                }
                rest = after_escaped;
            }
            byte if is_qdtext(byte) => rest = after,
            _ => return None,
        }
    }
    None
}

fn is_quoted_pair_char(byte: u8) -> bool {
    byte == b'\t' || matches!(byte, 0x20..=0x7e | 0x80..=0xff)
}

fn is_qdtext(byte: u8) -> bool {
    is_quoted_pair_char(byte) && byte != b'"' && byte != b'\\'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_content_type_accepts_valid_rfc9110_grpc_media_types() {
        for raw in [
            b"application/grpc".as_slice(),
            b"application/grpc+proto",
            b"Application/GRPC",
            b" \tapplication/grpc+proto \t",
            b"application/grpc+json+v2",
            b"application/grpc++",
            b"application/grpc;",
            b"application/grpc ; ; ",
            b"application/grpc; ; charset=utf-8; ;",
            b"application/grpc+proto; version=2; profile=\"custom;v2, test\"",
            b"application/grpc; empty=\"\"",
            b"application/grpc; escaped=\"a\\\"b\\\\c\\td\"",
            b"application/grpc; obs=\"\x80\xff\"",
            b"application/grpc; escaped_obs=\"\\\x80\\\xff\"",
            b"application/grpc; tab=\"a\tb\"",
        ] {
            assert!(is_native_grpc_media_type(raw), "{raw:?}");
            let mut headers = HeaderMap::new();
            headers.insert(
                http::header::CONTENT_TYPE,
                http::HeaderValue::from_bytes(raw).unwrap(),
            );
            assert!(has_native_content_type(&headers), "{raw:?}");
        }
    }

    #[test]
    fn native_content_type_rejects_missing_duplicate_and_malformed_values() {
        let empty = HeaderMap::new();
        assert!(!has_native_content_type(&empty));

        let mut duplicate = HeaderMap::new();
        duplicate.append(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/grpc"),
        );
        duplicate.append(
            http::header::CONTENT_TYPE,
            http::HeaderValue::from_static("application/grpc"),
        );
        assert!(!has_native_content_type(&duplicate));

        for raw in [
            b"".as_slice(),
            b"application/grp",
            b"application/grpc-web",
            b"application/grpc-web+proto",
            b"application/grpc+",
            b"application/grpc+ ",
            b"application/grpc+proto/extra",
            b"text/plain",
            b"application/grpc; profile",
            b"application/grpc; profile=",
            b"application/grpc; =value",
            b"application/grpc; a = b",
            b"application/grpc; a= b",
            b"application/grpc; a =b",
            b"application/grpc; a=b, text/plain",
            b"application/grpc; a=b junk",
            b"application/grpc; a=\"unterminated",
            b"application/grpc; a=\"trailing\\",
            b"application/grpc; a=\"escaped-quote\\\"",
            b"application/grpc; a=\"ok\"junk",
            b"application/grpc; a=\"\x00\"",
            b"application/grpc; a=\"\n\"",
            b"application/grpc; a=\"\r\"",
            b"application/grpc; a=\"\x7f\"",
            b"application/grpc; a=\"\\\x00\"",
            b"application/grpc; a=\"\\\n\"",
            b"application/grpc; a=\"\\\x7f\"",
            b"\napplication/grpc",
            b"application/grpc\r\n",
            b"application/grpc; a=b\x7f",
        ] {
            assert!(!is_native_grpc_media_type(raw), "{raw:?}");
            if let Ok(value) = http::HeaderValue::from_bytes(raw) {
                let mut headers = HeaderMap::new();
                headers.insert(http::header::CONTENT_TYPE, value);
                assert!(!has_native_content_type(&headers), "{raw:?}");
            }
        }
    }
}
