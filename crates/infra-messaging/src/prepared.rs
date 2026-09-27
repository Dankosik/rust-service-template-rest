use bytes::Bytes;
use domain_events::{Event, EventPayload};
use time::OffsetDateTime;

/// Serialized, routed event intent. Retrying never reserializes or reroutes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreparedEvent {
    pub(crate) subject: String,
    pub(crate) message_id: String,
    pub(crate) publication_id: String,
    pub(crate) event_type: String,
    pub(crate) schema_version: u16,
    pub(crate) occurred_at: OffsetDateTime,
    pub(crate) payload: Bytes,
}

impl PreparedEvent {
    /// Serializes one typed occurrence without connecting to a broker.
    ///
    /// This is shared by direct publication, outbox storage and the
    /// broker-independent Go compatibility proof.
    ///
    /// # Errors
    /// Rejects an invalid subject, unserializable payload or exceeded wire bound.
    pub fn prepare<T: EventPayload>(
        subject: impl Into<String>,
        event: &Event<T>,
        max_payload_bytes: usize,
    ) -> Result<Self, crate::MessagingError> {
        let subject = subject.into();
        if !crate::wire::valid_subject(&subject) {
            return Err(crate::MessagingError::Envelope("subject is invalid"));
        }
        let payload = serde_json::to_vec(event.payload())
            .map_err(|_| crate::MessagingError::Envelope("event payload cannot be serialized"))?;
        if payload.len() > max_payload_bytes {
            return Err(crate::MessagingError::Envelope(
                "payload exceeds configured maximum",
            ));
        }
        let prepared = Self {
            subject,
            message_id: event.id().to_owned(),
            publication_id: event.id().to_owned(),
            event_type: T::EVENT_TYPE.to_owned(),
            schema_version: T::SCHEMA_VERSION,
            occurred_at: event.occurred_at(),
            payload: payload.into(),
        };
        let headers = crate::wire::encode_prepared(&prepared)?;
        crate::wire::validate_encoded_message(
            &prepared.subject,
            &headers,
            prepared.payload.len(),
            max_payload_bytes,
        )?;
        Ok(prepared)
    }
    #[must_use]
    pub fn subject(&self) -> &str {
        &self.subject
    }
    #[must_use]
    pub fn message_id(&self) -> &str {
        &self.message_id
    }
    #[must_use]
    pub fn publication_id(&self) -> &str {
        &self.publication_id
    }
    #[must_use]
    pub fn event_type(&self) -> &str {
        &self.event_type
    }
    #[must_use]
    pub const fn schema_version(&self) -> u16 {
        self.schema_version
    }
    #[must_use]
    pub fn schema(&self) -> String {
        format!("v{}", self.schema_version)
    }
    #[must_use]
    pub const fn occurred_at(&self) -> OffsetDateTime {
        self.occurred_at
    }
    #[must_use]
    pub fn payload(&self) -> &Bytes {
        &self.payload
    }
}

/// Confirmed broker publication acknowledgment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishAck {
    pub stream: String,
    pub sequence: u64,
    pub duplicate: bool,
}
