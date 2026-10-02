# Planning: one recoverable, bounded jobs-worker delivery

Status: ready

## Fixed implementation unit

Outcome: deliver the complete B1–B5 correction as one independently acceptable
jobs-worker result: admitted capacity covers outcome bookkeeping; failed work
remains in custody; a PostgreSQL-only operator can discover and safely resolve
one inspected failed identity; publication identity is preserved; and registered
failure visibility is coherent across ordinary and publisher engines.

This is one fixed unit, **JW1**, carried inline in this result. There is no
task ledger or layer-by-layer acceptance. The schema, adapter, CLI, configuration,
profiles and guidance are companions of that outcome, not independently
deliverable tasks. Their dependency timing is internal to the unit. One fresh
Acceptance-Unit Lead implements JW1 and owns the final assembled delivery;
the existing root remains continuation coordinator. No wave or per-lane
test/review gate is created.

Source baseline: `67be869acea112af271ec8ba621cbc50ae9d36b7`.
Worktree:
`/Users/daniil/Projects/Opensource/rust-service-template-rest.codex-jobs-worker-reliability-20261002`.
Branch: `codex/jobs-worker-reliability-20261002`.
The current worktree contains the ready phase artifacts and no implementation.
Preserve those artifacts and any subsequently discovered unrelated work.

### Consumes

- [Intent](intent.md) and [Specification](spec.md) B1–B5 — ready behavior,
  authority and retained limits; gate implementation.
- [Technical Design result](technical-design-result.md), including its exact
  input identities and PASS reviews — ready technical authority; gate implementation.
- [System design](design/system.md), sections 1–6 — closed mechanism,
  resource bounds, receipt/finality, inspection and sampling contracts.
- [Ownership map](design/ownership.md), Responsibilities, Exported contract,
  Files and Non-Rust and generated file authority — exact providers, consumers,
  writable responsibilities and profile-removal companions.
- [Mechanism evidence](research/mechanisms.md) — accepted reuse and resolved
  dependency versions; no replacement-library investigation is outstanding.
- [Rollout](rollout.md), Affected graph and Ordered gates — migration order,
  mixed-worker compatibility, activation and rollback limits. Live schema,
  fleet replacement and operator actions gate only a separately authorized
  deployment/operation; they do not gate this PR's implementation or local acceptance.
- [Validation Routing](../../docs/validation-routing.md),
  [PostgreSQL Validation](../../docs/validation/postgres.md) and
  [Contributing](../../CONTRIBUTING.md) — final local/CI proof and PR ownership.
  Actual selected CI results gate completed PR delivery, not coding.

### Provides

One assembled implementation, its executor-authored regression coverage,
append-only migrations and generated metadata, correct retained/removed
template graphs, current canonical guidance, final local evidence and one
separate PR with actual selected CI receipts. Existing phase artifacts supply
the durable accepted decisions; the Lead records current execution and final
evidence in this unit's result or a linked delivery result, not a synthetic ledger.

### Boundary and obligation reconciliation

| Accepted obligation | Current-to-target work within JW1 | Canonical owner |
| --- | --- | --- |
| B1 / R1 | Replace early slot release with shared full-attempt custody; remove cancelled queued registrations and retire in-flight membership before reply visibility; retain immutable deadlines, result priority and uncertainty | System §1; ownership R1 and attempt/optional mechanical claim transfer |
| B2 / R2 | Remove failed retention duration and branch; preserve bounded completed-only 24-hour cleanup and existing live uniqueness | System §2; maintenance owner |
| B3 / R3, R4, R5, R6 | Add validated safe inspection/recovery API, history archive and version fencing, locked same-row redrive/discard and conflict/unknown outcomes; compose admitted PostgreSQL-only CLI before ordinary bootstrap | System §§2–5; exported contract, config and worker owners; rollout migrations |
| B4 / R3, R7 | Preserve all stored job/outbox identity and exact prepared bytes; preserve transactional-completion fencing and no automatic business replay; explain reconciliation and dedup horizon | Spec B4; system §§2–3, 6; current outbox owner remains unchanged in production |
| B5 / R2, R3 | Bounded keyset scan with explicit handled set and honest partial/error pages; capped failed gauge and a single process-owned union sample with last-good freshness | System §5; engine, maintenance and operator owners |
| Profile and release closure / R6 | Two new ordered migrations, checked-query metadata, manifests/markers and affected source projection proof all agree | Ownership Non-Rust and generated file authority; rollout |
| Shipping contract and cleanup / R7 | Replace contradictory current claims; document safe commands, limits and activation/rollback boundary in canonical docs | Ownership R7 and documentation list below |

Static lease, one publisher slot and combined failure domain are retained
with their accepted reopen conditions. No new runtime, broker topology,
administration transport, generic queue framework, dependency version upgrade,
schema repair, bulk/force action, payload editing, merge, deployment or live
queue operation belongs to JW1. There is no additional audit finding silently
deferred as companion implementation.

### Mutable owners and exclusive locks

The Lead owns all JW1 writes and may delegate a strict disjoint subset after
consulting the ownership map. The exact Rust file/declaration map remains in
[ownership](design/ownership.md); it is not duplicated as a guessed file inventory.

- Infra-jobs attempt custody, maintenance/engine sampling, public operator
  module, crate exports and their colocated tests. Shared infra-jobs exports,
  maintenance session check and process-peer contract are one mutation lock;
  coordinate them serially if lanes share this crate.
- Config jobs projection, generic loader integration and jobs-owned exports,
  including tests. These files form one config mutation scope.
- Jobs-worker CLI/operator lifecycle, central dispatch/exit mapping and process
  tests. The worker binary composition and its Cargo manifest are one scope.
- Existing jobs integration suite and registration in `test/tests/jobs/main.rs`;
  its shared fixtures/module registration are exclusive. Outbox colocated tests
  may change only if they are the smallest missing B4 proof owner.
- Append-only migration chain and migration README: one exclusive schema owner.
  Workspace/package dependency graph and Cargo.lock: one exclusive manifest owner;
  only already selected direct clap/serde_json edges are planned.
- `.sqlx/`: one generation owner and lock, consuming stable canonical schema
  and checked statements; no concurrent query/schema mutation during preparation.
- Template manifest, profile markers and projection expectations: one profile
  integration owner/lock, consuming the integrated source graph. Generated
  profile output is never edited as source.
- Canonical jobs/outbox/architecture/config guidance and this unit's execution
  result: assigned exclusive document scopes. All lanes join before final
  validation; CPU-heavy validation and generation use existing shared locks.

Do not introduce extra worktrees for sequential or cheap disjoint work. A
discovered mechanical writable overlap is resolved by serial ownership and
recorded in the existing execution result, not a new acceptance unit.

### Implementation dependency timing and first frontier

JW1 is ready now. There is no missing product decision, provider runtime or
external authority needed to begin. First establish the accepted provider
types and config projection and implement the closed custody/retention/sampler
changes; these can start from the existing tree and design. The Lead chooses
whether disjoint lanes save work. No provisional producer needs to pass tests
or review before its implemented contract is consumed.

1. Give schema and manifest edits one owner. Choose two current UTC versions
   greater than `20261002150000` and the actual base maximum, history column
   first, concurrent failed-kind index second. Keep old migrations immutable.
   Implement provider SQL against that schema and maintain the exact exported
   contract used by the CLI. Config projection and internal attempt custody
   need no live provider prerequisite.
2. Worker command composition consumes the agreed API/config contract; it may
   be authored from that closed contract before provider bodies land. Integrate
   those declarations before compile diagnostics need them. Keep ordinary
   no-subcommand startup and loader-only messaging projection valid.
3. Once canonical migrations and all changed checked SQL are stable, regenerate
   `.sqlx/` through the existing preparation owner and remove obsolete metadata
   through that generator. Schema availability for generation is an
   implementation prerequisite at this point, not a prerequisite to unrelated
   code. Missing required generation is not completed implementation; retain
   the exact capability gap and continue independent work.
4. Integrate manifest removals and source markers for new migrations, operator
   modules, config exports, CLI variants, dependency edges and proof modules.
   Keep clap/loader-only CLI for messaging-only workers; outbox keeps jobs.
   Consume final source paths before finalizing projection expectations.
5. Finish canonical documentation and executor-selected tests with the code,
   remove superseded paths/claims, reconcile all companions, then join every
   writer. Only this assembled boundary starts final validation and review.

These are dependency facts within JW1, not schedulable tasks or test waves.
Test cases, fixtures, assertions, proving layers and exact commands are chosen
and recorded by Implementation while coding. Existing nearest falsifiers are
design evidence, not a newly mandated scenario matrix.

### Canonical documentation and cleanup

Update `docs/background-jobs.md`, `docs/postgres-transactional-outbox.md`,
`docs/architecture/{async,runtime-lifecycle,persistence,boundaries}.md`,
`docs/configuration-source-policy.md`, affected crate rustdoc and
`migrations/README.md` under their existing profile markers. Replace early-slot,
seven-day-failed-retention, multiple-sampler and universal positional-refusal
claims. Describe safe receipt outcomes, permanent discard before its example,
explicit fleet-kind union, partial page continuation, history/storage custody,
at-least-once reconciliation, static lease, single publisher slot and combined
failure domain. State the all-old-retention-owners-stopped activation gate and
unsafe old-binary rollback. No unrelated roadmap stage is completed here.

Remove superseded failed-deletion and sampler branches, obsolete ownership
comments and stale generated metadata; do not leave a dormant legacy path.
No temporary runner/framework is needed. Preserve phase decisions and final
receipts through handoff; bundle cleanup follows the repository Cleanup owner
only after completed delivery, not during Planning.

### Final validation and delivery ownership

Claim: the assembled JW1 satisfies B1–B5, its shipped operator path composes
without unrelated providers, and retained/removed template profiles contain a
consistent source/schema/metadata graph. Local proof, CI proof and runtime
deployment are separate claims.

Checks: one Lead selects a non-overlapping final plan under the repository
budget. This multi-crate/manifest change uses the matching build and relevant
workspace tests, documentation consistency/docs-check, applicable static
migration/dependency checks and generated-source closure. Reuse valid scoped
evidence. Do not multiply complete builds across unchanged dimensions, add
per-task review, or run full/heavy aggregates merely for confidence.

Existing CI-owned database integration, SQLx drift, migration rehearsal,
source-initializer and any other actually selected CI gates remain real PR
gates. Their actual selected run must pass; local mocks or design review do
not establish transaction, lock, uniqueness or migration observations. Use the
current classifier/workflow for exact job selection. No live queue query,
lost-ack proxy, benchmark, new test environment or local duplication of the
CI-owned matrix is required by Planning. Missing optional evidence is disclosed;
missing required proof remains explicitly incomplete.

Observable: normal due work stays unclaimed when full attempt capacity is
occupied; failed work remains and can be inspected through PostgreSQL alone;
one admissible inspected version is redriven/discarded once with honest safe
outcomes; preserved outbox bytes and one complete registered-kind sample remain
consistent with the canonical guides. Evidence is reported at the exercised
boundary, never as a deployed fleet or recovered production event.

The same Lead owns one fresh independent final assembled delivery review for
changed concurrency/data integrity under shared Review, including cross-surface
interactions. Repair anchored defects and rerun only invalidated evidence.
No unit or lane gets a separate review/acceptance gate.

Human authority permits scoped commit/push and one separate PR. The delivery
owner follows External Effects before those actions and Contributing's draft
policy: a draft's cheap-only CI is not the complete selected release proof;
mark the completed candidate ready for review so its selected heavy gates run.
Report the exact PR/head and actual CI state; retain pending gates until their
result is known. Do not merge, deploy or perform live queue operations.

### Reopen if

Return to Technical Design for a concrete inability to enforce accepted
custody/order, API ownership, query bounds, finality or profile closure; to
Specification for changed behavior/retention/effect identity; to Intake for
changed requester meaning or missing external authority. Mechanical names,
test design, diagnostic failures and ordinary implementation repairs remain
with the Lead. Planning reopens only if a real independently acceptable outcome
or dependency boundary makes this single-unit ownership invalid.

## Planning readiness

Author dry run: every B1–B5/R1–R7 obligation has one implementation owner;
provider/config contracts are available now; canonical schema precedes generated
metadata; source integration precedes final profile closure; rollout runtime
gates consume no unauthorized action in the PR task. JW1 is independently
acceptable as the whole requested correction. No layer-only task or missing
technical choice is hidden at the first frontier. Independent
[Task Review / Readiness](planning-review.md) returned PASS with no findings.
Only status/readiness and this transition receipt changed after review; the
fixed unit's semantic scope is unchanged. Planning stops before Implementation.

## Transition Result V1

```text
status: ready
owner: Planning
result: specs/jobs-worker-reliability/planning-result.md#fixed-implementation-unit
review: specs/jobs-worker-reliability/planning-review.md (PASS)
movement_evidence: JW1 reconciles all B1-B5/R1-R7 obligations as one coherent
  fixed implementation unit; closed inputs, writable scopes, dependency timing,
  generated/profile custody and one final delivery owner are independently
  reviewed; no required input or surviving finding blocks the first frontier
reopen_owner: none
next_owner: Implementation
```

The continuation coordinator may dispatch a fresh Acceptance-Unit Lead for
JW1 in the named worktree. That Lead begins at
[Implementation dependency timing and first frontier](#implementation-dependency-timing-and-first-frontier),
owns the assembled final validation/review and returns the delivery result to
the coordinator. It creates no synthetic ledger transition or per-lane acceptance.
The Planning actor has performed only artifact edits and static reads, including
a relative-file-link check; no build, runtime validation, production change,
commit, push or live operation occurred. Repository docs-check remains final
delivery evidence. The read-only reviewer has completed; no descendant writer
or reader remains active.
