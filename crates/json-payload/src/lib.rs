use std::{cell::Cell, fmt::Display, io::Write};

use serde::{Serialize, Serializer, ser};

const NON_FINITE: &str = "non-finite float in JSON payload";

/// Keep output private until success: a refusal does not roll back earlier writes.
///
/// # Errors
/// Returns a serialization or writer error, including a non-finite float refusal
/// even when a custom [`Serialize`] implementation catches the nested error.
pub fn to_writer<W: Write, T: Serialize + ?Sized>(
    writer: W,
    value: &T,
) -> Result<(), serde_json::Error> {
    let rejected = Cell::new(false);
    let result = value.serialize(FiniteSerializer {
        inner: &mut serde_json::Serializer::new(writer),
        rejected: &rejected,
    });
    // Custom Serialize implementations can catch a nested serializer error.
    // A caught refusal must still prevent callers from publishing the output.
    if rejected.get() {
        return Err(ser::Error::custom(NON_FINITE));
    }
    result
}

struct FiniteSerializer<'a, S> {
    inner: S,
    rejected: &'a Cell<bool>,
}

struct FiniteValue<'a, T: ?Sized> {
    inner: &'a T,
    rejected: &'a Cell<bool>,
}

impl<T: Serialize + ?Sized> Serialize for FiniteValue<'_, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.inner.serialize(FiniteSerializer {
            inner: serializer,
            rejected: self.rejected,
        })
    }
}

struct Compound<'a, C> {
    inner: C,
    rejected: &'a Cell<bool>,
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
            self.rejected.set(true);
            return Err(ser::Error::custom(NON_FINITE));
        }
        self.inner.serialize_f32(value)
    }

    fn serialize_f64(self, value: f64) -> Result<Self::Ok, Self::Error> {
        if !value.is_finite() {
            self.rejected.set(true);
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
            rejected: self.rejected,
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
                rejected: self.rejected,
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
                rejected: self.rejected,
            },
        )
    }

    fn serialize_seq(self, len: Option<usize>) -> Result<Self::SerializeSeq, Self::Error> {
        Ok(Compound {
            inner: self.inner.serialize_seq(len)?,
            rejected: self.rejected,
        })
    }

    fn serialize_tuple(self, len: usize) -> Result<Self::SerializeTuple, Self::Error> {
        Ok(Compound {
            inner: self.inner.serialize_tuple(len)?,
            rejected: self.rejected,
        })
    }

    fn serialize_tuple_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeTupleStruct, Self::Error> {
        Ok(Compound {
            inner: self.inner.serialize_tuple_struct(name, len)?,
            rejected: self.rejected,
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
            rejected: self.rejected,
        })
    }

    fn serialize_map(self, len: Option<usize>) -> Result<Self::SerializeMap, Self::Error> {
        Ok(Compound {
            inner: self.inner.serialize_map(len)?,
            rejected: self.rejected,
        })
    }

    fn serialize_struct(
        self,
        name: &'static str,
        len: usize,
    ) -> Result<Self::SerializeStruct, Self::Error> {
        Ok(Compound {
            inner: self.inner.serialize_struct(name, len)?,
            rejected: self.rejected,
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
            rejected: self.rejected,
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
                    rejected: self.rejected,
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
            rejected: self.rejected,
        })
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Self::Error> {
        self.inner.serialize_value(&FiniteValue {
            inner: value,
            rejected: self.rejected,
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
                rejected: self.rejected,
            },
            &FiniteValue {
                inner: value,
                rejected: self.rejected,
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
                        rejected: self.rejected,
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
    use std::{cell::Cell, fmt, io::Cursor};

    use serde::{Serialize, Serializer, ser::SerializeMap, ser::SerializeSeq};

    use super::to_writer;

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
}
