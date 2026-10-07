# Supporting evidence: operation budgets

Definition capture, 2026-10-05. Baseline commit:
`78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`; branch
`codex/operation-budgets-20261005`. The continuation coordinator supplied the
accepted research PASS from `5927ffb`; the intervening baseline change touches
only auth/cache/outbound-machine-auth/production-contract documentation. This
capture reuses that source investigation rather than claiming new live runtime
measurements. Focused current-source inspection confirmed the context and body
ownership gaps below. No measured outbound overrun was established.

## Current owners and gaps

| Surface | Evidence and decision effect |
| --- | --- |
| HTTP | [RequestDeadline](../../../crates/infra-http/src/harden.rs), lines 45–76 and 175–220, exposes the fixed cutoff; admission/Tower covers opening through headers. Preserve origin and expose usable cancellation/budget context. |
| PostgreSQL | [Persistence architecture](../../../docs/architecture/persistence.md) owns acquire, session limits, idempotency/webhook whole-attempt cutoff and 100 ms reserve. `in_tx` has no deadline input; its commit classification and native return custody are deliberately unchanged. |
| gRPC inbound | [Router](../../../crates/infra-grpc/src/router.rs), lines 324–341, creates opening cutoff before authentication but does not place its private deadline in the business request. Valid caller duration owns stream lifetime from the same origin; absence currently leaves generic stream lifetime uncapped. |
| gRPC client | [Client](../../../crates/infra-grpc/src/client.rs), lines 49–64 and 160–232, defaults to `FullRpc`, bounding the full call from its own entry. Explicit `OpeningOnly` bounds opening only, retaining a caller-supplied lifetime or no local stream cap after headers. [OAuth binding](../../../crates/infra-oauth2-client-credentials/src/grpc.rs), lines 48–126, accepts either mode and uses supplied metadata for acquisition; absent metadata permits a fetch prelude followed by an independent local resource budget. Both compositions must retain their selected interval while sharing its origin with credential acquisition. |
| Authentication | [Provider](../../../crates/infra-bearerauthn/src/provider.rs), lines 375–382, has a three-second reqwest total limit and no automatic retry. Introspection coalesces; unknown-key JWT callers wait on shared process-owned refresh. Per-request wait cancellation must preserve that owner. |
| Cache | [Public adapter](../../../crates/infra-cache/src/lib.rs), lines 396–419, starts its own command deadline; [connection](../../../crates/infra-cache/src/connection.rs), lines 142–154, shares it across acquire/dispatch. No command replay. Supervisor reconnect is independently owned background recovery. |
| Outbound HTTP | [Adapter](../../../crates/infra-outbound-http/src/lib.rs), lines 194–254, computes remaining duration before synchronous preparation then starts a relative timeout. Correct by retaining absolute cutoff, without asserting measured runtime overrun. |
| Object storage | [Adapter](../../../crates/infra-object-storage/src/lib.rs), lines 269–295 and 422–460, configures SDK operation/read-attempt/connect/retry bounds but returns the body separately. [Download](../../../crates/infra-object-storage/src/download.rs), lines 1–150, retains the admission permit until body completion/drop and collection has no total body deadline. SDK stall protection is not a total lifetime bound. |
| Messaging/jobs | [Messaging registry](../../../crates/infra-messaging/src/registry.rs) passes cancellation only; [consumer](../../../crates/infra-messaging/src/consumer.rs), around line 647, wraps handler timeout. [Job context](../../../crates/infra-jobs/src/kind.rs), lines 108–117, already exposes fixed attempt deadline and cancellation. Extend business access without moving settlement/retry ownership. |

## Retry and uncertainty facts to preserve

- Jobs account at most 25 attempts by default with a 60 s attempt budget;
  polynomial backoff and jitter remain their owner. Forced-drain/snooze refunds
  can occur after an external call, so this is not a lifetime physical-call cap.
- Webhooks retain 20 attempts/30 s, terminal 410, and their bounded Retry-After
  floor. Outbox retains 25 attempts/30 s and one publish per attempt with the
  same immutable ID. JetStream retains five handler deliveries/30 s and its
  existing NAK schedule; settlement redelivery and ACK-loss duplicates remain.
- S3 read SDK attempt count is not total HTTP count: STS/ECS/IMDS credential
  owners may make additional calls. PUT/delete retain one SDK attempt and
  `OutcomeUnknown`. SQLx reconnect and Hyper reassignment of an unsent request
  are not permission for application effect replay.
- The SQLx bounded-return patch is owned by
  [PATCHES.md](../../../vendor/sqlx-core/PATCHES.md); this task does not alter it.

## Mechanism leads, not adopted design

The accepted research found pinned tower-http 0.7.1 already offers absolute
body deadline wrappers, while per-frame timeout resets each frame. Both are
poll-driven and alone cannot release an abandoned stream's permit. Bounded
collection and presigned GET are existing alternatives. Technical Design must
check resolved library APIs and choose an ownership mechanism satisfying the
accepted no-poll release behavior; no custom wrapper or new crate is preselected.

Concurrent reference PRs #242 (head
`b0f9899dd90ca1e1002e4dbf7575e3799a381e08`), #248 (reported head prefix
`7420f7c`), #246, and #247 are read-only leads about absolute deadlines,
S3 lifetime, transport recovery, and credentials. They are not merged baseline
or proof for this candidate. Verify exact identities and relevant changes before
reuse; never import their unrelated changes or modify their worktrees.

## Evidence boundary

This capture establishes source-level design inputs and accepted exclusions.
It does not establish live latency, production duration adequacy, runtime
resource release, merged state of a reference PR, or new library/API behavior.
The matching implementation proof and final candidate review remain ahead.
