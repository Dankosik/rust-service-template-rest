# T2 — Bounded OAuth refresh within the existing token lifetime

Outcome:
Reusable service tokens gain one stable randomized refresh lead and bounded
retry spread while their existing expiry, queue and driver ownership remains
intact; canonical OAuth guides describe that behavior and signing-key rotation.

Consumes:
- [Specification R1/R2](../spec.md#r1-bounded-schedule-spread) — timing ranges,
  preserved OAuth invariants and separate signing-key rotation contract.
- [D1/D2 OAuth](../design/technical-design.md#oauth-service-tokens) — installed
  fallible RNG, integer arithmetic and the three existing assignment owners.
- [D3/D4](../design/technical-design.md#d3-responsibility-and-file-map) — private
  adapter policy, current tests and canonical guide/decision-record owners.

Provides:
- One sampled refresh eligibility per admitted reusable service token and
  sampled next eligibility at the two existing retry assignment sites.
- Associated tests and consistent outbound authentication guide/decision record.

Boundary:
Use `[0.9 A,A]` lead with `A=min(5min,reusable lifetime/4)` at admission and
`[30s,33s]` retry spacing at enqueue and successful completion according to D2.
Cache hits do not slide or resample. Preserve foreground misses, reuse cutoff,
missing expiry/request-only behavior, overflow rejection, exact invalidation,
provider semaphore, one pending queue, shared lock, five-second enqueue-based
budget, one-second completed failure suppression and joined shutdown. Exchange
keeps existing cache behavior and no random background refresh. Source failure
uses the old schedule. Signing key/kid remain fixed startup material; document
provider overlap and independently valid issued tokens. No new timer or state owner.

Mutable owners:
- OAuth token/cache schedule policy and focused tests in
  `crates/infra-oauth2-client-credentials/src/lib.rs`, `src/tests.rs`, and existing
  proof surfaces within that crate.
- `docs/outbound-machine-authentication.md` and
  `docs/outbound-machine-authentication-decisions.md`, preserving markers.

Exclusive locks:
- none.

Final validation:
- Claim: Changed eligibility obeys accepted ranges and stable token ownership
  without weakening expiry, admission, failure suppression or driver completion.
- Checks: Consolidated build/test/docs and assembled protected review from
  [ledger Completion](../tasks.md#completion-evidence); executor selects focused
  cases and reuses adequate existing protection tests. No additional live check.
- Observable: Local behavior evidence and matching canonical text distinguish
  access-token refresh from signing-key replacement; no live provider assertion.

Reopen if:
D2 cannot preserve actual token/cache/driver semantics, or Specification timing,
expiry or compatibility needs change. Test selection and routine private factoring
remain Implementation decisions.
