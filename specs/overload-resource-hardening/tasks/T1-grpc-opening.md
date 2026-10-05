# T1 — Bound business gRPC openings before authentication

Outcome:
Business openings on one composed router and its clones use the existing K as
an independent fail-fast bound before all bearer-verification/handler work,
while authenticated terminal calls retain their existing K bound. The published
gRPC and resource-scope guidance describes that actual ownership accurately.

Consumes:
- [Spec G1](../spec.md#g1-bounded-grpc-opening-before-authentication) and
  [D1](../spec.md#d1-precise-unchanged-capacity-and-workload-guidance) — closed
  behavior and unchanged scopes.
- [Mechanism G1](../design/mechanism.md#g1-material-flow-and-ownership) and
  [documentation/projections](../design/mechanism.md#documentation-projections-and-collision-boundary)
  — existing middleware order, two semaphores, RAII opening custody and bounded
  cooperative rejection.
- [Ownership](../design/ownership.md#responsibilities), its
  [inverse map](../design/ownership.md#files-inverse-map-for-all-expected-rust-changes)
  and [guide map](../design/ownership.md#non-rust-files-and-projection-custody)
  — canonical source, guide and projection owners.
- [Dispositions](../recommendation-dispositions.md) — all recommendation
  exclusions and immutable independent-PR evidence, especially auth #247,
  gRPC client #246, resource/lifetime guidance #248 and logging/CPU #244/#245.
- Current source at `78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`;
  `crates/infra-grpc/src/router.rs`, `tests/transport.rs` under that crate and
  existing `call.rs`/status/observe/auth owners — implementation and parity.

Provides:
Implemented G1 with focused executor-authored tests, accurate companion source
comments/guides, and preserved projections. No new interface is consumed by T2;
its implementation dependency is release of shared documentation ownership.

Boundary:
Add the accepted private opening layer below the original deadline and above
authentication, sharing its independent semaphore across clones. Preserve
existing terminal body custody, health bypass, zero opt-out, auth results,
deadline precedence, shed identity and 100 ms/64 KiB rejection limits. Rejection
polling must cooperate as accepted. Replace superseded single-count claims.

D1 companion work consolidates unchanged resource-scope guidance in the
production contract as conditional obligations when a service retains/adopts
the capability: handler-head versus
feature-owned HTTP body custody; auth/cache/provider attempts versus all
callers and actual resource EOF; original consuming deadlines; SQLx native
acquisition, 100 ms HTTP reserve, short transactions, shared readiness and
outcome uncertainty; jobs/webhook/outbox active capacity versus service-owned
backlog, expiry and fairness; peak replica/pool/LISTEN/admin/provider/broker
accounting; consumer-owned CPU/blocking admission with no new workload.
Keep unresolved business choices unresolved. Use only file-level links to the
always-retained `docs/configuration-source-policy.md`,
`docs/architecture/integration.md`, `docs/architecture/runtime-lifecycle.md`
and `docs/architecture/persistence.md`. Their existing marked sections own
optional-guide links. Production-contract remains unmarked, with no direct
links to removable guides or fragment links to removable sections. This
consolidation adds no S1 behavior promise ahead of T2; complete-GET tightening
and its storage guidance remain T2-owned.

No auth-provider/client/call-body rewrite, new configuration/dependency/profile,
quota, queue, global scheduler, logging change, benchmark or independent PR
import. Generated protobuf/OpenAPI and listed read-only parity owners remain
outside this unit. Historical research and Definition/Design stay unchanged.

Mutable owners:
- `infra-grpc` router admission/rejection and its existing transport proof:
  `crates/infra-grpc/src/router.rs`, `crates/infra-grpc/tests/transport.rs`.
- gRPC configuration field comments only: `crates/config/src/grpc.rs`.
- Existing gRPC guides: `docs/grpc.md`, `docs/grpc-decisions.md`.
- gRPC budget prose inside existing markers in
  `docs/configuration-source-policy.md`; do not edit the storage sections.
- `docs/production-contract.md` capability-conditional resource-scope
  consolidation with only the four retained file-level link owners above;
  no new markers, D1 manifest registration or resolved service fields.
- Only if existing authn proof markers cannot contain needed auth-specific
  additions: `scripts/lib/template_profiles.json` existing authn block list;
  the accepted default is to reuse existing markers without a manifest edit.
- This packet's executor-selected proof/command notes; implementation status,
  receipts and execution identity remain root-owned in `../tasks.md`.

Exclusive locks:
- `docs/configuration-source-policy.md` whole-file edit ownership until the
  Implemented handoff. T2 starts after this writer is released.
- If its accepted conditional edit occurs, the template-profile manifest is
  exclusively owned by this unit during that edit; no other planned unit edits it.

Final validation:
- Claim: G1's pre-auth opening bound and independent terminal bound hold with
  the accepted deadline, overload/auth precedence, release, health/zero and
  projection semantics; D1 does not invent runtime capacity or import parallel
  PR behavior.
- Checks: Matching repository build/relevant tests and actual changed
  documentation/profile validation, consolidated with T2 only after all writers
  join. The executor selects cases, fixtures, commands and proving layer while
  coding; no separate approved test inventory is required. Independent final
  assembled delivery review covers these concurrency invariants.
- Observable: Capacity refusal prevents verifier/provider/handler entry;
  permitted work and retained terminal streams obey distinct lifetimes and
  recover when owners finish or cancel. Current guides and retained/pruned
  profiles agree with that behavior. This is local correctness, not a fleet,
  p99/RSS or live-provider capacity claim.

Reopen if:
A concrete result invalidates accepted admission lifetime/order, bounded
rejection, existing terminal custody or placement: Technical Design. Required
new behavior, knob/dependency/feature or changed resource scope: Definition.
Affected source/independent-PR drift refreshes only that disposition. Mechanical
test/caller/marker repairs remain with Implementation and do not add a phase gate.

## Executor notes for assembled Completion

Implementation-only coding feedback on 2026-10-05:

- `rustfmt --edition 2024` on the three touched Rust files initially could not
  resolve `rustfmt` from PATH. Re-running with
  `/Users/daniil/.cargo/bin/rustfmt` completed successfully.
- `/opt/homebrew/bin/rtk proxy bash scripts/ci/validation-lock.sh --
  /Users/daniil/.cargo/bin/cargo check --locked -p infra-grpc --tests` completed
  with exit 0 in 1m 24s under the Git-common validation lock. It checked the
  production and test types without running tests. The only diagnostic was
  the existing deprecated `Atomic::fetch_update` in
  `vendor/sqlx-core/src/pool/inner.rs:229`; that read-only owner was unchanged.
- No runtime test, matching build, lint, docs/profile check, acceptance review
  or external effect was performed. The following cases and command choices
  remain unverified input to the final delivery owner.

The existing `transport` integration test owns the new composed-router cases;
its real introspection verifier/provider fixture supplies held provider work.
A local fixture gate and request count add no production seam. Provider
connection tasks now join/shut down with the existing fixture instead of being
left detached. All additions reuse existing authn proof markers; the manifest
is unchanged.

| New case | Distinguishing behavior and existing-coverage gap |
| --- | --- |
| `opening_admission_counts_auth_followers_and_recovers_after_cancellation` | Two same-token callers exhaust K=2 across clones before auth; missing/malformed/distinct callers shed with the exact catalog identity and one counter increment each, health remains callable, an expired original deadline wins, and cancelled followers/leader release capacity. Existing held-handler saturation begins after auth and misses this gap. |
| `opening_and_terminal_capacity_have_independent_lifetimes` | A returned head frees an opening while retaining its terminal slot; another slow authentication reserves no terminal slot; authentication still precedes terminal shedding. Existing terminal-stream tests do not place slow auth between two open streams. |
| `opening_timeout_releases_capacity_and_zero_disables_both_counts` | Opening expiry releases capacity; zero admits auth while another opening waits and allows multiple retained terminal bodies. Existing deadline tests do not combine timeout recovery with a one-slot opening or zero. |
| `rejected_ready_empty_frames_cooperate_with_other_tasks` | A finite sequence of immediately-ready empty frames gives another task runtime progress before drainage completes. Existing network rejection flood waits on sockets and cannot expose a non-cooperative in-memory body. |

Reuse the existing transport tests for terminal body status/deadline/drop,
handler panic/failure, all cardinalities, health drain and rejection flood.
Do not add duplicate layer-local tests. Final validation should run the matching
repository build and relevant test route once for the assembled T1/T2 surface,
plus actual docs/profile checks selected by the delivery owner.

Retain pre-fix/post-fix evidence for the two defect discriminators above: the
follower-admission test and ready-empty-frame cooperation test. With all writers
and readers stopped, the delivery owner can temporarily substitute only
`crates/infra-grpc/src/router.rs` from the fixed
`78aa3a832bfb4d7e9632ce5ebbbf1680705c31af` baseline, retaining the candidate tests.
Save/restore the exact candidate bytes even on failure; use the existing
Git-common validation lock and existing checkout/runner. Run each selected
`cargo test --locked -p infra-grpc --test transport <exact-name> -- --exact`
against the baseline and then the restored candidate. Expected baseline
assertions are `Unauthenticated` versus `ResourceExhausted` on the saturated
missing-credential call, and absent peer-task progress during ready-frame
rejection drainage. Compile errors or unrelated failures are not regression
proof. This comparison needs no new environment, runner or dependency.
