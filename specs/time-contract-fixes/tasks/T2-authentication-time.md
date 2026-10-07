# T2 — Preserve authentication time and evidence provenance

Outcome:
Replace sentinel calendar time and unconditional retained-principal reuse with
canonical unavailable trust on clock failure and current-calendar eligibility
on retained introspection evidence. Preserve actual fresh provider evidence,
including joined waiters, under existing fresh lifetime/leeway semantics.

Consumes:
- [Specification: introspection reuse](../spec.md#introspection-reuse) and
  [unavailable authentication clock](../spec.md#unavailable-authentication-clock).
- [Design: introspection evidence](../design/technical-design.md#introspection-evidence-and-conditional-reuse)
  and [calendar failure](../design/technical-design.md#unavailable-calendar-time).
- [Bearer owner](../../../crates/infra-bearerauthn/src/lib.rs), its claims and
  introspection engines, and [authentication guide](../../../docs/authentication.md).
- Existing bounded HTTP contract; T1 code is not an implementation prerequisite.

Provides:
- A private verified introspection result and explicit Fresh/Retained provenance.
- Usable-time verification with the existing unavailable-trust mappings and
  updated authentication guidance.

Boundary:
The claims, engine and cache changes form one authentication outcome: retain
Principal's public identity/equality while adding the reviewed private evidence
wrapper. Keep positive storage separate from the zero-retention native Moka
try_get_with fill coordinator, using the same digest/trust context and explicit
Fresh/Retained tags. Every retained consumer must pass its own current strict
expiry and existing nbf-leeway check; a failed check follows the miss/provider
path without stale invalidation. Preserve joined fresh results and native error
sharing, next-call recovery, cancellation and fixed replacement retention.
Immediately before delivery each Fresh caller, including a joined waiter,
samples usable time and reapplies existing fresh expiry then nbf guards with
unchanged 30-second leeway, equality and saturating arithmetic. A now-invalid
fresh result returns canonical invalid trust without provider retry or cache
invalidation; a provenance tag cannot authorize past those guards.

Return the closed clock-unavailable reason through existing verification and
transport failure owners. Keep deterministic timing seams private and narrow;
no universal Clock, exported test-only API, public verifier option, negative
cache, stale fallback, raw token observation, skew or provider-budget change.
Update the guide's wall-clock-step and pre-epoch sentinel statements together
with the code. Preserve template markers and canonical code ownership.

Mutable owners:
- `crates/infra-bearerauthn` claims, JWT/introspection verification, cache,
  closed reason/observations, relevant tests and rustdoc; no manifest change.
- `docs/authentication.md` time, caching and clock-failure guidance.
- This packet's implementation details and chosen final-validation commands.

Exclusive locks:
- none.

Final validation:
- Claim: A retained result cannot authorize outside its current calendar
  eligibility; unavailable calendar time never authenticates; fresh provider
  evidence retains its accepted leeway while coalescing, replacement retention,
  error sharing and recovery preserve the existing contract.
- Checks: The ledger's consolidated matching build and relevant tests,
  documentation consistency and final assembled authorization-sensitive review.
  No additional runtime requirement. Concrete cases, controls and commands are
  chosen by the Lead during implementation, then executed at final validation.
- Observable: Bearer verification and existing unavailable-trust result boundary;
  native coalescing and evidence provenance remain distinguishable. Source proof
  of unchanged HTTP/gRPC mapping is not a claim of live provider execution.

Reopen if:
System Design if Moka provenance/sharing, retention or the private clock flow
contradicts its resolved-library evidence; Specification for altered temporal
compatibility or failure semantics. Report a required transport write overlap
to the root before mutation rather than extending into T3's owner.

## Implementation result

- unit: T2.
- verdict: Implemented.
- candidate: bounded working-tree changes in `infra-bearerauthn`'s `lib.rs`,
  `claims.rs`, `introspection.rs`, `jwt.rs`, and `docs/authentication.md`.
- provides: private verified introspection lifetime evidence; separate positive
  retention and zero-TTL native Moka fills with explicit Fresh/Retained results;
  strict current retained eligibility and caller-current fresh delivery guards;
  fixed replacement retention; canonical unavailable clock failure and recovery.
  Principal identity, fresh leeway/order, provider policy and transport mapping
  remain with their existing owners.
- implementation details: private per-engine Unix-time callbacks follow the
  production sampling boundaries. Tests reuse the TLS fixture and native Moka
  publication gates; no exported timing API or callback-count choreography.
  Added forward-expiry/backward-not-before reuse, retained waiter provenance and
  newer replacement preservation, delayed fresh waiter equality/refusal/clock
  failure, pre-epoch extreme-expiry JWT/introspection recovery, and replacement
  retention cases. Existing coalescing coverage now also shares provider outage
  errors and proves subsequent recovery; cancellation and nonretained-success
  cases remain the existing primary owners.
- chosen final commands: consolidate `make build` with the other tasks;
  `make test-package PKG=infra-bearerauthn` or include this crate once in the
  root's `make test-changed PKGS="..."` route; `make docs-check` for the assembled
  documentation. Root may collect bounded compiler diagnostics with
  `cargo check --locked -p infra-bearerauthn --all-targets` before final execution.
  Final authorization-sensitive review belongs to the assembled result.
- uncertainty: code and test cases are authored but compiler and runtime proof
  have not run. In particular the zero-TTL joined publication and new timing
  cases await the consolidated crate tests. No live-provider proof is claimed.
- next_owner: root Ledger Orchestrator for assembled diagnostics, validation and
  review. T2 writer releases this scope and remains available for repairs.
- release scope: local implementation only; no commit, push, deployment,
  infrastructure or transport-source mutation.
