#!/usr/bin/env bash
# Generate and verify Go wire records with the pinned Go source.
#
# The bridge is deliberately a package test added only to a temporary checkout:
# package natsjs owns buildNATSMessage/decodeMessage, while domainevent owns the
# typed event construction it feeds.  It adapts JSON records to those APIs; it
# does not duplicate their wire validation or transfer-id implementation.
set -euo pipefail

readonly source_url="${GO_WIRE_SOURCE_URL:-https://github.com/Dankosik/go-service-template-rest.git}"
readonly source_revision="a50f43cf869b86accb3fdba24d758cc2d15a02df"
readonly wire_hash="c98d7c1db43f515e072e91416cf1baa6d0da5c898337d62be9fef3173ed9fe88"
readonly dead_letter_hash="4063580a0322974331c13f3de19e3296b0c40d1785cbbf98d695dc74ef5b953c"
readonly event_hash="add1371aa3ef73b26a9f1eb08d9b992f48969d4a8f6dbd256d81d6d1a058e556"

usage() {
  printf '%s\n' "usage: $0 generate|check|verify-rust [fixture-file] [rust-export-file]" >&2
  exit 64
}

mode="${1:-}"
fixture_file="${2:-crates/infra-messaging/tests/fixtures/go-wire/records.json}"
rust_export_file="${3:-}"
case "$mode" in
  generate|check) ;;
  verify-rust) [[ -n "$rust_export_file" ]] || usage ;;
  *) usage ;;
esac

fixture_dir="$(dirname "$fixture_file")"
mkdir -p "$fixture_dir"
fixture_file="$(cd "$fixture_dir" && pwd)/$(basename "$fixture_file")"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/go-wire.XXXXXX")"
cleanup() { rm -rf "$scratch"; }
trap cleanup EXIT

git clone --quiet --filter=blob:none --no-checkout "$source_url" "$scratch/source"
git -C "$scratch/source" checkout --quiet --detach "$source_revision"
[[ "$(git -C "$scratch/source" rev-parse HEAD)" == "$source_revision" ]] || {
  printf '%s\n' 'pinned Go source revision did not resolve' >&2
  exit 1
}

verify_hash() {
  local expected="$1" path="$2" actual
  if command -v shasum >/dev/null 2>&1; then
    actual="$(shasum -a 256 "$scratch/source/$path" | awk '{print $1}')"
  else
    actual="$(sha256sum "$scratch/source/$path" | awk '{print $1}')"
  fi
  [[ "$actual" == "$expected" ]] || {
    printf 'Go source hash mismatch for %s: got %s, want %s\n' "$path" "$actual" "$expected" >&2
    exit 1
  }
}
verify_hash "$wire_hash" internal/infra/natsjs/message_wire.go
verify_hash "$dead_letter_hash" internal/infra/natsjs/message_deadletter.go
verify_hash "$event_hash" internal/domainevent/event.go

bridge="$scratch/source/internal/infra/natsjs/go_wire_compat_bridge_test.go"
cat >"$bridge" <<'GO_BRIDGE'
package natsjs

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"os"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/example/go-service-template-rest/internal/domainevent"
	"github.com/nats-io/nats.go"
	"github.com/nats-io/nats.go/jetstream"
)

const compatFixtureVersion = 1

type compatFixtureSet struct {
	Version    int                 `json:"version"`
	Provenance compatProvenance    `json:"provenance"`
	Cases      []compatFixtureCase `json:"cases"`
}

type compatProvenance struct {
	Repository string            `json:"repository"`
	Revision   string            `json:"revision"`
	Files      map[string]string `json:"files"`
}

type compatFixtureCase struct {
	Name               string             `json:"name"`
	Event              compatEvent        `json:"event"`
	Encoded            compatRecord       `json:"encoded"`
	Inbound            compatRecord       `json:"inbound"`
	Decoded            compatDecoded      `json:"decoded"`
	Reencoded          compatRecord       `json:"reencoded"`
	ReencodedGoDecodes bool               `json:"reencoded_go_decodes"`
	DLQ                compatRecord       `json:"dlq"`
	Restored           compatEvent        `json:"restored"`
}

type compatEvent struct {
	Subject       string `json:"subject"`
	MessageID     string `json:"message_id"`
	PublicationID string `json:"publication_id"`
	Type          string `json:"event_type"`
	Schema        string `json:"schema"`
	CreatedAt     string `json:"created_at"`
	UnixSeconds   int64  `json:"unix_seconds"`
	Nanosecond    int    `json:"nanosecond"`
	BodyBase64    string `json:"body_base64"`
}

type compatRecord struct {
	Subject       string            `json:"subject"`
	Headers       map[string]string `json:"headers"`
	BodyBase64    string            `json:"body_base64"`
	Stream        string            `json:"stream"`
	StreamSequence uint64           `json:"stream_sequence"`
	StoredAt      string            `json:"stored_at"`
}

type compatDecoded struct {
	Subject       string `json:"subject"`
	MessageID     string `json:"message_id"`
	PublicationID string `json:"publication_id"`
	Type          string `json:"event_type"`
	Schema        string `json:"schema"`
	CreatedAt     string `json:"created_at"`
	UnixSeconds   int64  `json:"unix_seconds"`
	Nanosecond    int    `json:"nanosecond"`
	BodyBase64    string `json:"body_base64"`
}

type compatMsg struct {
	subject string
	header nats.Header
	data    []byte
	meta    *jetstream.MsgMetadata
}

func (m *compatMsg) Metadata() (*jetstream.MsgMetadata, error) { return m.meta, nil }
func (m *compatMsg) Data() []byte                              { return m.data }
func (m *compatMsg) Headers() nats.Header                      { return m.header }
func (m *compatMsg) Subject() string                           { return m.subject }
func (m *compatMsg) Reply() string                             { return "" }
func (m *compatMsg) Ack() error                                { return nil }
func (m *compatMsg) DoubleAck(context.Context) error           { return nil }
func (m *compatMsg) Nak() error                                { return nil }
func (m *compatMsg) NakWithDelay(time.Duration) error          { return nil }
func (m *compatMsg) InProgress() error                         { return nil }
func (m *compatMsg) Term() error                               { return nil }
func (m *compatMsg) TermWithReason(string) error               { return nil }

func TestGoWireCompatibilityBridge(t *testing.T) {
	fixturePath := os.Getenv("GO_WIRE_FIXTURES")
	if fixturePath == "" { t.Fatal("GO_WIRE_FIXTURES is required") }
	mode := os.Getenv("GO_WIRE_MODE")
	if mode == "generate" {
		verifyLimits(t)
		set := generatedFixtures(t)
		writeJSON(t, fixturePath, set)
		return
	}
	set := readFixtures(t, fixturePath)
	if set.Version != compatFixtureVersion { t.Fatalf("fixture version = %d", set.Version) }
	verifyProvenance(t, set.Provenance)
	for _, fixture := range set.Cases { verifyFixture(t, fixture) }
	verifyLimits(t)
	if mode == "verify-rust" { verifyRustExport(t, os.Getenv("GO_WIRE_RUST_EXPORT")) }
}

func generatedFixtures(t *testing.T) compatFixtureSet {
	t.Helper()
	return compatFixtureSet{
		Version: compatFixtureVersion,
		Provenance: compatProvenance{
			Repository: "https://github.com/Dankosik/go-service-template-rest",
			Revision: "a50f43cf869b86accb3fdba24d758cc2d15a02df",
			Files: map[string]string{
				"internal/infra/natsjs/message_wire.go": "c98d7c1db43f515e072e91416cf1baa6d0da5c898337d62be9fef3173ed9fe88",
				"internal/infra/natsjs/message_deadletter.go": "4063580a0322974331c13f3de19e3296b0c40d1785cbbf98d695dc74ef5b953c",
				"internal/domainevent/event.go": "add1371aa3ef73b26a9f1eb08d9b992f48969d4a8f6dbd256d81d6d1a058e556",
			},
		},
		Cases: []compatFixtureCase{
			generateFixture(t, "fractional_raw_bytes", "events.created", "message-fraction", "publication-fraction", "example.created", 1, "2026-09-26T12:34:56.123456789Z", []byte(`{"amount":1.00,"escaped":"\\u0041","items":[true,false]}`), "2026-09-26T18:04:56.123456789+05:30"),
			generateFixture(t, "identity_schema_boundary", "events.boundary", strings.Repeat("m", 256), strings.Repeat("p", 256), strings.Repeat("t", 256), 65535, "2026-09-26T12:34:56Z", []byte(`{"raw": "spacing is preserved"}`), "2026-09-26T12:34:56.5+00:00"),
			generateFixture(t, "go_permissive_fraction_offset", "events.timestamp", "message-permissive", "publication-permissive", "example.timestamp", 1, "2026-09-26T12:34:56Z", []byte(`{"source":"Go RFC3339Nano parser"}`), "2026-01-02T3:04:05,12345678912+24:00"),
			generateFixture(t, "go_offset_minute_sixty", "events.timestamp", "message-offset-minute", "publication-offset-minute", "example.timestamp", 1, "2026-09-26T12:34:56Z", []byte(`{"source":"Go RFC3339Nano parser"}`), "2026-01-02T03:04:05+00:60"),
			generateFixture(t, "go_utc_year_boundary", "events.timestamp", "message-year-boundary", "publication-year-boundary", "example.timestamp", 1, "2026-09-26T12:34:56Z", []byte(`{"source":"Go RFC3339Nano parser"}`), "9999-12-31T23:59:59-01:00"),
		},
	}
}

func generateFixture(t *testing.T, name, subject, messageID, publicationID, eventType string, version uint16, occurredAt string, payload []byte, inboundCreatedAt string) compatFixtureCase {
	t.Helper()
	createdAt, err := time.Parse(time.RFC3339Nano, occurredAt)
	if err != nil { t.Fatal(err) }
	domain := domainevent.Event{ID: messageID, Type: eventType, Version: version, OccurredAt: createdAt, Payload: json.RawMessage(payload)}
	if err := domain.Validate(); err != nil { t.Fatalf("domain validation: %v", err) }
	event := EventFromDomain(subject, domain)
	event.PublicationID = publicationID
	encoded, err := buildNATSMessage(context.Background(), event, len(payload))
	if err != nil { t.Fatalf("buildNATSMessage: %v", err) }
	inbound := cloneRecord(encoded, "EVENTS", 17, "2026-09-26T12:35:00.000000001Z")
	inbound.Headers[headerCreatedAt] = inboundCreatedAt
	meta := recordMetadata(t, inbound)
	decoded, _, err := decodeMessage(recordMsg(t, inbound, meta), meta)
	if err != nil { t.Fatalf("decodeMessage: %v", err) }
	reencoded, err := buildNATSMessage(context.Background(), Event{
		Subject: decoded.subject, MessageID: decoded.messageID, PublicationID: decoded.publicationID,
		Type: decoded.eventType, Schema: decoded.schema, CreatedAt: decoded.createdAt, Payload: decoded.payload,
	}, len(decoded.payload))
	if err != nil { t.Fatalf("re-emit buildNATSMessage: %v", err) }
	_, _, reencodedDecodeErr := decodeMessage(recordMsg(t, cloneRecord(reencoded, "EVENTS", 17, "2026-09-26T12:35:00.000000001Z"), meta), meta)
	dlq, _ := deadLetterMessage(recordMsg(t, inbound, meta), meta, decoded, deadLetterExhausted)
	dlqRecord := cloneRecord(dlq, "EVENTS_DLQ", 29, "2026-09-26T12:36:00.000000002Z")
	restored, err := RestoreDeadLetter(recordMsg(t, dlqRecord, recordMetadata(t, dlqRecord)))
	if err != nil { t.Fatalf("RestoreDeadLetter: %v", err) }
	return compatFixtureCase{
		Name: name,
		Event: eventRecord(event),
		Encoded: cloneRecord(encoded, "", 0, ""),
		Inbound: inbound,
		Decoded: decodedRecord(decoded),
		Reencoded: cloneRecord(reencoded, "", 0, ""),
		ReencodedGoDecodes: reencodedDecodeErr == nil,
		DLQ: dlqRecord,
		Restored: eventRecord(restored),
	}
}

func verifyFixture(t *testing.T, fixture compatFixtureCase) {
	t.Helper()
	event := eventFromRecord(t, fixture.Event)
	encoded, err := buildNATSMessage(context.Background(), event, len(event.Payload))
	if err != nil { t.Fatalf("%s buildNATSMessage: %v", fixture.Name, err) }
	if got := cloneRecord(encoded, "", 0, ""); !reflect.DeepEqual(got, fixture.Encoded) { t.Fatalf("%s encoded Go record drifted\n got %#v\nwant %#v", fixture.Name, got, fixture.Encoded) }
	inboundMeta := recordMetadata(t, fixture.Inbound)
	decoded, _, err := decodeMessage(recordMsg(t, fixture.Inbound, inboundMeta), inboundMeta)
	if err != nil { t.Fatalf("%s decodeMessage: %v", fixture.Name, err) }
	if got := decodedRecord(decoded); !reflect.DeepEqual(got, fixture.Decoded) { t.Fatalf("%s decoded Go record drifted\n got %#v\nwant %#v", fixture.Name, got, fixture.Decoded) }
	reencoded, err := buildNATSMessage(context.Background(), Event{
		Subject: decoded.subject, MessageID: decoded.messageID, PublicationID: decoded.publicationID,
		Type: decoded.eventType, Schema: decoded.schema, CreatedAt: decoded.createdAt, Payload: decoded.payload,
	}, len(decoded.payload))
	if err != nil { t.Fatalf("%s re-emit buildNATSMessage: %v", fixture.Name, err) }
	if got := cloneRecord(reencoded, "", 0, ""); !reflect.DeepEqual(got, fixture.Reencoded) { t.Fatalf("%s reencoded Go record drifted\n got %#v\nwant %#v", fixture.Name, got, fixture.Reencoded) }
	_, _, reencodedDecodeErr := decodeMessage(recordMsg(t, cloneRecord(reencoded, "EVENTS", 17, "2026-09-26T12:35:00.000000001Z"), inboundMeta), inboundMeta)
	reencodedGoDecodes := reencodedDecodeErr == nil
	if reencodedGoDecodes != fixture.ReencodedGoDecodes { t.Fatalf("%s reencoded Go decode observation drifted: got %t, want %t", fixture.Name, reencodedGoDecodes, fixture.ReencodedGoDecodes) }
	dlq, _ := deadLetterMessage(recordMsg(t, fixture.Inbound, inboundMeta), inboundMeta, decoded, deadLetterExhausted)
	if got := cloneRecord(dlq, fixture.DLQ.Stream, fixture.DLQ.StreamSequence, fixture.DLQ.StoredAt); !reflect.DeepEqual(got, fixture.DLQ) { t.Fatalf("%s DLQ Go record drifted\n got %#v\nwant %#v", fixture.Name, got, fixture.DLQ) }
	restored, err := RestoreDeadLetter(recordMsg(t, fixture.DLQ, recordMetadata(t, fixture.DLQ)))
	if err != nil { t.Fatalf("%s RestoreDeadLetter: %v", fixture.Name, err) }
	if got := eventRecord(restored); !reflect.DeepEqual(got, fixture.Restored) { t.Fatalf("%s restored Go record drifted\n got %#v\nwant %#v", fixture.Name, got, fixture.Restored) }
}

func verifyProvenance(t *testing.T, provenance compatProvenance) {
	t.Helper()
	want := compatProvenance{
		Repository: "https://github.com/Dankosik/go-service-template-rest",
		Revision: "a50f43cf869b86accb3fdba24d758cc2d15a02df",
		Files: map[string]string{
			"internal/infra/natsjs/message_wire.go": "c98d7c1db43f515e072e91416cf1baa6d0da5c898337d62be9fef3173ed9fe88",
			"internal/infra/natsjs/message_deadletter.go": "4063580a0322974331c13f3de19e3296b0c40d1785cbbf98d695dc74ef5b953c",
			"internal/domainevent/event.go": "add1371aa3ef73b26a9f1eb08d9b992f48969d4a8f6dbd256d81d6d1a058e556",
		},
	}
	if !reflect.DeepEqual(provenance, want) { t.Fatalf("fixture provenance drifted\\n got %#v\\nwant %#v", provenance, want) }
}

func verifyLimits(t *testing.T) {
	t.Helper()
	base := Event{Subject: "events.limits", MessageID: strings.Repeat("m", 256), PublicationID: strings.Repeat("p", 256), Type: strings.Repeat("t", 256), Schema: "v65535", CreatedAt: time.Date(2026, 9, 26, 12, 0, 0, 0, time.UTC), Payload: []byte(`{"v":1}`)}
	if _, err := buildNATSMessage(context.Background(), base, len(base.Payload)); err != nil { t.Fatalf("maximum identity was rejected: %v", err) }
	overIdentity := base
	overIdentity.MessageID += "x"
	if _, err := buildNATSMessage(context.Background(), overIdentity, len(overIdentity.Payload)); err == nil { t.Fatal("over-limit identity was accepted") }
	overPayload := base
	overPayload.Payload = append(overPayload.Payload, 'x')
	if _, err := buildNATSMessage(context.Background(), overPayload, len(base.Payload)); err == nil { t.Fatal("over-limit payload was accepted") }
}

func verifyRustExport(t *testing.T, path string) {
	t.Helper()
	if path == "" { t.Fatal("GO_WIRE_RUST_EXPORT is required") }
	var exports []compatRecord
	data, err := os.ReadFile(path)
	if err != nil { t.Fatal(err) }
	if err := json.Unmarshal(data, &exports); err != nil { t.Fatal(err) }
	if len(exports) == 0 { t.Fatal("Rust export contains no production-encoded records") }
	for index, record := range exports {
		meta := recordMetadata(t, record)
		if _, _, err := decodeMessage(recordMsg(t, record, meta), meta); err != nil { t.Fatalf("Rust record %d rejected by actual Go decodeMessage: %v", index, err) }
	}
}

func eventRecord(event Event) compatEvent { return compatEvent{event.Subject, event.MessageID, event.PublicationID, event.Type, event.Schema, event.CreatedAt.UTC().Format(time.RFC3339Nano), event.CreatedAt.Unix(), event.CreatedAt.Nanosecond(), base64.StdEncoding.EncodeToString(event.Payload)} }
func decodedRecord(message Message) compatDecoded { return compatDecoded{message.Subject(), message.MessageID(), message.PublicationID(), message.Type(), message.Schema(), message.CreatedAt().UTC().Format(time.RFC3339Nano), message.CreatedAt().Unix(), message.CreatedAt().Nanosecond(), base64.StdEncoding.EncodeToString(message.Payload())} }
func eventFromRecord(t *testing.T, record compatEvent) Event { t.Helper(); createdAt, err := time.Parse(time.RFC3339Nano, record.CreatedAt); if err != nil { t.Fatal(err) }; body, err := base64.StdEncoding.DecodeString(record.BodyBase64); if err != nil { t.Fatal(err) }; return Event{Subject: record.Subject, MessageID: record.MessageID, PublicationID: record.PublicationID, Type: record.Type, Schema: record.Schema, CreatedAt: createdAt, Payload: body} }
func cloneRecord(msg *nats.Msg, stream string, sequence uint64, storedAt string) compatRecord { headers := make(map[string]string, len(msg.Header)); for key, values := range msg.Header { if len(values) != 1 { panic("compat bridge supports one value per header") }; headers[key] = values[0] }; return compatRecord{Subject: msg.Subject, Headers: headers, BodyBase64: base64.StdEncoding.EncodeToString(msg.Data), Stream: stream, StreamSequence: sequence, StoredAt: storedAt} }
func recordMetadata(t *testing.T, record compatRecord) *jetstream.MsgMetadata { t.Helper(); storedAt, err := time.Parse(time.RFC3339Nano, record.StoredAt); if err != nil { t.Fatal(err) }; return &jetstream.MsgMetadata{Sequence: jetstream.SequencePair{Stream: record.StreamSequence, Consumer: 1}, Timestamp: storedAt, Stream: record.Stream, Consumer: "go-wire-compat"} }
func recordMsg(t *testing.T, record compatRecord, metadata *jetstream.MsgMetadata) *compatMsg { t.Helper(); body, err := base64.StdEncoding.DecodeString(record.BodyBase64); if err != nil { t.Fatal(err) }; header := make(nats.Header, len(record.Headers)); for key, value := range record.Headers { header.Set(key, value) }; return &compatMsg{subject: record.Subject, header: header, data: body, meta: metadata} }
func readFixtures(t *testing.T, path string) compatFixtureSet { t.Helper(); data, err := os.ReadFile(path); if err != nil { t.Fatal(err) }; var set compatFixtureSet; if err := json.Unmarshal(data, &set); err != nil { t.Fatal(err) }; return set }
func writeJSON(t *testing.T, path string, value any) { t.Helper(); data, err := json.MarshalIndent(value, "", "  "); if err != nil { t.Fatal(err) }; data = append(data, '\n'); if err := os.WriteFile(path, data, 0o644); err != nil { t.Fatal(err) } }

GO_BRIDGE

case "$mode" in
  generate)
    GO_WIRE_FIXTURES="$fixture_file" GO_WIRE_MODE=generate go -C "$scratch/source" test ./internal/infra/natsjs -run '^TestGoWireCompatibilityBridge$' -count=1
    ;;
  check)
    GO_WIRE_FIXTURES="$fixture_file" GO_WIRE_MODE=check go -C "$scratch/source" test ./internal/infra/natsjs -run '^TestGoWireCompatibilityBridge$' -count=1
    ;;
  verify-rust)
    GO_WIRE_FIXTURES="$fixture_file" GO_WIRE_MODE=verify-rust GO_WIRE_RUST_EXPORT="$rust_export_file" go -C "$scratch/source" test ./internal/infra/natsjs -run '^TestGoWireCompatibilityBridge$' -count=1
    ;;
esac
