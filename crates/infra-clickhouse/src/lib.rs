mod admission;
mod client;
mod credentials;
mod error;
mod insert;
mod metadata;
mod observe;
mod query;

#[cfg(test)]
mod tests;

pub use clickhouse::Row;
pub use clickhouse::types::{Decimal32, Decimal64, Decimal128};

#[doc(hidden)]
pub mod _priv {
    pub use clickhouse::_priv::RowKind;
}

pub mod serde {
    pub use clickhouse::serde::{ipv4, time, uuid, uuid_vec};
}

pub use self::client::{
    ClickHouse, ClickHouseAuth, ClickHouseOptions, ClientCertificate, CloseOutcome, QuerySettings,
    TransportSecurity,
};
pub use self::error::{InsertError, InsertFailure, QueryError, StartError, UnknownOutcome};
pub use self::insert::{InsertOptions, InsertOutcomes, InsertReport, WriteRow};
pub use self::observe::HISTOGRAMS;
pub use self::query::{Param, Query, ReadRow};
