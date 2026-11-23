use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{self, Visitor},
};
use std::fmt;

pub const MAX_DECIMAL_SCALE: u8 = 38;
pub(crate) const DECIMAL_PARAM_SERDE_NAME: &str = "__clickhouse_decimal_param";
pub(crate) const DECIMAL32_PREFIX: &str = "__clickhouse_decimal32_";
pub(crate) const DECIMAL64_PREFIX: &str = "__clickhouse_decimal64_";
pub(crate) const DECIMAL128_PREFIX: &str = "__clickhouse_decimal128_";
pub(crate) const DATETIME64_PREFIX: &str = "__clickhouse_datetime64_";
pub(crate) const DATETIME64_SECS_NAME: &str = "__clickhouse_datetime64_0";
pub(crate) const DATETIME64_MILLIS_NAME: &str = "__clickhouse_datetime64_3";
pub(crate) const DATETIME64_MICROS_NAME: &str = "__clickhouse_datetime64_6";
pub(crate) const DATETIME64_NANOS_NAME: &str = "__clickhouse_datetime64_9";

const DECIMAL32_NAMES: [&str; 10] = [
    "__clickhouse_decimal32_0",
    "__clickhouse_decimal32_1",
    "__clickhouse_decimal32_2",
    "__clickhouse_decimal32_3",
    "__clickhouse_decimal32_4",
    "__clickhouse_decimal32_5",
    "__clickhouse_decimal32_6",
    "__clickhouse_decimal32_7",
    "__clickhouse_decimal32_8",
    "__clickhouse_decimal32_9",
];

const DECIMAL64_NAMES: [&str; 19] = [
    "__clickhouse_decimal64_0",
    "__clickhouse_decimal64_1",
    "__clickhouse_decimal64_2",
    "__clickhouse_decimal64_3",
    "__clickhouse_decimal64_4",
    "__clickhouse_decimal64_5",
    "__clickhouse_decimal64_6",
    "__clickhouse_decimal64_7",
    "__clickhouse_decimal64_8",
    "__clickhouse_decimal64_9",
    "__clickhouse_decimal64_10",
    "__clickhouse_decimal64_11",
    "__clickhouse_decimal64_12",
    "__clickhouse_decimal64_13",
    "__clickhouse_decimal64_14",
    "__clickhouse_decimal64_15",
    "__clickhouse_decimal64_16",
    "__clickhouse_decimal64_17",
    "__clickhouse_decimal64_18",
];

const DECIMAL128_NAMES: [&str; 39] = [
    "__clickhouse_decimal128_0",
    "__clickhouse_decimal128_1",
    "__clickhouse_decimal128_2",
    "__clickhouse_decimal128_3",
    "__clickhouse_decimal128_4",
    "__clickhouse_decimal128_5",
    "__clickhouse_decimal128_6",
    "__clickhouse_decimal128_7",
    "__clickhouse_decimal128_8",
    "__clickhouse_decimal128_9",
    "__clickhouse_decimal128_10",
    "__clickhouse_decimal128_11",
    "__clickhouse_decimal128_12",
    "__clickhouse_decimal128_13",
    "__clickhouse_decimal128_14",
    "__clickhouse_decimal128_15",
    "__clickhouse_decimal128_16",
    "__clickhouse_decimal128_17",
    "__clickhouse_decimal128_18",
    "__clickhouse_decimal128_19",
    "__clickhouse_decimal128_20",
    "__clickhouse_decimal128_21",
    "__clickhouse_decimal128_22",
    "__clickhouse_decimal128_23",
    "__clickhouse_decimal128_24",
    "__clickhouse_decimal128_25",
    "__clickhouse_decimal128_26",
    "__clickhouse_decimal128_27",
    "__clickhouse_decimal128_28",
    "__clickhouse_decimal128_29",
    "__clickhouse_decimal128_30",
    "__clickhouse_decimal128_31",
    "__clickhouse_decimal128_32",
    "__clickhouse_decimal128_33",
    "__clickhouse_decimal128_34",
    "__clickhouse_decimal128_35",
    "__clickhouse_decimal128_36",
    "__clickhouse_decimal128_37",
    "__clickhouse_decimal128_38",
];

macro_rules! define_scaled_decimal {
    ($name:ident, $inner:ty, $max_scale:literal, $names:ident, $doc:literal) => {
        #[doc = $doc]
        #[repr(transparent)]
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name<const SCALE: u8>($inner);

        impl<const SCALE: u8> Default for $name<SCALE> {
            fn default() -> Self {
                Self::new(0)
            }
        }

        impl<const SCALE: u8> $name<SCALE> {
            const VALID_SCALE: () = assert!(
                SCALE <= $max_scale,
                concat!(stringify!($name), " scale exceeds ClickHouse maximum")
            );

            #[must_use]
            pub const fn new(unscaled: $inner) -> Self {
                let () = Self::VALID_SCALE;
                Self(unscaled)
            }

            #[must_use]
            pub const fn unscaled(self) -> $inner {
                let () = Self::VALID_SCALE;
                self.0
            }

            #[must_use]
            pub const fn scale(self) -> u8 {
                let () = Self::VALID_SCALE;
                SCALE
            }

            const fn serde_name() -> &'static str {
                let () = Self::VALID_SCALE;
                $names[SCALE as usize]
            }
        }

        impl<const SCALE: u8> From<$inner> for $name<SCALE> {
            fn from(unscaled: $inner) -> Self {
                Self::new(unscaled)
            }
        }

        impl<const SCALE: u8> From<$name<SCALE>> for $inner {
            fn from(value: $name<SCALE>) -> Self {
                value.0
            }
        }

        impl<const SCALE: u8> Serialize for $name<SCALE> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let () = Self::VALID_SCALE;
                if serializer.is_human_readable() {
                    serialize_decimal_param(i128::from(self.0), SCALE, serializer)
                } else {
                    serializer.serialize_newtype_struct(Self::serde_name(), &self.0)
                }
            }
        }

        impl<'de, const SCALE: u8> Deserialize<'de> for $name<SCALE> {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct NewtypeVisitor<const S: u8>;

                impl<'de, const S: u8> Visitor<'de> for NewtypeVisitor<S> {
                    type Value = $name<S>;

                    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                        write!(formatter, "{}({S})", stringify!($name))
                    }

                    fn visit_newtype_struct<D: Deserializer<'de>>(
                        self,
                        deserializer: D,
                    ) -> Result<Self::Value, D::Error> {
                        let raw = <$inner as Deserialize>::deserialize(deserializer)?;
                        Ok($name::new(raw))
                    }
                }

                deserializer.deserialize_newtype_struct(Self::serde_name(), NewtypeVisitor::<SCALE>)
            }
        }

        impl<const SCALE: u8> crate::row::Primitive for $name<SCALE> {}

        impl<const SCALE: u8> fmt::Display for $name<SCALE> {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let () = Self::VALID_SCALE;
                format_scaled_i128(f, i128::from(self.0), SCALE)
            }
        }
    };
}

define_scaled_decimal!(
    Decimal32,
    i32,
    9,
    DECIMAL32_NAMES,
    "Scale-validated ClickHouse `Decimal32(S)` value stored as a scaled `i32`."
);

define_scaled_decimal!(
    Decimal64,
    i64,
    18,
    DECIMAL64_NAMES,
    "Scale-validated ClickHouse `Decimal64(S)` value stored as a scaled `i64`."
);

define_scaled_decimal!(
    Decimal128,
    i128,
    38,
    DECIMAL128_NAMES,
    "Scale-validated ClickHouse `Decimal128(S)` value stored as a scaled `i128`."
);

struct FormattedDecimal {
    unscaled: i128,
    scale: u8,
}

impl fmt::Display for FormattedDecimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        format_scaled_i128(f, self.unscaled, self.scale)
    }
}

impl Serialize for FormattedDecimal {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

pub fn serialize_decimal_param<S: Serializer>(
    unscaled: i128,
    scale: u8,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    if scale > MAX_DECIMAL_SCALE {
        return Err(serde::ser::Error::custom(format!(
            "decimal scale {scale} exceeds ClickHouse maximum {MAX_DECIMAL_SCALE}"
        )));
    }
    serializer.serialize_newtype_struct(DECIMAL_PARAM_SERDE_NAME, &FormattedDecimal { unscaled, scale })
}

pub(crate) fn format_scaled_i128(
    f: &mut fmt::Formatter<'_>,
    unscaled: i128,
    scale: u8,
) -> fmt::Result {
    if scale == 0 {
        return write!(f, "{unscaled}");
    }
    let sign = if unscaled < 0 { "-" } else { "" };
    let abs = unscaled.unsigned_abs();
    let divisor = 10_u128.pow(u32::from(scale));
    let whole = abs / divisor;
    let frac = abs % divisor;
    write!(f, "{sign}{whole}.{frac:0width$}", width = usize::from(scale))
}

pub(crate) struct DateTime64Wire {
    pub(crate) name: &'static str,
    pub(crate) ticks: i64,
}

impl Serialize for DateTime64Wire {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_newtype_struct(self.name, &self.ticks)
    }
}

pub(crate) fn deserialize_datetime64_ticks<'de, D: Deserializer<'de>>(
    deserializer: D,
    name: &'static str,
) -> Result<i64, D::Error> {
    struct TicksVisitor(&'static str);

    impl<'de> Visitor<'de> for TicksVisitor {
        type Value = i64;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(self.0)
        }

        fn visit_newtype_struct<D: Deserializer<'de>>(
            self,
            deserializer: D,
        ) -> Result<Self::Value, D::Error> {
            i64::deserialize(deserializer)
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
            Ok(v)
        }
    }

    deserializer.deserialize_newtype_struct(name, TicksVisitor(name))
}
