# T1 — Bounded NATS reconnect with accurate credential guidance

Outcome:
NATS attempts beyond the existing immediate attempts independently vary within
the accepted capped window, while the canonical messaging guide describes
reconnect and credential publication accurately.

Consumes:
- [Specification R1/R2](../spec.md#r1-bounded-schedule-spread) — accepted ranges,
  existing recovery/admission invariants and NATS operator contract.
- [D1-D3](../design/technical-design.md#d1-reuse-the-installed-fallible-randomness-source)
  — existing fallible AWS-LC source through the SDK's rustls re-export,
  saturating delay policy and messaging ownership. The bounded
  [D1/D3 recovery](../design/design-dependency-transition.md) supersedes the
  earlier direct-dependency edge.
- [D4](../design/technical-design.md#d4-canonical-documentation-mapping)
  — messaging guide remains canonical.

Provides:
- The existing NATS adapter installs its supported private reconnect delay
  callback; tests and `docs/durable-messaging.md` cover the new contract.
- Messaging captures the static RNG interface through the already enabled
  async-nats/rustls AWS-LC backend. No manifest, feature or lockfile change remains.

Boundary:
Replace only the deterministic reconnect schedule: attempts 0/1 remain immediate,
subsequent capped B maps to `[0.9 B,B]`, saturation stays 4 seconds including
large attempt counts, fresh fallible samples preserve the conservative schedule
on source failure. Retain SDK retry/lifecycle ownership, unlimited recovery,
auth callback rereads, startup/request budgets and sanitized failures. Explain
coherent per-challenge JWT+seed reading, external early renewal, inline startup
snapshot, open sessions, recovery after expiry/refusal and reconnect CA reread.
No auth-reader redesign, framework, profile knob or new runtime owner.

Mutable owners:
- Existing messaging adapter policy and its focused tests:
  `crates/infra-messaging/src/messaging.rs` and existing messaging proof surfaces.
- Remove only this task's attempted `aws-lc-rs` declaration from
  `crates/infra-messaging/Cargo.toml`, restoring its original bytes.
  `Cargo.lock` is unchanged.
- Canonical `docs/durable-messaging.md`, including its template markers.

Exclusive locks:
- The bounded restoration of T1's attempted manifest declaration is T1-owned.
  No lockfile generation or dependency-input change remains after restoration.

Final validation:
- Claim: Accepted bounds and fallback hold without weakening existing NATS
  recovery/authentication ownership; documentation matches the implementation.
- Checks: One assembled repository build/test/docs boundary in [ledger Completion](../tasks.md#completion-evidence);
  existing selected dependency/profile gates remain CI-owned. No additional live check.
- Observable: Local behavior evidence for changed policy and source-qualified
  operator guidance; no claim of measured fleet distribution or real rotation.

Reopen if:
D1 cannot supply retained-profile dependency resolution/API, D2 conflicts with
actual SDK scheduling semantics, or Specification ranges/compatibility must change.
Routine test choices and the scoped manifest restoration stay with Implementation.
