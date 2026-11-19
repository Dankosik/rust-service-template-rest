# T4 — Complete immediately terminal gRPC responses in place

Outcome:
Already terminal outbound gRPC responses reach the same headers and once-only completion effects without constructing or locking shared state for a `Done` stream.

Depends on:
- none

Sources:
- [R6](../spec.md#r6--immediately-terminal-grpc-responses), [D6](../design/design.md#d6--finish-terminal-grpc-responses-before-allocating-stream-state).
- `crates/infra-outbound-grpc/src/call.rs::{attach_client_response,ClientResponseBody::start,StreamCompletion::finish,StreamPhase::{transition,stage_stopped_trailers}}` — first lifetime stop, wire status, later stop or EOS and release of custody.
- `crates/infra-outbound-grpc/src/client.rs::PreparedCall::send_inner`, `vendor/tonic/src/body.rs::Body::{empty,new}`, `vendor/tonic/src/client/grpc.rs`, `vendor/tonic/src/codec/decode.rs` — transport versus decoded observer selection, tonic boxing and decoder ownership.
- `crates/infra-outbound-grpc/tests/client_status.rs` — public transport and decoded completion boundaries.

Provides:
- Immediate gRPC terminal completion with unchanged transport and decoder ownership

Boundary:
- In scope: `crates/infra-outbound-grpc/src/call.rs` response attachment and custody and directly affected `tests/client_status.rs` coverage. Keep the current completion, including hook and drop effects, before returning the empty tonic body when phase is `Done`. Keep staged trailers distinct from `Done`, and keep existing wrappers for staged trailers and open bodies. Spawn the watcher only for a watched open lifetime as today.
- Out of scope: `client.rs` send and observer selection, T3 method and span observation, and vendored implementation.

Mutable owners:
- `crates/infra-outbound-grpc/src/call.rs` response custody, including its module tests, and `crates/infra-outbound-grpc/tests/client_status.rs`.

Exclusive locks:
- none

Final validation:
- Observable claim: At public gRPC transport and decoded response attachment, an already-`Done` response produces the empty body after completion without constructing or locking stream state. Preserve G1, G4 and G5 including precedence of existing local stop and wire/fallback statuses, immediate upload release, once-only wire hook and transport reporting, decoded outcome custody, staged trailers, and open-stream idle stops.
- Additional proof: Matched [gRPC workload evidence](../design/comparison.md#workloads), owned by the Delivery Owner.

Reopen if:
- Technical Design owns a change to tonic's empty body, decoder or status-observer contract.
