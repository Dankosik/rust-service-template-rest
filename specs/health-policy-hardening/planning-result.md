# Health-policy hardening: Planning result

Status: ready

## Inputs and execution carrier

[Intent](intent.md), [Specification](spec.md), [Definition result](definition-result.md),
[Technical Design](design.md), and [Design result](design-result.md) are ready.
The specification and design have independent PASS reviews. Their current
SHA-256 identities are respectively
`0e36e32ef020b8f7aee5aa4012aced2ef8aec761834fbb862945a470d50b8c56` and
`e1484f9c1a70971fd0f8b5abbf63c9f167c4c64ec41b220ae8af0c15a4093be0`.

Working checkout:
`/Users/daniil/Projects/Opensource/rust-service-template-rest.codex-health-policy-hardening-20261005`;
branch `codex/health-policy-hardening-20261005`;
base `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
Current workflow owners are this checkout's [AGENTS.md](../../AGENTS.md),
[Implementation](../../docs/spec-first-workflow/phases/implementation.md),
[Transition](../../docs/spec-first-workflow/shared/transition.md), and
[Codex adapter](../../docs/agent-harness/codex.md).

Use one fixed unit below, carried by one fresh general-purpose Acceptance-Unit
Lead dispatched by the existing root continuation coordinator. The Lead owns
implementation, assembled final validation, repairs, and local acceptance.
The root retains continuation and the requested one-PR delivery outcome; it
does not substitute its own acceptance. There is no independently schedulable
handoff, intermediate release, or multi-unit execution state needing a ledger.
This receipt preserves the fixed unit across the phase boundary; no separate
task packet, tasks.md, or synthetic task-status transition is needed.

## Fixed unit: truthful health policy through failure and startup silence

**Outcome.** Deliver the coherent R1–R4 repair in the existing health and pooled
PostgreSQL owners, with focused regression coverage and matching operating
guidance, so failed recovery cannot revive stale Ready and startup admission
cannot wait indefinitely on session readback or rejection cleanup.

**Current-to-target delta.** Implement the mechanisms already closed in
[Design](design.md#decisions-and-material-flows):

| Accepted obligation | Implementation delta and final observable |
| --- | --- |
| R1 | The existing failure fold checks previous publication freshness at new completion. Stale Ready immediately becomes the ordinary failed verdict; fresh threshold smoothing, equality freshness, success reset and drain precedence remain intact. |
| R2 | The existing publication owner emits last-completion Unix timestamp and stale-bound gauges. Completed failures count, cancellation/drain alone do not; documented zero/NaN/clock/scrape limits allow operators to interpret stopped refresh without changing endpoint authority. |
| R3 | Existing pooled admission bounds complete session verification to 5 seconds, returns its typed sanitized timeout, and bounds rejection cleanup to 5 seconds while retaining the original rejection. Only verified success returns the native pool; cancellation adds no detached work. |
| R4 | Existing documentation agrees on freshness, metrics, sequential admission budgets, detection estimates and listener/platform limits, without claiming fleet guarantees or changed runtime policy. |

These are parts of one accepted health-policy outcome, consumed together as one
PR. Splitting code, proof, or guidance into tasks would leave companion work
required for that outcome. No new product behavior, mechanism, or ownership
choice is left to Planning or the executor. Other research recommendations
retain the dispositions in [Specification](spec.md#recommendation-dispositions).

**Writable ownership.** The Lead has exclusive mutation ownership of the
[Design inverse file map](design.md#responsibility-and-inverse-file-map):
`crates/health/src/lib.rs`, `crates/infra-postgres/src/pool.rs`,
`test/tests/postgres.rs`, and the four named existing documentation owners.
`test/tests/support/commit_proxy.rs` is writable only if the chosen proof needs
a narrow refinement of the existing relay. Code and colocated tests share the
same owner; any delegated lanes need disjoint writers and joined completion.
All accepted phase inputs and unrelated work remain preserved. Mechanical
caller/file consequences follow the design's map-update rule; changed behavior
or mechanisms reopen their smallest upstream owner. No generated contract,
config default, dependency, vendor patch, migration, or infrastructure change
is planned. Canonical source edits precede any required derived output; the
accepted design identifies no generated output to change.

**Dependencies and authority.** Implementation consumes the ready inputs above
immediately and has no unavailable code, product decision, service, or external
input prerequisite. Work stays in this isolated checkout; the original dirty
checkout is outside its writable scope. Scoped code, tests, docs, validation,
commit, push and one separate PR are authorized. Merge, deployment, production
reads and infrastructure changes are excluded. Before a remote write, the
acting delivery owner applies existing contribution and external-effect owners
and their gates; passing local checks never substitutes for that gate.

## Completion and proof boundary

Test cases, fixtures, assertions, proving layers and exact commands belong to
the executor while implementing this fixed unit. Reuse adequate proof and add
only material missing coverage for the accepted deltas. No preliminary test
plan or test review is required.

After all unit code, tests and documentation are assembled and all writers have
joined, the Lead runs one consolidated final-validation plan under the current
repository route for the actual changed surfaces. It covers the matching build,
relevant tests and documentation consistency, preserving required existing CI
gates without duplicating broad runs or multiplying a matrix. Observed
PostgreSQL behavior requires the existing persistence/database validation owner
and real-database evidence; use the existing harness or its CI gate, and keep
that empirical claim pending until observed. This plan adds no local
infrastructure, extra full-repository gate, or fleet qualification.

One fresh independent [Implementation Review](../../docs/spec-first-workflow/phases/implementation-review.md)
is selected for the final assembled candidate because the changed readiness,
cancellation and cleanup behavior materially affects correctness and concurrency.
There is no per-task review or verification gate. Repair in-scope failures and
blocking review findings, and recheck only invalidated proof.

Local acceptance requires R1–R4 implemented, the repository-selected local
checks passed and the final independent review resolved, with no known in-scope
defect. Missing required evidence is incomplete verification, never PASS.
The requested one-PR outcome additionally requires commit/push/PR delivery and
its applicable CI results, recorded separately from local proof. No result here
establishes deployed behavior or authorizes merge/deployment.

The Lead records its assembled candidate, actual commands/results, review and
material limitations in the task's Implementation completion result. Planning's
ready status closes only these planning decisions, not that implementation.
The coordinator consumes the result and continues the already-authorized
remaining delivery work. No unresolved user-owned input remains.

## Readiness walkthrough and reopen

The Lead can enter through this fixed unit, load its ready spec/design and the
current Implementation owner, implement at the named source owners, choose
matching proof, and update the existing guidance. The consumed contracts,
write scope, proof boundary and external-effect limits are all named. Future
CI/database observations gate their claims and relevant delivery action; they
do not prevent coding from the closed inputs. Preflight reported pinned Rust
1.99.0 and Docker available; Cargo needs `/Users/daniil/.cargo/bin` on PATH.
These are capability notes, not passing validation receipts.

Reopen Planning only for an invalid unit/completion boundary; Technical Design
for invalid mechanisms or non-mechanical ownership; Specification for changed
observable behavior or an incompatible admission bound; Intake for changed
requester meaning or external authority. Routine coding, fixtures and command
selection stay with the Lead.

## Independent Planning review and Transition

Reviewer: `/root/health_planning/planning_review`, fresh `reviewer-agent`,
native `gpt-6-astra` / `high`, `fork_turns: none`. Native dispatch accepted those
settings and returned that identity; no fallback was used.

```text
candidate: planning-result.md SHA-256 685b237c885154654c71bc4b7727d4cfc99752c5cd6272e8b5e6522301ad1aac
verdict: PASS
findings: none
evidence_boundary: Independent written walkthrough of the fixed Planning result, ready intent/spec/design and review receipts, current Planning/Readiness/Implementation/Transition/Evidence Contract/Codex owners, named-file availability and ownership/proof boundaries. No live checks, build, tests, writes or external action.
reopen_owner: none
```

The reviewer checked unchanged hashes before and after review for all six
consumed task artifacts. Its attempted falsifiers found no invalid split,
omitted R1–R4 obligation or file owner, unavailable implementation input,
competing custody, chat-only decision, premature verification gate, missing
claim-specific proof boundary, or expanded external authority.

After PASS, only the status and this review/Transition receipt changed; the
fixed unit and its meaning remain unchanged. The unchanged-scope rule preserves
the review. Planning wrote only this result; its 13 relative links and named
fragments resolved. This static check is not `make docs-check`. No build, tests,
services, production/test/general-documentation edits, commit, push or PR ran.

```text
status: ready
owner: Planning
result: specs/health-policy-hardening/planning-result.md, Fixed unit
review: Independent Planning review above, PASS
movement_evidence: One coherent fixed unit closes R1-R4 ownership, consumed inputs, final observable outcome, carrier, proof timing and delivery boundary; a fresh independent readiness review found no material gap.
reopen_owner: Planning for an invalid unit/completion boundary; Technical Design for mechanism/ownership; Specification for behavior/budgets; Intake for requester scope or external authority
next_owner: Implementation, one fresh Acceptance-Unit Lead dispatched by the existing root continuation coordinator
```
