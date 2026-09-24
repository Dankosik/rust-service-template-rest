//! Semantic fingerprints: canonical input encoding 1 and its digest.

use std::fmt;
use std::num::NonZeroU32;

use infra_idempotency_store::Digest;
use serde::ser::{
    SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant, SerializeTuple,
    SerializeTupleStruct, SerializeTupleVariant,
};
use serde::{Serialize, Serializer};
use sha2::{Digest as _, Sha256};

/// The versioned digest of an operation's semantic input.
pub struct Fingerprint {
    version: NonZeroU32,
    current: Digest,
    accepted: Vec<Digest>,
}

impl Fingerprint {
    /// Fingerprint `input` under `version`.
    ///
    /// # Errors
    ///
    /// Returns [`FingerprintError`] when `input` cannot be encoded.
    pub fn new<T: Serialize + ?Sized>(
        version: NonZeroU32,
        input: &T,
    ) -> Result<Self, FingerprintError> {
        let current = fingerprint_digest(version, &encode(input)?);
        Ok(Self {
            version,
            current,
            accepted: vec![current],
        })
    }

    /// Also accept `equivalent` as the same request under this version.
    ///
    /// # Errors
    ///
    /// Returns [`FingerprintError`] when `equivalent` cannot be encoded.
    pub fn also_matching<T: Serialize + ?Sized>(
        mut self,
        equivalent: &T,
    ) -> Result<Self, FingerprintError> {
        let digest = fingerprint_digest(self.version, &encode(equivalent)?);
        self.accepted.push(digest);
        Ok(self)
    }

    /// The digest a new record stores.
    pub(super) const fn current(&self) -> Digest {
        self.current
    }

    /// Every digest a live record may carry to match, current first.
    pub(super) fn accepted(&self) -> &[Digest] {
        &self.accepted
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Fingerprint")
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

/// The semantic input could not be encoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("idempotency fingerprint input cannot be encoded")]
pub struct FingerprintError;

/// The fingerprint digest domain (system design section 8).
const FINGERPRINT_DOMAIN: &[u8] = b"http-idempotency/fingerprint/v1";

/// SHA-256 over the domain, `0x00`, the big-endian `version`, and the
/// canonical `encoding`.
fn fingerprint_digest(version: NonZeroU32, encoding: &[u8]) -> Digest {
    let mut hasher = Sha256::new();
    hasher.update(FINGERPRINT_DOMAIN);
    hasher.update([0u8]);
    hasher.update(version.get().to_be_bytes());
    hasher.update(encoding);
    hasher.finalize().into()
}

/// The canonical input encoding 1 bytes of `input`.
pub(super) fn encode<T: Serialize + ?Sized>(input: &T) -> Result<Vec<u8>, FingerprintError> {
    let mut output = Vec::new();
    input
        .serialize(Encoder {
            output: &mut output,
        })
        .map_err(|_| FingerprintError)?;
    Ok(output)
}

// Tag bytes of canonical input encoding 1 (system design section 8).

/// bool.
const TAG_BOOL: u8 = 0x01;
/// Every integer width, and `u128` up to `i128::MAX`.
const TAG_INT: u8 = 0x02;
/// `u128` above `i128::MAX`.
const TAG_U128_HIGH: u8 = 0x03;
/// f32, f64.
const TAG_FLOAT: u8 = 0x04;
/// char, str.
const TAG_TEXT: u8 = 0x05;
/// bytes.
const TAG_BYTES: u8 = 0x06;
/// none.
const TAG_NONE: u8 = 0x07;
/// some(x).
const TAG_SOME: u8 = 0x08;
/// unit, unit struct.
const TAG_UNIT: u8 = 0x09;
/// unit variant.
const TAG_UNIT_VARIANT: u8 = 0x0a;
/// newtype variant.
const TAG_NEWTYPE_VARIANT: u8 = 0x0b;
/// seq, tuple, tuple struct.
const TAG_SEQ: u8 = 0x0c;
/// tuple variant.
const TAG_TUPLE_VARIANT: u8 = 0x0d;
/// map.
const TAG_MAP: u8 = 0x0e;
/// struct.
const TAG_STRUCT: u8 = 0x0f;
/// struct variant.
const TAG_STRUCT_VARIANT: u8 = 0x10;

/// Every `f64` NaN, regardless of payload or sign, canonicalizes to this bit
/// pattern.
const NAN_BITS: u64 = 0x7ff8_0000_0000_0000;

/// `u32` big-endian length, then the UTF-8 bytes of `value`.
fn write_text(output: &mut Vec<u8>, value: &str) -> Result<(), EncodeError> {
    let len = u32::try_from(value.len()).map_err(|_| EncodeError)?;
    output.extend_from_slice(&len.to_be_bytes());
    output.extend_from_slice(value.as_bytes());
    Ok(())
}

/// A `Serialize` value could not be encoded: a length or count overflowed
/// `u32`, a map had a duplicate encoded key or a dangling key, or the
/// value's own `Serialize` implementation failed.
#[derive(Debug)]
struct EncodeError;

impl fmt::Display for EncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("value cannot be encoded")
    }
}

impl std::error::Error for EncodeError {}

impl serde::ser::Error for EncodeError {
    fn custom<T: fmt::Display>(_msg: T) -> Self {
        Self
    }
}

/// Writes canonical input encoding 1 directly into `output`.
struct Encoder<'a> {
    output: &'a mut Vec<u8>,
}

/// Every integer width shares tag [`TAG_INT`]: the value as a 16-byte
/// big-endian `i128`.
fn write_int(output: &mut Vec<u8>, value: i128) {
    output.push(TAG_INT);
    output.extend_from_slice(&value.to_be_bytes());
}

impl<'a> Serializer for Encoder<'a> {
    type Ok = ();
    type Error = EncodeError;
    type SerializeSeq = SeqEncoder<'a>;
    type SerializeTuple = SeqEncoder<'a>;
    type SerializeTupleStruct = SeqEncoder<'a>;
    type SerializeTupleVariant = SeqEncoder<'a>;
    type SerializeMap = MapEncoder<'a>;
    type SerializeStruct = StructEncoder<'a>;
    type SerializeStructVariant = StructEncoder<'a>;

    fn serialize_bool(self, v: bool) -> Result<Self::Ok, Self::Error> {
        self.output.push(TAG_BOOL);
        self.output.push(u8::from(v));
        Ok(())
    }

    fn serialize_i8(self, v: i8) -> Result<Self::Ok, Self::Error> {
        write_int(self.output, i128::from(v));
        Ok(())
    }

    fn serialize_i16(self, v: i16) -> Result<Self::Ok, Self::Error> {
        write_int(self.output, i128::from(v));
        Ok(())
    }

    fn serialize_i32(self, v: i32) -> Result<Self::Ok, Self::Error> {
        write_int(self.output, i128::from(v));
        Ok(())
    }

    fn serialize_i64(self, v: i64) -> Result<Self::Ok, Self::Error> {
        write_int(self.output, i128::from(v));
        Ok(())
    }

    fn serialize_i128(self, v: i128) -> Result<Self::Ok, Self::Error> {
        write_int(self.output, v);
        Ok(())
    }

    fn serialize_u8(self, v: u8) -> Result<Self::Ok, Self::Error> {
        write_int(self.output, i128::from(v));
        Ok(())
    }

    fn serialize_u16(self, v: u16) -> Result<Self::Ok, Self::Error> {
        write_int(self.output, i128::from(v));
        Ok(())
    }

    fn serialize_u32(self, v: u32) -> Result<Self::Ok, Self::Error> {
        write_int(self.output, i128::from(v));
        Ok(())
    }

    fn serialize_u64(self, v: u64) -> Result<Self::Ok, Self::Error> {
        write_int(self.output, i128::from(v));
        Ok(())
    }

    fn serialize_u128(self, v: u128) -> Result<Self::Ok, Self::Error> {
        if let Ok(fits) = i128::try_from(v) {
            write_int(self.output, fits);
        } else {
            self.output.push(TAG_U128_HIGH);
            self.output.extend_from_slice(&v.to_be_bytes());
        }
        Ok(())
    }

    fn serialize_f32(self, v: f32) -> Result<Self::Ok, Self::Error> {
        self.serialize_f64(f64::from(v))
    }

    fn serialize_f64(self, v: f64) -> Result<Self::Ok, Self::Error> {
        let bits = if v.is_nan() { NAN_BITS } else { v.to_bits() };
        self.output.push(TAG_FLOAT);
        self.output.extend_from_slice(&bits.to_be_bytes());
        Ok(())
    }

    fn serialize_char(self, v: char) -> Result<Self::Ok, Self::Error> {
        let mut buf = [0u8; 4];
        let encoded: &str = v.encode_utf8(&mut buf);
        self.serialize_str(encoded)
    }

    fn serialize_str(self, v: &str) -> Result<Self::Ok, Self::Error> {
        self.output.push(TAG_TEXT);
        write_text(self.output, v)
    }

    fn serialize_bytes(self, v: &[u8]) -> Result<Self::Ok, Self::Error> {
        self.output.push(TAG_BYTES);
        let len = u32::try_from(v.len()).map_err(|_| EncodeError)?;
        self.output.extend_from_slice(&len.to_be_bytes());
        self.output.extend_from_slice(v);
        Ok(())
    }

    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        self.output.push(TAG_NONE);
        Ok(())
    }

    fn serialize_some<T>(self, value: &T) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        self.output.push(TAG_SOME);
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        self.output.push(TAG_UNIT);
        Ok(())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<Self::Ok, Self::Error> {
        self.serialize_unit()
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        self.output.push(TAG_UNIT_VARIANT);
        write_text(self.output, variant)
    }

    fn serialize_newtype_struct<T>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T>(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error>
    where
        T: ?Sized + Serialize,
    {
        self.output.push(TAG_NEWTYPE_VARIANT);
        write_text(&mut *self.output, variant)?;
        value.serialize(Encoder {
            output: self.output,
        })
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        self.output.push(TAG_SEQ);
        Ok(SeqEncoder::open(self.output))
    }

    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        self.output.push(TAG_TUPLE_VARIANT);
        write_text(&mut *self.output, variant)?;
        Ok(SeqEncoder::open(self.output))
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Ok(MapEncoder {
            output: self.output,
            entries: Vec::new(),
            pending_key: None,
        })
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        self.output.push(TAG_STRUCT);
        Ok(StructEncoder {
            output: self.output,
            fields: Vec::new(),
        })
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        self.output.push(TAG_STRUCT_VARIANT);
        write_text(&mut *self.output, variant)?;
        Ok(StructEncoder {
            output: self.output,
            fields: Vec::new(),
        })
    }
}

/// Returned for a seq, tuple, tuple struct, or tuple variant: a `u32` count
/// placeholder patched at `end` with the elements actually written, so an
/// unknown-length sequence works and the count never trusts a declared
/// length.
struct SeqEncoder<'a> {
    output: &'a mut Vec<u8>,
    start: usize,
    count: u32,
}

impl<'a> SeqEncoder<'a> {
    fn open(output: &'a mut Vec<u8>) -> Self {
        let start = output.len();
        output.extend_from_slice(&[0; 4]);
        Self {
            output,
            start,
            count: 0,
        }
    }
}

impl SeqEncoder<'_> {
    fn push<T>(&mut self, value: &T) -> Result<(), EncodeError>
    where
        T: ?Sized + Serialize,
    {
        value.serialize(Encoder {
            output: &mut *self.output,
        })?;
        self.count = self.count.checked_add(1).ok_or(EncodeError)?;
        Ok(())
    }

    fn finish(self) {
        let count = self.count.to_be_bytes();
        self.output[self.start..self.start + 4].copy_from_slice(&count);
    }
}

impl SerializeSeq for SeqEncoder<'_> {
    type Ok = ();
    type Error = EncodeError;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        self.push(value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.finish();
        Ok(())
    }
}

impl SerializeTuple for SeqEncoder<'_> {
    type Ok = ();
    type Error = EncodeError;

    fn serialize_element<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        self.push(value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.finish();
        Ok(())
    }
}

impl SerializeTupleStruct for SeqEncoder<'_> {
    type Ok = ();
    type Error = EncodeError;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        self.push(value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.finish();
        Ok(())
    }
}

impl SerializeTupleVariant for SeqEncoder<'_> {
    type Ok = ();
    type Error = EncodeError;

    fn serialize_field<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        self.push(value)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.finish();
        Ok(())
    }
}

/// Returned from `serialize_map`. Keys and values are encoded into scratch
/// buffers so entries can be sorted by encoded key bytes at `end`.
struct MapEncoder<'a> {
    output: &'a mut Vec<u8>,
    entries: Vec<(Vec<u8>, Vec<u8>)>,
    pending_key: Option<Vec<u8>>,
}

impl SerializeMap for MapEncoder<'_> {
    type Ok = ();
    type Error = EncodeError;

    fn serialize_key<T>(&mut self, key: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        if self.pending_key.is_some() {
            return Err(EncodeError);
        }
        let mut buf = Vec::new();
        key.serialize(Encoder { output: &mut buf })?;
        self.pending_key = Some(buf);
        Ok(())
    }

    fn serialize_value<T>(&mut self, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        let key = self.pending_key.take().ok_or(EncodeError)?;
        let mut buf = Vec::new();
        value.serialize(Encoder { output: &mut buf })?;
        self.entries.push((key, buf));
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        if self.pending_key.is_some() {
            return Err(EncodeError);
        }
        let mut entries = self.entries;
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        if entries.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err(EncodeError);
        }
        let count = u32::try_from(entries.len()).map_err(|_| EncodeError)?;
        self.output.push(TAG_MAP);
        self.output.extend_from_slice(&count.to_be_bytes());
        for (key, value) in entries {
            self.output.extend_from_slice(&key);
            self.output.extend_from_slice(&value);
        }
        Ok(())
    }
}

/// Returned from `serialize_struct` and `serialize_struct_variant`. Fields
/// are encoded into scratch buffers so they can be stably sorted by name
/// bytes at `end`; duplicate names are kept, in their original order.
struct StructEncoder<'a> {
    output: &'a mut Vec<u8>,
    fields: Vec<(&'static str, Vec<u8>)>,
}

impl SerializeStruct for StructEncoder<'_> {
    type Ok = ();
    type Error = EncodeError;

    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        let mut buf = Vec::new();
        value.serialize(Encoder { output: &mut buf })?;
        self.fields.push((key, buf));
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        finish_struct(self.output, self.fields)
    }
}

impl SerializeStructVariant for StructEncoder<'_> {
    type Ok = ();
    type Error = EncodeError;

    fn serialize_field<T>(&mut self, key: &'static str, value: &T) -> Result<(), Self::Error>
    where
        T: ?Sized + Serialize,
    {
        let mut buf = Vec::new();
        value.serialize(Encoder { output: &mut buf })?;
        self.fields.push((key, buf));
        Ok(())
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        finish_struct(self.output, self.fields)
    }
}

/// `u32` count, then the fields stably sorted by name bytes, each
/// `text(name) || enc(value)`.
fn finish_struct(
    output: &mut Vec<u8>,
    mut fields: Vec<(&'static str, Vec<u8>)>,
) -> Result<(), EncodeError> {
    fields.sort_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    let count = u32::try_from(fields.len()).map_err(|_| EncodeError)?;
    output.extend_from_slice(&count.to_be_bytes());
    for (name, value) in fields {
        write_text(output, name)?;
        output.extend_from_slice(&value);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::num::NonZeroU32;

    use serde::Serialize;

    use super::{Fingerprint, encode, fingerprint_digest};

    /// The section 8 pinned vector's input shape.
    #[derive(Serialize)]
    struct Widget<'a> {
        name: &'a str,
        count: u32,
        tags: Vec<&'a str>,
        note: Option<&'a str>,
    }

    /// A unit struct: encodes the same as `()`.
    #[derive(Serialize)]
    struct UnitStruct;

    /// A tuple struct: shares tag `0x0c` with a seq and a tuple.
    #[derive(Serialize)]
    struct Pair(u8, u8);

    /// One variant per enum row of the data model.
    #[derive(Serialize)]
    enum Sample<'a> {
        Unit,
        Newtype(u8),
        Tuple(u8, u8),
        Struct { flag: bool, label: &'a str },
    }

    /// A type whose `Serialize` calls `serialize_bytes` directly.
    struct RawBytes<'a>(&'a [u8]);

    impl Serialize for RawBytes<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_bytes(self.0)
        }
    }

    /// A `Serialize` implementation that always fails.
    struct AlwaysFails;

    impl Serialize for AlwaysFails {
        fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(<S::Error as serde::ser::Error>::custom("nope"))
        }
    }

    /// Collects a slice with a repeated key as a map.
    struct DuplicateKeys;

    impl Serialize for DuplicateKeys {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.collect_map([("a", 1u8), ("a", 2u8)])
        }
    }

    #[derive(Serialize)]
    struct NameThenCount<'a> {
        name: &'a str,
        count: u32,
    }

    #[derive(Serialize)]
    struct CountThenName<'a> {
        count: u32,
        name: &'a str,
    }

    #[derive(Serialize)]
    struct NarrowCount {
        count: u16,
    }

    #[derive(Serialize)]
    struct WideCount {
        count: u32,
    }

    #[derive(Serialize)]
    struct BaseFields {
        count: u32,
    }

    #[derive(Serialize)]
    struct ExtendedFields {
        count: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        extra: Option<u32>,
    }

    fn v(version: u32) -> NonZeroU32 {
        NonZeroU32::new(version).expect("nonzero")
    }

    fn decode_hex(hex: &str) -> Vec<u8> {
        assert_eq!(hex.len() % 2, 0, "{hex}");
        (0..hex.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("hex byte"))
            .collect()
    }

    fn decode_digest(hex: &str) -> [u8; 32] {
        <[u8; 32]>::try_from(decode_hex(hex).as_slice()).expect("32 bytes")
    }

    #[test]
    fn the_pinned_widget_vector_matches_the_encoding_and_both_version_digests() {
        let widget = Widget {
            name: "widget",
            count: 3,
            tags: vec!["a", "b"],
            note: None,
        };
        let encoded = encode(&widget).expect("encodes");
        assert_eq!(
            encoded,
            decode_hex(
                "0f0000000400000005636f756e740200000000000000000000000000000003000000046e616d650500000006776964676574000000046e6f74650700000004746167730c00000002050000000161050000000162"
            )
        );
        let v1 = Fingerprint::new(v(1), &widget).expect("fingerprints");
        assert_eq!(
            v1.current(),
            decode_digest("b4d06026e23a6de8d9fabe96199ec1713cebc5807fa7d09ace149402f559ff64")
        );
        let v2 = Fingerprint::new(v(2), &widget).expect("fingerprints");
        assert_eq!(
            v2.current(),
            decode_digest("cadb7034bde8b7b3ed8c5caaf0bc4638bd956b2fd1f466be921d2a7a203ed419")
        );
    }

    #[test]
    fn bool_encodes_as_tag_and_byte() {
        assert_eq!(encode(&true).expect("encodes"), decode_hex("0101"));
        assert_eq!(encode(&false).expect("encodes"), decode_hex("0100"));
    }

    #[test]
    fn integer_widths_share_one_signed_encoding_form() {
        assert_eq!(
            encode(&(-5i8)).expect("encodes"),
            decode_hex("02fffffffffffffffffffffffffffffffb")
        );
        assert_eq!(
            encode(&(-300i16)).expect("encodes"),
            decode_hex("02fffffffffffffffffffffffffffffed4")
        );
        assert_eq!(
            encode(&(-70_000i32)).expect("encodes"),
            decode_hex("02fffffffffffffffffffffffffffeee90")
        );
        assert_eq!(
            encode(&(-5_000_000_000i64)).expect("encodes"),
            decode_hex("02fffffffffffffffffffffffed5fa0e00")
        );
        assert_eq!(
            encode(&200u8).expect("encodes"),
            decode_hex("02000000000000000000000000000000c8")
        );
        assert_eq!(
            encode(&60_000u16).expect("encodes"),
            decode_hex("020000000000000000000000000000ea60")
        );
        assert_eq!(
            encode(&4_000_000_000u32).expect("encodes"),
            decode_hex("02000000000000000000000000ee6b2800")
        );
        assert_eq!(
            encode(&10_000_000_000_000_000_000u64).expect("encodes"),
            decode_hex("0200000000000000008ac7230489e80000")
        );
    }

    #[test]
    fn i128_min_and_u128_above_i128_max() {
        assert_eq!(
            encode(&i128::MIN).expect("encodes"),
            decode_hex("0280000000000000000000000000000000")
        );
        let boundary = u128::try_from(i128::MAX).expect("fits");
        assert_eq!(
            encode(&boundary).expect("encodes"),
            decode_hex("027fffffffffffffffffffffffffffffff")
        );
        assert_eq!(
            encode(&(boundary + 1)).expect("encodes"),
            decode_hex("0380000000000000000000000000000000")
        );
        assert_eq!(
            encode(&u128::MAX).expect("encodes"),
            decode_hex("03ffffffffffffffffffffffffffffffff")
        );
    }

    #[test]
    fn floats_canonicalize_every_nan_and_keep_other_bits() {
        assert_eq!(
            encode(&1.5f64).expect("encodes"),
            decode_hex("043ff8000000000000")
        );
        assert_eq!(
            encode(&1.5f32).expect("encodes"),
            decode_hex("043ff8000000000000")
        );
        assert_eq!(
            encode(&f64::NAN).expect("encodes"),
            decode_hex("047ff8000000000000")
        );
        assert_eq!(
            encode(&(-f64::NAN)).expect("encodes"),
            decode_hex("047ff8000000000000")
        );
        assert_eq!(
            encode(&f64::from_bits(0x7ff0_0000_0000_0001)).expect("encodes"),
            decode_hex("047ff8000000000000")
        );
        assert_eq!(
            encode(&f32::NAN).expect("encodes"),
            decode_hex("047ff8000000000000")
        );
    }

    #[test]
    fn char_and_str_share_the_text_encoding() {
        assert_eq!(encode(&'x').expect("encodes"), decode_hex("050000000178"));
        assert_eq!(
            encode(&'\u{e9}').expect("encodes"),
            decode_hex("0500000002c3a9")
        );
        assert_eq!(
            encode(&"hi").expect("encodes"),
            decode_hex("05000000026869")
        );
    }

    #[test]
    fn bytes_use_their_own_tag() {
        let value = RawBytes(&[1, 2, 3]);
        assert_eq!(
            encode(&value).expect("encodes"),
            decode_hex("0600000003010203")
        );
    }

    #[test]
    fn option_encodes_none_and_some() {
        assert_eq!(
            encode(&Option::<u8>::None).expect("encodes"),
            decode_hex("07")
        );
        assert_eq!(
            encode(&Some(5u8)).expect("encodes"),
            decode_hex("080200000000000000000000000000000005")
        );
    }

    #[test]
    fn unit_and_unit_struct_share_the_same_tag() {
        assert_eq!(encode(&()).expect("encodes"), decode_hex("09"));
        assert_eq!(encode(&UnitStruct).expect("encodes"), decode_hex("09"));
    }

    #[test]
    fn enum_variants_use_their_own_tags() {
        assert_eq!(
            encode(&Sample::Unit).expect("encodes"),
            decode_hex("0a00000004556e6974")
        );
        assert_eq!(
            encode(&Sample::Newtype(9)).expect("encodes"),
            decode_hex("0b000000074e6577747970650200000000000000000000000000000009")
        );
        assert_eq!(
            encode(&Sample::Tuple(1, 2)).expect("encodes"),
            decode_hex(
                "0d000000055475706c650000000202000000000000000000000000000000010200000000000000000000000000000002"
            )
        );
        assert_eq!(
            encode(&Sample::Struct {
                flag: true,
                label: "hi"
            })
            .expect("encodes"),
            decode_hex(
                "10000000065374727563740000000200000004666c61670101000000056c6162656c05000000026869"
            )
        );
    }

    #[test]
    fn seq_tuple_and_tuple_struct_share_the_same_tag() {
        let seq_hex = "0c00000003020000000000000000000000000000000102000000000000000000000000000000020200000000000000000000000000000003";
        assert_eq!(
            encode(&vec![1u8, 2u8, 3u8]).expect("encodes"),
            decode_hex(seq_hex)
        );
        let pair_hex =
            "0c0000000202000000000000000000000000000000010200000000000000000000000000000002";
        assert_eq!(encode(&(1u8, 2u8)).expect("encodes"), decode_hex(pair_hex));
        assert_eq!(encode(&Pair(1, 2)).expect("encodes"), decode_hex(pair_hex));
    }

    #[test]
    fn map_entries_sort_by_encoded_key_bytes() {
        let expected = decode_hex(
            "0e0000000205000000016102000000000000000000000000000000010500000001620200000000000000000000000000000002",
        );
        let mut ordered = BTreeMap::new();
        ordered.insert("b", 2u8);
        ordered.insert("a", 1u8);
        assert_eq!(encode(&ordered).expect("encodes"), expected);
    }

    #[test]
    fn hash_map_iteration_order_does_not_change_the_encoding() {
        let expected = decode_hex(
            "0e00000002050000000178020000000000000000000000000000000a0500000001790200000000000000000000000000000014",
        );
        let mut map = HashMap::new();
        map.insert("y", 20u8);
        map.insert("x", 10u8);
        assert_eq!(encode(&map).expect("encodes"), expected);
    }

    #[test]
    fn duplicate_encoded_map_keys_fail() {
        assert!(encode(&DuplicateKeys).is_err());
    }

    #[test]
    fn field_declaration_order_does_not_change_the_encoding() {
        let a = NameThenCount {
            name: "w",
            count: 1,
        };
        let b = CountThenName {
            count: 1,
            name: "w",
        };
        assert_eq!(encode(&a).expect("encodes"), encode(&b).expect("encodes"));
    }

    #[test]
    fn widening_an_integer_field_keeps_the_encoding() {
        let narrow = NarrowCount { count: 7 };
        let wide = WideCount { count: 7 };
        assert_eq!(
            encode(&narrow).expect("encodes"),
            encode(&wide).expect("encodes")
        );
    }

    #[test]
    fn a_skipped_absent_option_field_keeps_the_encoding() {
        let base = BaseFields { count: 1 };
        let extended = ExtendedFields {
            count: 1,
            extra: None,
        };
        assert_eq!(
            encode(&base).expect("encodes"),
            encode(&extended).expect("encodes")
        );
    }

    #[test]
    fn a_custom_serialize_error_fails() {
        assert!(encode(&AlwaysFails).is_err());
    }

    #[test]
    fn also_matching_keeps_current_and_appends_in_order() {
        let first = Fingerprint::new(v(1), &1u8).expect("fingerprints");
        let current = first.current();
        let matched = first
            .also_matching(&2u8)
            .expect("fingerprints")
            .also_matching(&3u8)
            .expect("fingerprints");
        assert_eq!(matched.current(), current);
        assert_eq!(matched.accepted().len(), 3);
        assert_eq!(matched.accepted()[0], current);
        assert_eq!(
            matched.accepted()[1],
            fingerprint_digest(v(1), &encode(&2u8).expect("encodes"))
        );
        assert_eq!(
            matched.accepted()[2],
            fingerprint_digest(v(1), &encode(&3u8).expect("encodes"))
        );
    }

    #[test]
    fn a_different_version_gives_a_different_digest() {
        let a = Fingerprint::new(v(1), &"x").expect("fingerprints");
        let b = Fingerprint::new(v(2), &"x").expect("fingerprints");
        assert_ne!(a.current(), b.current());
    }

    #[test]
    fn debug_shows_no_digest_bytes() {
        let fingerprint = Fingerprint::new(v(7), &"secret-input").expect("fingerprints");
        let rendered = format!("{fingerprint:?}");
        assert!(rendered.contains("version: 7"), "{rendered}");
        assert!(rendered.contains(".."), "{rendered}");
        assert!(!rendered.contains("current"), "{rendered}");
        assert!(!rendered.contains("accepted"), "{rendered}");
    }
}
