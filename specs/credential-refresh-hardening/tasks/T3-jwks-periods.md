# T3 — Independent JWKS periods under the existing worker

Outcome:
The single JWKS worker schedules independent bounded periods while retaining
key publication, cancellation and unknown-key authority; authentication guidance
accurately describes refresh and revocation limits.

Consumes:
- [Specification R1/R2](../spec.md#r1-bounded-schedule-spread) — periodic range,
  unknown-key cooldown, last-good key and revocation contract.
- [D2 JWKS](../design/technical-design.md#jwks) — persistent monotonic deadline,
  rearm behavior and existing KeyStore/pending-loop ownership.
- [D1/D3/D4](../design/technical-design.md#d1-reuse-the-installed-fallible-randomness-source)
  — installed fill API, private arithmetic and canonical source/guide placement.

Provides:
- Initial and subsequent `[13m30s,15m]` sampled waits in the existing refresh
  worker and focused tests beside current owners.
- Canonical authentication guide distinguishes periodic refresh and revocation.

Boundary:
Replace fixed Interval with one sleep-until deadline. Preserve it through
unknown-key work; rearm from observed monotonic time only when periodic work is
selected, with no catch-up backlog. Retain biased cancellation-first waiting
and fetch selection, one fetch/publication worker, pending tickets, store stop,
coalescing, last-good keys, sanitized failures and exactly 30-second unknown-key
cooldown. RNG failure retains the original 15-minute period. Remove unused
Interval/MissedTickBehavior imports. Document no key maximum-age cutoff, no
known-kid bad-signature forced refresh, token expiry and fixed policy material.
No new task, readiness policy, trust cutoff or public interface.

Mutable owners:
- `crates/infra-bearerauthn/src/refresh.rs` and associated existing tests within
  the bearer-authentication crate; `KeyStore` stays unchanged except a necessary
  behavior-preserving test adjustment within the accepted boundary.
- Canonical `docs/authentication.md`, preserving profile markers.

Exclusive locks:
- none.

Final validation:
- Claim: Periodic waiting meets bounds without moving unknown-key cooldown,
  losing cancellation/coalescing or changing last-good publication semantics.
- Checks: Consolidated build/test/docs and assembled protected review from
  [ledger Completion](../tasks.md#completion-evidence); executor selects proving
  controls without chance-sensitive assertions. No additional live check.
- Observable: Local scheduler/owner evidence and accurate guidance; wait bounds
  are not a network-completion SLA or immediate key-revocation claim.

Reopen if:
D2 conflicts with actual worker/KeyStore authority or Specification timing,
revocation or lifecycle behavior must change. Concrete cases remain executor-owned.
