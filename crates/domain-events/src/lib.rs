use time::UtcDateTime;

pub trait EventPayload {
    const EVENT_TYPE: &'static str;
    const SCHEMA_VERSION: u16;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event<T> {
    pub id: String,
    pub occurred_at: UtcDateTime,
    pub payload: T,
}
