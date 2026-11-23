//! Bespoke data types for use with ClickHouse.

pub use bf16::BFloat16;
pub use decimal::{
    Decimal32, Decimal64, Decimal128, MAX_DECIMAL_SCALE, serialize_decimal_param,
};
pub use int256::{Int256, TryFromInt256Error, TryFromUInt256Error, UInt256};

pub(crate) mod bf16;
pub(crate) mod decimal;
pub(crate) mod int256;
