# T2 — Reduce work in prepared outbound HTTP calls

Outcome:
Prepared HTTP requests borrow the client destination through attempt finalization, split the authority host once during normal origin admission, and use static zero-length values for empty methods with a content-length requirement.

Depends on:
- none

Sources:
- [R3 and R4](../spec.md#r3--http-observation-and-destination-fields), [D3 and D4](../design/design.md#d3--borrow-http-destination-data-through-attempt-finalization).
- `crates/infra-outbound-http/src/lib.rs::Client::prepare_scoped`, `PreparedRequest::send_with_response`, `buffer_response`; `crates/infra-outbound-http/src/observe.rs::Attempt::{start,record_status,finish}` — borrowed send scope, final headers, abandonment guard and consumption.
- `crates/infra-outbound-http/src/policy.rs::{Origin::admits,admit_request,frame_body}`, `crates/shared-rules/src/uri.rs::http_origin_port` — admitted aliases, authority/userinfo policy, canonical Host and framing.
- `http@1.5.0/src/uri/authority.rs::Authority::host`, `src/header/value.rs::HeaderValue::from_static` — borrowed host view and static bytes.
- `crates/infra-outbound-http/src/transport.rs::build_transport`, `vendor/hyper-util/src/client/legacy/client.rs` — existing configuration and retry boundary.

Provides:
- Prepared HTTP path with borrowed destination and equivalent admission and framing

Boundary:
- In scope: The private prepared HTTP admission/send/observation owner and its affected coverage. Keep one DropGuard and the same disarm/finish path. Use the already borrowed client destination during the attempt and preserve recorder ownership at emission. Reuse one host view in origin admission. Strip Transfer-Encoding, use literal zero for empty POST, PUT or PATCH, remove Content-Length for other empty methods, and retain numeric byte length for nonempty bodies.
- Out of scope: Buffer ownership, lifetime or URI changes, transport settings and all conditional alternatives. Keep credential and gRPC owners untouched.

Mutable owners:
- `crates/infra-outbound-http/` prepared request, policy, attempt and directly affected coverage.

Exclusive locks:
- none

Final validation:
- Observable claim: Buffered and headers-only calls reach the same admitted peer while the attempt keeps a destination borrow instead of cloning its owner, the normal admission path extracts its host once, and empty required Content-Length uses static zero without numeric formatting. Preserve G1, G3, G4 and G5 including one terminal observation, after-header status and classification on body failure, timely final checks, trace fields, admission, retry, and on-wire framing.
- Additional proof: Matched [HTTP workload evidence](../design/comparison.md#workloads), owned by the Delivery Owner.

Reopen if:
- Technical Design owns an observer that must outlive the prepared client's borrow or a changed supported origin/framing contract.
