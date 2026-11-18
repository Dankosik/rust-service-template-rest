#![expect(
    clippy::expect_used,
    clippy::panic,
    reason = "tests must fail with fixture context when generated Go records or the production wire boundary violate the contract"
)]

use std::collections::BTreeMap;
use std::env;
use std::fs;

use async_nats::HeaderMap;
use bytes::Bytes;
use domain_events::{Event, EventPayload};
use infra_messaging::wire::{
    CREATED_AT, DEAD_LETTER_REASON, DeadLetterRecord, NATS_MSG_ID, dead_letter_id, decode_envelope,
    encode_prepared, restore_dead_letter,
};
use infra_messaging::{Registry, Route};
use serde::Deserialize;
use serde_with::base64::Base64;
use serde_with::serde_as;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use time::macros::utc_datetime;

const FIXTURE_PATH_ENV: &str = "GO_WIRE_FIXTURES";
const EXPORT_PATH_ENV: &str = "GO_WIRE_RUST_EXPORT";
const MAX_PAYLOAD_BYTES: usize = 8 * 1024;

#[derive(Debug, Deserialize)]
struct FixtureSet {
    version: u8,
    cases: Vec<FixtureCase>,
}

#[derive(Debug, Deserialize)]
struct FixtureCase {
    name: String,
    inbound: WireRecord,
    decoded: EventRecord,
    reencoded: WireRecord,
    reencoded_go_decodes: bool,
    dlq: WireRecord,
    restored: EventRecord,
}

#[serde_as]
#[derive(Debug, Deserialize)]
struct EventRecord {
    subject: String,
    message_id: String,
    publication_id: String,
    event_type: String,
    schema: String,
    created_at: String,
    unix_seconds: i64,
    nanosecond: u32,
    #[serde(rename = "body_base64")]
    #[serde_as(as = "Base64")]
    body: Vec<u8>,
}

impl EventRecord {
    fn occurred_at(&self) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp_nanos(
            i128::from(self.unix_seconds) * 1_000_000_000 + i128::from(self.nanosecond),
        )
        .expect("fixture instant is representable")
    }
}

#[serde_as]
#[derive(Debug, Deserialize, serde::Serialize)]
struct WireRecord {
    subject: String,
    headers: BTreeMap<String, String>,
    #[serde(rename = "body_base64")]
    #[serde_as(as = "Base64")]
    body: Vec<u8>,
    stream: String,
    stream_sequence: u64,
    stored_at: String,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct BridgePayload {
    amount: f64,
    escaped: String,
    items: Vec<bool>,
}

impl EventPayload for BridgePayload {
    const EVENT_TYPE: &'static str = "example.created";
    const SCHEMA_VERSION: u16 = 1;
}

#[test]
#[ignore = "CI generates GO_WIRE_FIXTURES with the pinned actual Go package"]
#[expect(
    clippy::disallowed_methods,
    reason = "wire fixture owns finite output-file exchange with its Go consumer"
)]
fn rust_production_wire_exports_for_go() {
    let fixtures = fixtures();
    assert_eq!(fixtures.version, 1, "unsupported Go-wire fixture version");
    for name in ["fractional_raw_bytes", "identity_schema_boundary"] {
        assert!(
            fixtures.cases.iter().any(|fixture| fixture.name == name),
            "fixture must retain the {name} actual-Go observation"
        );
    }

    for fixture in &fixtures.cases {
        verify_go_record_in_rust(fixture);
    }
    verify_production_limits(&fixtures);

    let records = rust_production_records();
    let export_path = env::var(EXPORT_PATH_ENV)
        .expect("CI must provide GO_WIRE_RUST_EXPORT for actual Go decode");
    fs::write(
        export_path,
        serde_json::to_vec_pretty(&records).expect("Rust records serialize"),
    )
    .expect("Rust production records write");
}

#[expect(
    clippy::disallowed_methods,
    reason = "wire fixture owns finite input-file loading before decoding"
)]
fn fixtures() -> FixtureSet {
    let path = env::var(FIXTURE_PATH_ENV)
        .expect("CI must generate GO_WIRE_FIXTURES before this test runs");
    serde_json::from_slice(&fs::read(path).expect("Go fixture reads"))
        .expect("Go fixture has the bridge record schema")
}

fn verify_go_record_in_rust(fixture: &FixtureCase) {
    verify_inbound_record(fixture);
    verify_reencoded_record(fixture);
    verify_dead_letter_round_trip(fixture);
}

fn verify_inbound_record(fixture: &FixtureCase) {
    let inbound_headers = headers(&fixture.inbound.headers);
    let inbound_body = Bytes::from(fixture.inbound.body.clone());
    let decoded = decode_envelope(&fixture.inbound.subject, &inbound_headers, inbound_body)
        .unwrap_or_else(|error| panic!("{}: Go record rejected by Rust: {error}", fixture.name));

    assert_eq!(
        decoded.message_id(),
        fixture.decoded.message_id,
        "{} message ID",
        fixture.name
    );
    assert_eq!(
        decoded.event_type(),
        fixture.decoded.event_type,
        "{} event type",
        fixture.name
    );
    assert_eq!(
        decoded.schema_version(),
        schema_version(&fixture.decoded.schema),
        "{} schema",
        fixture.name
    );
    assert_eq!(
        decoded.occurred_at(),
        fixture.decoded.occurred_at(),
        "{} UTC instant",
        fixture.name
    );
    assert_eq!(
        decoded.payload().as_ref(),
        fixture.decoded.body.as_slice(),
        "{} raw payload",
        fixture.name
    );
    assert_eq!(
        fixture.inbound.subject, fixture.decoded.subject,
        "{} subject",
        fixture.name
    );
    assert_eq!(
        fixture.inbound.headers.get(NATS_MSG_ID),
        Some(&fixture.decoded.publication_id),
        "{} publication identity",
        fixture.name
    );
}

fn verify_reencoded_record(fixture: &FixtureCase) {
    assert_eq!(
        fixture.reencoded.headers.get(CREATED_AT),
        Some(&fixture.decoded.created_at),
        "{} Go re-emitted timestamp text",
        fixture.name
    );
    if fixture.reencoded_go_decodes {
        let reencoded = decode_envelope(
            &fixture.reencoded.subject,
            &headers(&fixture.reencoded.headers),
            Bytes::from(fixture.reencoded.body.clone()),
        )
        .unwrap_or_else(|error| {
            panic!(
                "{}: Go-roundtrippable re-emission rejected by Rust: {error}",
                fixture.name
            )
        });
        assert_eq!(
            reencoded.occurred_at(),
            fixture.decoded.occurred_at(),
            "{} re-emitted UTC instant",
            fixture.name
        );
        assert_eq!(
            reencoded.payload().as_ref(),
            fixture.decoded.body.as_slice(),
            "{} re-emitted raw payload",
            fixture.name
        );
    }
}

fn verify_dead_letter_round_trip(fixture: &FixtureCase) {
    let source_publication_id = fixture
        .inbound
        .headers
        .get(NATS_MSG_ID)
        .expect("generated Go fixture has Nats-Msg-Id");
    let expected_transfer = dead_letter_id(
        &fixture.inbound.stream,
        fixture.inbound.stream_sequence,
        parse_timestamp(&fixture.inbound.stored_at),
        source_publication_id,
    )
    .unwrap_or_else(|error| panic!("{}: Rust DLQ id rejected Go source: {error}", fixture.name));
    assert_eq!(
        fixture.dlq.headers.get(NATS_MSG_ID),
        Some(&expected_transfer),
        "{} DLQ transfer hash",
        fixture.name
    );
    assert_eq!(
        fixture
            .dlq
            .headers
            .get(DEAD_LETTER_REASON)
            .map(String::as_str),
        Some("exhausted"),
        "{} DLQ reason",
        fixture.name
    );

    let restored = restore_dead_letter(DeadLetterRecord {
        headers: headers(&fixture.dlq.headers),
        payload: Bytes::from(fixture.dlq.body.clone()),
        stream: fixture.dlq.stream.clone(),
        stream_sequence: fixture.dlq.stream_sequence,
        stored_at: parse_timestamp(&fixture.dlq.stored_at),
    })
    .unwrap_or_else(|error| panic!("{}: Go DLQ record rejected by Rust: {error}", fixture.name));
    assert_eq!(
        restored.subject(),
        fixture.restored.subject,
        "{} restored subject",
        fixture.name
    );
    assert_eq!(
        restored.message_id(),
        fixture.restored.message_id,
        "{} restored message ID",
        fixture.name
    );
    assert_eq!(
        restored.publication_id(),
        fixture.restored.publication_id,
        "{} redrive hash",
        fixture.name
    );
    assert_eq!(
        restored.event_type(),
        fixture.restored.event_type,
        "{} restored type",
        fixture.name
    );
    assert_eq!(
        restored.schema_label(),
        fixture.restored.schema,
        "{} restored schema",
        fixture.name
    );
    assert_eq!(
        restored.occurred_at(),
        fixture.restored.occurred_at(),
        "{} restored UTC instant",
        fixture.name
    );
    assert_eq!(
        fixture.restored.created_at, fixture.decoded.created_at,
        "{} Go-restored timestamp text",
        fixture.name
    );
    assert_eq!(
        restored.payload().as_ref(),
        fixture.restored.body.as_slice(),
        "{} restored raw payload",
        fixture.name
    );
}

fn verify_production_limits(fixtures: &FixtureSet) {
    let boundary = fixtures
        .cases
        .iter()
        .find(|fixture| fixture.name == "identity_schema_boundary")
        .expect("generated Go fixture retains identity/schema boundary");
    let decoded = decode_envelope(
        &boundary.inbound.subject,
        &headers(&boundary.inbound.headers),
        Bytes::from(boundary.inbound.body.clone()),
    )
    .expect("Go's 256-byte identity and v65535 remain admitted");
    assert_eq!(decoded.message_id().len(), 256);
    assert_eq!(decoded.event_type().len(), 256);
    assert_eq!(decoded.schema_version(), u16::MAX);

    let route = Route::<BridgePayload>::new("events.created");
    let mut registry = Registry::default();
    let publication = registry
        .declare_publication(&route, MAX_PAYLOAD_BYTES)
        .expect("bridge route admits");
    let small = Registry::default()
        .declare_publication(&route, 1)
        .expect("bridge route admits the one-byte payload limit");
    let event = bridge_event("rust-payload-limit");
    assert!(
        small.prepare(&event).is_err(),
        "production preparation enforces payload admission"
    );
    assert!(
        publication.prepare(&bridge_event("x".repeat(257))).is_err(),
        "over-limit Go identity must reject before encoding"
    );

    let mut oversized_headers = headers(&fixtures.cases[0].inbound.headers);
    oversized_headers.insert("X-Go-Wire-Limit", "x".repeat(MAX_PAYLOAD_BYTES));
    assert!(
        decode_envelope(
            &fixtures.cases[0].inbound.subject,
            &oversized_headers,
            Bytes::from(fixtures.cases[0].inbound.body.clone()),
        )
        .is_err(),
        "production decoding enforces the 8 KiB header bound"
    );
}

fn rust_production_records() -> Vec<WireRecord> {
    let route = Route::<BridgePayload>::new("events.rust");
    let mut registry = Registry::default();
    let publication = registry
        .declare_publication(&route, MAX_PAYLOAD_BYTES)
        .expect("Rust export route admits");
    let event = bridge_event("rust-go-compat-1");
    let prepared = publication
        .prepare(&event)
        .expect("production Rust preparation admits");
    let headers = encode_prepared(&prepared).expect("production Rust encoding admits");
    vec![WireRecord {
        subject: prepared.subject().to_owned(),
        headers: header_values(&headers),
        body: prepared.payload().to_vec(),
        stream: "RUST_EXPORTS".to_owned(),
        stream_sequence: 1,
        stored_at: "2026-09-26T12:37:00.000000003Z".to_owned(),
    }]
}

fn bridge_event(id: impl Into<String>) -> Event<BridgePayload> {
    Event {
        id: id.into(),
        occurred_at: utc_datetime!(2026-09-26 12:34:56.123456789),
        payload: BridgePayload {
            amount: 1.0,
            escaped: "\\u0041".to_owned(),
            items: vec![true, false],
        },
    }
}

fn headers(values: &BTreeMap<String, String>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in values {
        headers.insert(name.as_str(), value.as_str());
    }
    headers
}

fn header_values(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .map(|(name, values)| {
            assert_eq!(
                values.len(),
                1,
                "production normal envelope has one value per header"
            );
            (name.to_string(), values[0].to_string())
        })
        .collect()
}

fn parse_timestamp(value: &str) -> OffsetDateTime {
    OffsetDateTime::parse(value, &Rfc3339).expect("fixture timestamp is RFC3339")
}

fn schema_version(value: &str) -> u16 {
    value
        .strip_prefix('v')
        .expect("fixture schema begins with v")
        .parse()
        .expect("fixture schema has decimal version")
}
