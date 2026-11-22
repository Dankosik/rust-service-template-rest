use std::{
    cell::Cell,
    fmt::{self, Display},
    io::{self, Write},
};

use serde::{Serialize, Serializer, ser};

const NON_FINITE: &str = "non-finite float in JSON payload";
const MIN_BOUNDED_CAPACITY: usize = 128;

#[derive(Debug)]
pub enum EncodeError {
    Serialize(serde_json::Error),
    TooLarge { bytes: usize },
}

impl Display for EncodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Serialize(error) => error.fmt(formatter),
            Self::TooLarge { bytes } => {
                write!(formatter, "serialized JSON payload is {bytes} bytes")
            }
        }
    }
}

impl std::error::Error for EncodeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Serialize(error) => Some(error),
            Self::TooLarge { .. } => None,
        }
    }
}

pub fn to_writer<W: Write, T: Serialize + ?Sized>(
    writer: W,
    value: &T,
) -> Result<(), serde_json::Error> {
    let saw_non_finite_float = Cell::new(false);
    let result = value.serialize(FiniteSerializer {
        inner: &mut serde_json::Serializer::new(writer),
        saw_non_finite_float: &saw_non_finite_float,
    });
    if saw_non_finite_float.get() {
        return Err(ser::Error::custom(NON_FINITE));
    }
    result
}

pub fn to_vec_bounded<T: Serialize + ?Sized>(
    value: &T,
    max_bytes: usize,
) -> Result<Vec<u8>, EncodeError> {
    to_vec_bounded_with_capacity(value, max_bytes, 0)
}

pub fn to_vec_bounded_with_capacity<T: Serialize + ?Sized>(
    value: &T,
    max_bytes: usize,
    capacity: usize,
) -> Result<Vec<u8>, EncodeError> {
    let mut writer = BoundedWriter::with_capacity(max_bytes, capacity);
    to_writer(&mut writer, value).map_err(EncodeError::Serialize)?;
    if writer.total_bytes > max_bytes {
        return Err(EncodeError::TooLarge {
            bytes: writer.total_bytes,
        });
    }
    Ok(writer.bytes)
}

struct BoundedWriter {
    bytes: Vec<u8>,
    total_bytes: usize,
    limit: usize,
}

impl BoundedWriter {
    fn with_capacity(limit: usize, capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity.min(limit)),
            total_bytes: 0,
            limit,
        }
    }
}

impl Write for BoundedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.total_bytes = self
            .total_bytes
            .checked_add(buf.len())
            .ok_or_else(|| io::Error::other("serialized payload length overflow"))?;
        let retained = buf.len().min(self.limit - self.bytes.len());
        let needed = self.bytes.len() + retained;
        if needed > self.bytes.capacity() {
            let capacity = self
                .bytes
                .capacity()
                .saturating_mul(2)
                .max(MIN_BOUNDED_CAPACITY)
                .max(needed)
                .min(self.limit);
            self.bytes.reserve_exact(capacity - self.bytes.len());
        }
        self.bytes.extend_from_slice(&buf[..retained]);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct FiniteSerializer<'a, S> {
    inner: S,
    saw_non_finite_float: &'a Cell<bool>,
}

struct FiniteValue<'a, T: ?Sized> {
    inner: &'a T,
    saw_non_finite_float: &'a Cell<bool>,
}

impl<T: Serialize + ?Sized> Serialize for FiniteValue<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.inner.serialize(FiniteSerializer {
            inner: serializer,
            saw_non_finite_float: self.saw_non_finite_float,
        })
    }
}

struct Compound<'a, C> {
    inner: C,
    saw_non_finite_float: &'a Cell<bool>,
}

macro_rules! forward_scalar {
    ($($method:ident($ty:ty)),* $(,)?) => {
        $(fn $method(self, value: $ty) -> Result<Self::Ok, Self::Error> {
            self.inner.$method(value)
        })*
    };
}

impl<'a, S: Serializer> Serializer for FiniteSerializer<'a, S> {
    type Ok = S::Ok;
    type Error = S::Error;
    type SerializeSeq = Compound<'a, S::SerializeSeq>;
    type SerializeTuple = Compound<'a, S::SerializeTuple>;
    type SerializeTupleStruct = Compound<'a, S::SerializeTupleStruct>;
    type SerializeTupleVariant = Compound<'a, S::SerializeTupleVariant>;
    type SerializeMap = Compound<'a, S::SerializeMap>;
    type SerializeStruct = Compound<'a, S::SerializeStruct>;
    type SerializeStructVariant = Compound<'a, S::SerializeStructVariant>;

    forward_scalar! {
        serialize_bool(bool),
        serialize_i8(i8), serialize_i16(i16), serialize_i32(i32),
        serialize_i64(i64), serialize_i128(i128),
        serialize_u8(u8), serialize_u16(u16), serialize_u32(u32),
        serialize_u64(u64), serialize_u128(u128),
        serialize_char(char), serialize_str(&str), serialize_bytes(&[u8]),
    }

    fn serialize_f32(self, value: f32) -> Result<Self::Ok, Self::Error> {
        if !value.is_finite() {
            self.saw_non_finite_float.set(true);
            return Err(ser::Error::custom(NON_FINITE));
        }
        self.inner.serialize_f32(value)
    }

    fn serialize_f64(self, value: f64) -> Result<Self::Ok, Self::Error> {
        if !value.is_finite() {
            self.saw_non_finite_float.set(true);
            return Err(ser::Error::custom(NON_FINITE));
        }
        self.inner.serialize_f64(value)
    }

    fn serialize_none(self) -> Result<Self::Ok, Self::Error> {
        self.inner.serialize_none()
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<Self::Ok, Self::Error> {
        self.inner.serialize_some(&FiniteValue {
            inner: value,
            saw_non_finite_float: self.saw_non_finite_float,
        })
    }

    fn serialize_unit(self) -> Result<Self::Ok, Self::Error> {
        self.inner.serialize_unit()
    }

    fn serialize_unit_struct(self, name: &'static str) -> Result<Self::Ok, Self::Error> {
        self.inner.serialize_unit_struct(name)
    }

    fn serialize_unit_variant(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
    ) -> Result<Self::Ok, Self::Error> {
        self.inner
            .serialize_unit_variant(name, variant_index, variant)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        self.inner.serialize_newtype_struct(
            name,
            &FiniteValue {
                inner: value,
                saw_non_finite_float: self.saw_non_finite_float,
            },
        )
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<Self::Ok, Self::Error> {
        self.inner.serialize_newtype_variant(
            name,
            variant_index,
            variant,
            &FiniteValue {
                inner: value,
                saw_non_finite_float: self.saw_non_finite_float,
            },
        )
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        Ok(Compound {
            inner: self.inner.serialize_seq(len)?,
            saw_non_finite_float: self.saw_non_finite_float,
        })
    }

    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        Ok(Compound {
            inner: self.inner.serialize_tuple(len)?,
            saw_non_finite_float: self.saw_non_finite_float,
        })
    }

    fn serialize_tuple_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        Ok(Compound {
            inner: self.inner.serialize_tuple_struct(name, len)?,
            saw_non_finite_float: self.saw_non_finite_float,
        })
    }

    fn serialize_tuple_variant(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleVariant, Self::Error> {
        Ok(Compound {
            inner: self
                .inner
                .serialize_tuple_variant(name, variant_index, variant, len)?,
            saw_non_finite_float: self.saw_non_finite_float,
        })
    }

    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Ok(Compound {
            inner: self.inner.serialize_map(len)?,
            saw_non_finite_float: self.saw_non_finite_float,
        })
    }

    fn serialize_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        Ok(Compound {
            inner: self.inner.serialize_struct(name, len)?,
            saw_non_finite_float: self.saw_non_finite_float,
        })
    }

    fn serialize_struct_variant(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStructVariant, Self::Error> {
        Ok(Compound {
            inner: self
                .inner
                .serialize_struct_variant(name, variant_index, variant, len)?,
            saw_non_finite_float: self.saw_non_finite_float,
        })
    }

    fn collect_str<T: Display + ?Sized>(self, value: &T) -> Result<Self::Ok, Self::Error> {
        self.inner.collect_str(value)
    }

    fn is_human_readable(&self) -> bool {
        self.inner.is_human_readable()
    }
}

macro_rules! sequence_compound {
    ($trait:ident, $method:ident) => {
        impl<C: ser::$trait> ser::$trait for Compound<'_, C> {
            type Ok = C::Ok;
            type Error = C::Error;

            fn $method<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
                self.inner.$method(&FiniteValue {
                    inner: value,
                    saw_non_finite_float: self.saw_non_finite_float,
                })
            }

            fn end(self) -> Result<Self::Ok, Self::Error> {
                self.inner.end()
            }
        }
    };
}

sequence_compound!(SerializeSeq, serialize_element);
sequence_compound!(SerializeTuple, serialize_element);
sequence_compound!(SerializeTupleStruct, serialize_field);
sequence_compound!(SerializeTupleVariant, serialize_field);

impl<C: ser::SerializeMap> ser::SerializeMap for Compound<'_, C> {
    type Ok = C::Ok;
    type Error = C::Error;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Self::Error> {
        self.inner.serialize_key(&FiniteValue {
            inner: key,
            saw_non_finite_float: self.saw_non_finite_float,
        })
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.inner.serialize_value(&FiniteValue {
            inner: value,
            saw_non_finite_float: self.saw_non_finite_float,
        })
    }

    fn serialize_entry<K: Serialize + ?Sized, V: Serialize + ?Sized>(
        &mut self,
        key: &K,
        value: &V,
    ) -> Result<(), Self::Error> {
        self.inner.serialize_entry(
            &FiniteValue {
                inner: key,
                saw_non_finite_float: self.saw_non_finite_float,
            },
            &FiniteValue {
                inner: value,
                saw_non_finite_float: self.saw_non_finite_float,
            },
        )
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        self.inner.end()
    }
}

macro_rules! struct_compound {
    ($trait:ident) => {
        impl<C: ser::$trait> ser::$trait for Compound<'_, C> {
            type Ok = C::Ok;
            type Error = C::Error;

            fn serialize_field<T: Serialize + ?Sized>(
                &mut self,
                key: &'static str,
                value: &T,
            ) -> Result<(), Self::Error> {
                self.inner.serialize_field(
                    key,
                    &FiniteValue {
                        inner: value,
                        saw_non_finite_float: self.saw_non_finite_float,
                    },
                )
            }

            fn skip_field(&mut self, key: &'static str) -> Result<(), Self::Error> {
                self.inner.skip_field(key)
            }

            fn end(self) -> Result<Self::Ok, Self::Error> {
                self.inner.end()
            }
        }
    };
}

struct_compound!(SerializeStruct);
struct_compound!(SerializeStructVariant);

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        fmt,
        io::{Cursor, Write as _},
    };

    use serde::{
        Serialize, Serializer,
        ser::{self, SerializeMap, SerializeSeq},
    };

    use super::{
        BoundedWriter, EncodeError, to_vec_bounded, to_vec_bounded_with_capacity, to_writer,
    };

    #[derive(Serialize)]
    struct Newtype<T>(T);

    #[derive(Serialize)]
    struct TupleStruct<T>(bool, T);

    #[derive(Serialize)]
    struct Record<T> {
        value: T,
        #[serde(skip_serializing_if = "Option::is_none")]
        omitted: Option<()>,
    }

    #[derive(Serialize)]
    enum Variant<T> {
        Unit,
        Newtype(T),
        Tuple(bool, T),
        Struct {
            value: T,
            #[serde(skip_serializing_if = "Option::is_none")]
            omitted: Option<()>,
        },
    }

    struct Map<K, V> {
        key: K,
        value: V,
        separate: bool,
    }

    impl<K: Serialize, V: Serialize> Serialize for Map<K, V> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut map = serializer.serialize_map(Some(1))?;
            if self.separate {
                map.serialize_key(&self.key)?;
                map.serialize_value(&self.value)?;
            } else {
                map.serialize_entry(&self.key, &self.value)?;
            }
            map.end()
        }
    }

    fn rejects(value: &(impl Serialize + ?Sized)) {
        let error = to_writer(Vec::new(), value).unwrap_err();
        assert_eq!(error.to_string(), "non-finite float in JSON payload");
    }

    fn rejects_every_shape<T: Serialize + Copy>(value: T) {
        rejects(&value);
        rejects(&Some(value));
        rejects(&Newtype(value));
        rejects(&vec![Some(Newtype(value))]);
        rejects(&(true, value));
        rejects(&TupleStruct(true, value));
        rejects(&Record {
            value,
            omitted: None,
        });
        rejects(&Variant::Newtype(value));
        rejects(&Variant::Tuple(true, value));
        rejects(&Variant::Struct {
            value,
            omitted: None,
        });
        for separate in [false, true] {
            rejects(&Map {
                key: "number",
                value,
                separate,
            });
            rejects(&Map {
                key: Some(Newtype(value)),
                value: (),
                separate,
            });
        }
    }

    #[test]
    fn non_finite_floats_are_refused_through_every_recursive_entry() {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            rejects_every_shape(value);
        }
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            rejects_every_shape(value);
        }
    }

    fn assert_json_bytes(value: &(impl Serialize + ?Sized)) -> Vec<u8> {
        let mut actual = Vec::new();
        to_writer(&mut actual, value).unwrap();
        assert_eq!(actual, serde_json::to_vec(value).unwrap());
        actual
    }

    #[test]
    fn valid_shapes_keep_serde_json_bytes_and_intentional_nulls() {
        assert_eq!(assert_json_bytes(&()), b"null");
        assert_eq!(assert_json_bytes(&None::<f64>), b"null");
        assert_eq!(assert_json_bytes(&serde_json::Value::Null), b"null");
        assert_eq!(assert_json_bytes(&"NaN"), br#""NaN""#);
        assert_eq!(assert_json_bytes(&Some(-0.0_f32)), b"-0.0");
        assert_eq!(
            assert_json_bytes(&Record {
                value: 1.25,
                omitted: None
            }),
            br#"{"value":1.25}"#
        );
        assert_json_bytes(&Newtype(vec![Some(1.5_f32), None]));
        assert_json_bytes(&TupleStruct(true, -3.5_f64));
        assert_json_bytes(&Variant::<f64>::Unit);
        assert_json_bytes(&Variant::Newtype(1.25_f32));
        assert_json_bytes(&Variant::Tuple(true, 1.25_f64));
        assert_json_bytes(&Variant::Struct {
            value: 1.25_f64,
            omitted: None,
        });
        assert_json_bytes(&(i128::MIN, u128::MAX, '\n', "quoted\"\\text"));
        for separate in [false, true] {
            assert_json_bytes(&Map {
                key: Some(Newtype(1.5_f32)),
                value: f64::MAX,
                separate,
            });
        }
    }

    struct Counting<'a, T> {
        value: T,
        calls: &'a Cell<usize>,
    }

    impl<T: Serialize> Serialize for Counting<'_, T> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            self.calls.set(self.calls.get() + 1);
            self.value.serialize(serializer)
        }
    }

    #[test]
    fn custom_root_map_keys_and_values_each_serialize_once() {
        for separate in [false, true] {
            let root_calls = Cell::new(0);
            let key_calls = Cell::new(0);
            let value_calls = Cell::new(0);
            let value = Counting {
                calls: &root_calls,
                value: Map {
                    key: Counting {
                        value: Newtype("key"),
                        calls: &key_calls,
                    },
                    value: Some(Counting {
                        value: 2.5,
                        calls: &value_calls,
                    }),
                    separate,
                },
            };
            let mut bytes = Vec::new();
            to_writer(&mut bytes, &value).unwrap();
            assert_eq!(bytes, br#"{"key":2.5}"#);
            assert_eq!(
                (root_calls.get(), key_calls.get(), value_calls.get()),
                (1, 1, 1)
            );
        }
    }

    struct SwallowsFloatError;

    impl Serialize for SwallowsFloatError {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut seq = serializer.serialize_seq(Some(1))?;
            let _ = seq.serialize_element(&Some(f64::NAN));
            seq.end()
        }
    }

    #[test]
    fn catching_a_nested_float_error_cannot_turn_rejection_into_success() {
        rejects(&SwallowsFloatError);
        assert_eq!(assert_json_bytes(&Some(1.0_f64)), b"1.0");
    }

    struct Displayed<'a> {
        fragments: &'a Cell<usize>,
    }

    impl fmt::Display for Displayed<'_> {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            for fragment in ["first-\"\\", "second"] {
                self.fragments.set(self.fragments.get() + 1);
                formatter.write_str(fragment)?;
            }
            Ok(())
        }
    }

    impl Serialize for Displayed<'_> {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.collect_str(self)
        }
    }

    #[test]
    fn collect_str_streams_values_and_keys_until_the_writer_fails() {
        let fragments = Cell::new(0);
        let value = Displayed {
            fragments: &fragments,
        };
        assert_eq!(assert_json_bytes(&value), br#""first-\"\\second""#);
        fragments.set(0);
        assert!(to_writer(Cursor::new([0; 4]), &value).unwrap_err().is_io());
        assert_eq!(fragments.get(), 1);

        for separate in [false, true] {
            fragments.set(0);
            let map = Map {
                key: Displayed {
                    fragments: &fragments,
                },
                value: (),
                separate,
            };
            assert!(to_writer(Cursor::new([0; 4]), &map).unwrap_err().is_io());
            assert_eq!(fragments.get(), 1);
        }
    }

    struct HumanReadable;

    impl Serialize for HumanReadable {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            if serializer.is_human_readable() {
                serializer.serialize_str("human")
            } else {
                serializer.serialize_u8(0)
            }
        }
    }

    #[test]
    fn nested_serializers_retain_human_readable_representation() {
        assert_eq!(
            assert_json_bytes(&Some(Newtype(HumanReadable))),
            br#""human""#
        );
    }

    #[derive(Clone, Copy)]
    enum StreamFailure {
        None,
        Custom,
        NonFinite,
    }

    struct Streamed {
        elements: usize,
        size_hint: Option<usize>,
        failure: StreamFailure,
        calls: Cell<usize>,
    }

    impl Serialize for Streamed {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            self.calls.set(self.calls.get() + 1);
            let mut seq = serializer.serialize_seq(self.size_hint)?;
            for _ in 0..self.elements {
                seq.serialize_element(&0_u8)?;
            }
            match self.failure {
                StreamFailure::None => seq.end(),
                StreamFailure::Custom => Err(ser::Error::custom("late refusal")),
                StreamFailure::NonFinite => {
                    seq.serialize_element(&f64::NAN)?;
                    seq.end()
                }
            }
        }
    }

    #[test]
    fn bounded_writer_discards_excess_output_without_reserving_beyond_limit() {
        let payload = Streamed {
            elements: 16_384,
            size_hint: Some(usize::MAX),
            failure: StreamFailure::None,
            calls: Cell::new(0),
        };
        for limit in [0, 1, 31, 256] {
            let mut writer = BoundedWriter::with_capacity(limit, 0);
            assert_eq!(writer.bytes.capacity(), 0);
            to_writer(&mut writer, &payload).expect("streamed zeros serialize");
            assert_eq!(writer.total_bytes, 32_769);
            assert_eq!(writer.bytes.len(), limit);
            assert!(writer.bytes.capacity() <= limit);
        }

        let mut writer = BoundedWriter::with_capacity(1_024, 0);
        to_writer(&mut writer, "x").expect("small string serializes");
        assert_eq!(writer.bytes, br#""x""#);
        assert!(writer.bytes.capacity() < 1_024);

        let chunk = vec![b'x'; 65_536];
        writer
            .write_all(&chunk)
            .expect("oversized chunk is counted without retaining");
        assert_eq!(writer.total_bytes, 65_539);
        assert_eq!(writer.bytes.len(), 1_024);
        assert!(writer.bytes.capacity() <= 1_024);

        let mut overflow = BoundedWriter::with_capacity(8, 0);
        overflow.total_bytes = usize::MAX;
        assert!(overflow.write(&[0]).is_err());
    }

    #[test]
    fn to_vec_bounded_enforces_limit_and_prefers_late_serialization_errors() {
        let small = Streamed {
            elements: 1,
            size_hint: Some(usize::MAX),
            failure: StreamFailure::None,
            calls: Cell::new(0),
        };
        assert_eq!(to_vec_bounded(&small, 3).unwrap(), b"[0]");
        assert_eq!(small.calls.get(), 1);
        std::assert_matches!(
            to_vec_bounded(&small, 2),
            Err(EncodeError::TooLarge { bytes: 3 })
        );

        for failure in [
            StreamFailure::None,
            StreamFailure::Custom,
            StreamFailure::NonFinite,
        ] {
            let payload = Streamed {
                elements: 64,
                size_hint: Some(usize::MAX),
                failure,
                calls: Cell::new(0),
            };
            let err = to_vec_bounded(&payload, 16).unwrap_err();
            match failure {
                StreamFailure::Custom => {
                    let EncodeError::Serialize(error) = err else {
                        panic!("late serialization error must precede TooLarge");
                    };
                    assert_eq!(error.to_string(), "late refusal");
                }
                StreamFailure::NonFinite => {
                    std::assert_matches!(err, EncodeError::Serialize(_));
                }
                StreamFailure::None => {
                    std::assert_matches!(err, EncodeError::TooLarge { bytes: 129 });
                }
            }
            assert_eq!(payload.calls.get(), 1);
        }
    }

    #[test]
    fn to_vec_bounded_with_capacity_preallocates_without_exceeding_the_limit() {
        let value = "a".repeat(3_000);
        let expected_len = value.len() + 2;
        let encoded = to_vec_bounded_with_capacity(&value, 4_096, expected_len).unwrap();
        assert_eq!(encoded.len(), expected_len);
        assert_eq!(encoded.capacity(), expected_len);

        let clamped = to_vec_bounded_with_capacity("x", 16, usize::MAX).unwrap();
        assert_eq!(clamped, br#""x""#);
        assert!(clamped.capacity() <= 16);

        let err = to_vec_bounded_with_capacity(&value, 16, usize::MAX).unwrap_err();
        std::assert_matches!(err, EncodeError::TooLarge { bytes } if bytes == expected_len);
    }
}
