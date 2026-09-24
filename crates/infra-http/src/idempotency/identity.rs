//! Request identity: the `Idempotency-Key` grammar and the scope digest.

use infra_idempotency_store::ScopeKey;
use sha2::{Digest as _, Sha256};

/// The longest key in bytes.
pub(super) const MAX_KEY_BYTES: usize = 255;

/// The scope digest domain (system design section 8).
const SCOPE_DOMAIN: &[u8] = b"http-idempotency/scope/v1";
/// The caller tag when a verified subject is present.
const SUBJECT_TAG: u8 = 0x01;
/// The caller tag when the caller is the client ID.
const CLIENT_TAG: u8 = 0x02;

/// Whether `byte` is an RFC 9110 `tchar`.
pub(super) const fn is_tchar(byte: u8) -> bool {
    matches!(
        byte,
        b'!' | b'#'
            | b'$'
            | b'%'
            | b'&'
            | b'\''
            | b'*'
            | b'+'
            | b'-'
            | b'.'
            | b'^'
            | b'_'
            | b'`'
            | b'|'
            | b'~'
            | b'0'..=b'9'
            | b'A'..=b'Z'
            | b'a'..=b'z'
    )
}

/// The one valid key among the request's `Idempotency-Key` field values.
pub(super) fn valid_key<'a>(values: impl IntoIterator<Item = &'a [u8]>) -> Option<&'a [u8]> {
    let mut values = values.into_iter();
    let value = values.next()?;
    if values.next().is_some() {
        return None;
    }
    if value.is_empty() || value.len() > MAX_KEY_BYTES {
        return None;
    }
    value.iter().all(|&byte| is_tchar(byte)).then_some(value)
}

/// The verified caller a key is scoped to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Caller<'a> {
    Subject(&'a str),
    Client(&'a str),
}

impl<'a> Caller<'a> {
    /// The subject when present, otherwise the client id.
    pub(super) fn of(subject: Option<&'a str>, client_id: Option<&'a str>) -> Option<Self> {
        subject.map(Self::Subject).or(client_id.map(Self::Client))
    }
}

/// The scope digest of one caller, operation, and key.
pub(super) fn scope_key(
    issuer: &str,
    caller: Caller<'_>,
    operation_id: &str,
    key: &[u8],
) -> Option<ScopeKey> {
    let (tag, caller_text) = match caller {
        Caller::Subject(subject) => (SUBJECT_TAG, subject.as_bytes()),
        Caller::Client(client_id) => (CLIENT_TAG, client_id.as_bytes()),
    };
    let mut hasher = Sha256::new();
    hasher.update(SCOPE_DOMAIN);
    hasher.update([0u8]);
    write_text(&mut hasher, issuer.as_bytes())?;
    hasher.update([tag]);
    write_text(&mut hasher, caller_text)?;
    write_text(&mut hasher, operation_id.as_bytes())?;
    write_text(&mut hasher, key)?;
    Some(ScopeKey::from_digest(hasher.finalize().into()))
}

/// `u32` big-endian length, then `bytes`. `None` only when the length does
/// not fit `u32`.
fn write_text(hasher: &mut Sha256, bytes: &[u8]) -> Option<()> {
    let len = u32::try_from(bytes.len()).ok()?;
    hasher.update(len.to_be_bytes());
    hasher.update(bytes);
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_digest(hex: &str) -> [u8; 32] {
        assert_eq!(hex.len(), 64, "{hex}");
        let mut digest = [0u8; 32];
        for (byte, pair) in digest.iter_mut().zip(hex.as_bytes().as_chunks::<2>().0) {
            let text = std::str::from_utf8(pair).expect("ascii hex pair");
            *byte = u8::from_str_radix(text, 16).expect("hex digit");
        }
        digest
    }

    /// The bytes the key pattern's bracket expression admits, expanded
    /// without a regex crate: a char followed by `-` and another char is a
    /// range, and a trailing `-` is literal.
    fn pattern_allowed_bytes() -> [bool; 256] {
        let pattern = super::super::openapi::KEY_PATTERN.as_bytes();
        let bracket = pattern
            .strip_prefix(b"^[")
            .and_then(|rest| rest.strip_suffix(b"]+$"))
            .expect("KEY_PATTERN is a bracket expression");
        let mut allowed = [false; 256];
        let mut index = 0;
        while index < bracket.len() {
            if index + 2 < bracket.len() && bracket[index + 1] == b'-' {
                for value in bracket[index]..=bracket[index + 2] {
                    allowed[usize::from(value)] = true;
                }
                index += 3;
            } else {
                allowed[usize::from(bracket[index])] = true;
                index += 1;
            }
        }
        allowed
    }

    #[test]
    fn is_tchar_matches_every_byte_of_the_key_pattern_class() {
        let allowed = pattern_allowed_bytes();
        for byte in 0..=u8::MAX {
            assert_eq!(is_tchar(byte), allowed[usize::from(byte)], "byte {byte}");
        }
    }

    #[test]
    fn max_key_bytes_matches_the_openapi_constant() {
        assert_eq!(
            u64::try_from(MAX_KEY_BYTES).expect("fits u64"),
            super::super::openapi::KEY_MAX_LENGTH
        );
    }

    #[test]
    fn valid_key_accepts_the_shortest_the_longest_and_every_tchar_symbol() {
        assert_eq!(valid_key([b"k".as_slice()]), Some(b"k".as_slice()));

        let longest = vec![b'a'; MAX_KEY_BYTES];
        assert_eq!(valid_key([longest.as_slice()]), Some(longest.as_slice()));

        let every_symbol: Vec<u8> = (0..=u8::MAX).filter(|&byte| is_tchar(byte)).collect();
        assert_eq!(
            valid_key([every_symbol.as_slice()]),
            Some(every_symbol.as_slice())
        );
    }

    #[test]
    fn valid_key_rejects_every_invalid_shape() {
        let no_values: [&[u8]; 0] = [];
        assert_eq!(valid_key(no_values), None);
        assert_eq!(valid_key([b"".as_slice()]), None);

        let too_long = vec![b'a'; MAX_KEY_BYTES + 1];
        assert_eq!(valid_key([too_long.as_slice()]), None);

        assert_eq!(valid_key([b"a".as_slice(), b"b".as_slice()]), None);
        assert_eq!(valid_key([b"\"k\"".as_slice()]), None);
        assert_eq!(valid_key([b"a,b".as_slice()]), None);
        assert_eq!(valid_key([b"a b".as_slice()]), None);
        assert_eq!(valid_key([[0x01u8].as_slice()]), None);
        assert_eq!(valid_key([[0xc3u8, 0xa9].as_slice()]), None);
    }

    #[test]
    fn caller_of_prefers_the_subject_over_the_client_id() {
        assert_eq!(
            Caller::of(Some("subject"), Some("client")),
            Some(Caller::Subject("subject"))
        );
        assert_eq!(
            Caller::of(None, Some("client")),
            Some(Caller::Client("client"))
        );
        assert_eq!(
            Caller::of(Some("subject"), None),
            Some(Caller::Subject("subject"))
        );
        assert_eq!(Caller::of(None, None), None);
    }

    #[test]
    fn scope_key_matches_the_pinned_subject_vector() {
        let key = scope_key(
            "https://issuer.example",
            Caller::Subject("fixture-subject"),
            "createWidget",
            b"k-123",
        );
        let expected = ScopeKey::from_digest(decode_digest(
            "e48d23569e751551fdcb4683dd3e6026c6f3f177b5b79e2c7ae9f4e4e36187d4",
        ));
        assert_eq!(key, Some(expected));
    }

    #[test]
    fn scope_key_matches_the_pinned_client_vector() {
        let key = scope_key(
            "https://issuer.example",
            Caller::Client("fixture-subject"),
            "createWidget",
            b"k-123",
        );
        let expected = ScopeKey::from_digest(decode_digest(
            "19fa9cef85be61dd5fe3a519736fd76940cbbddec761038bd5a918e07ba1a7b9",
        ));
        assert_eq!(key, Some(expected));
    }
}
