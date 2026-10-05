# T2 — Reusable lazy gRPC dial recovery

Outcome:
The existing lazy Client applies the native cooperative five-second DNS/TCP/TLS
timeout, preserves its original deadline across idle polling, and can recover
through the same Client without extending caller or stream lifetimes. Physical
cleanup may wait for resumed polling; a later call may observe the retained
error before a subsequent call redials. Native ready-before-timeout behavior is
retained, without an eager idle-cleanup or strict late-ready rejection promise.

Consumes:
- [Specification](../spec.md#grpc-reusable-channel-recovery).
- [Design](../design/transport.md#grpc-and-authentication) — native HttpConnector settings and tonic wrapper order.
- [Execution boundary](execution-boundary.md).

Provides:
- Native bounded lazy connector, regression coverage and accurate gRPC deadline/keepalive/material-rotation guidance.

Boundary:
Use supported connect_with_connector_lazy with native hyper-util; construction
stays network-free. Preserve TLS, endpoint-equivalent socket/HTTP2 settings,
status/finality, shorter caller budgets and OpeningOnly streams. No RPC retry,
custom connector implementation, discovery or stream-age policy.

Mutable owners:
- `crates/infra-grpc/src/client.rs`, `crates/infra-grpc/Cargo.toml` and its existing tests/fixtures.
- `docs/grpc.md` for connection lifetime, active-call 60 s PING/20 s timeout, peer enforcement and construction-time material.

Exclusive locks:
- none for these owners. Any necessary root Cargo.lock adjustment is handed to the exclusive dependency/profile/delivery assembly writer before task integration; never mutate it concurrently.

Final validation:
- Claim: Same-client later recovery works after a stalled full dial, preserving lazy construction and existing deadline/stream semantics.
- Checks: Matching assembled build/tests and documentation route under execution-boundary.md; no additional checks.
- Observable: A still-pending expired dial is released when readiness is polled, subsequent calls recover through the same client, idle time does not renew its deadline, and caller/OpeningOnly boundaries retain their meaning.

Reopen if:
Native connector/wrapper behavior differs from the accepted design (Technical
Design); a new behavior or compatibility exception is needed (Definition).
