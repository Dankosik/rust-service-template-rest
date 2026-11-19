# T3 — Reduce gRPC observation work

Outcome:
Outbound gRPC reuses a verified series for warm declared methods without reparsing the path, avoids the admission lock on a full-snapshot miss, and defers copying the two diagnostic strings to OpenTelemetry value conversion.

Depends on:
- none

Sources:
- [R5](../spec.md#r5--grpc-method-admission-and-tracing), [D5](../design/design.md#d5--reuse-validated-grpc-method-labels-and-defer-diagnostic-string-copies).
- `crates/infra-outbound-grpc/src/observe.rs::{Call::start,make_client_span,Series::{resolve,admit_path},MethodMetrics,verified_client_path}` — labels, append-only width, declared method verification and attribute bounds. The existing parsed path validator remains the authority for new names.
- `crates/infra-outbound-grpc/src/client.rs::Client::{prepare_call,refuse_call}`, `vendor/tonic/src/extensions.rs::GrpcMethod` — call observation and injection from existing span/ambient context and the exact declaration extension.
- `arc-swap@1.9.4::ArcSwap::load`, `metrics@0.24.6::SharedString`, `tracing-opentelemetry@0.34.0/src/span_ext.rs::OpenTelemetrySpanExt::set_attribute`, `opentelemetry@0.33.1/src/common.rs::Value` — immutable reader snapshots, comparable stored labels and conversion after the bridge availability check.

Provides:
- Bounded gRPC observation fast path with unchanged labels and wire context

Boundary:
- In scope: `crates/infra-outbound-grpc/src/observe.rs` and observation coverage within that module. Require declaration and each 256-byte part bound before lookup, and cap the path lookup at 514 bytes. Compare both declared parts to the admitted labels on a hit. On a miss at or beyond 256 admitted methods return fallback without locking. Below capacity parse and verify the new name, with the existing recheck under the admission lock. Keep bounded diagnostic checks and use a private borrowed conversion for the original method and user agent at their existing attribute calls.
- Out of scope: `client.rs` context injection and entry points remain read-only. T4 owns response custody and its separate tests. Preserve the current fallback label, trace visibility and attachment boundary.

Mutable owners:
- `crates/infra-outbound-grpc/src/observe.rs` outbound metrics and span creation, including its module tests.

Exclusive locks:
- none

Final validation:
- Observable claim: Outbound gRPC admission avoids a warm-path parse only for an exact path with matching declared labels, and a full-registry miss no longer attempts the admission mutex. Disabled or bridgeless tracing avoids eager copies of eligible diagnostic values. Preserve G4 and G5 including cardinality, labels, redaction and enabled attribute behavior, plus unchanged wire context when the outbound span is filtered and an ambient context exists.
- Additional proof: Matched [gRPC workload evidence](../design/comparison.md#workloads), owned by the Delivery Owner.

Reopen if:
- Technical Design owns a change to admitted label identity, append-only snapshots, or telemetry conversion order. A mixed subscriber filtering only the bridge may still convert before discovering missing span data; evaluate the narrower saving D5 specifies.
