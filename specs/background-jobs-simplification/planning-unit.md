# Fixed implementation unit — simplified durable jobs

Status: ready (independent Task Review / Readiness: PASS)

Outcome:
Replace the existing jobs lease/upkeep/attribution path with the accepted static
lease and supervisor-owned outcome path, including its storage, public APIs,
observability, migration and adopter contracts, in one separately reviewable PR.
These are layers of one replacement: none is an independently supported release
while its companion remains unimplemented. There is one acceptance unit, no
task ledger, and no intermediate test or review gates.

Consumes:
- [Intent](intent.md) — desired outcome, one-PR authority, no merge/deploy.
- [Definition result](definition-result.md) and [specification](spec.md) — ready
  accepted behavior and preserved invariants.
- [Technical Design result](technical-design-result.md) — independently passed
  mechanism and ownership decisions.
- [System design](design/system.md) — exact API, SQL, deadline, propagation and
  sampling contracts; all sections are consumed by this one unit.
- [Ownership map](design/ownership.md) — existing responsibility owners and
  deletion/replacement surfaces.
- [Rollout](design/rollout.md) — documented stopped-producer/worker conversion
  and forward-compatible recovery; live execution is outside this unit.
- [Implementation](../../docs/spec-first-workflow/phases/implementation.md),
  [Codex harness](../../docs/agent-harness/codex.md), and
  [validation routing](../../docs/validation-routing.md) — current execution,
  feedback, assembled proof and CI ownership.

Provides:
- The complete supported jobs replacement, its code and tests, coherent active
  documentation and profile removal inventory, and one PR with exact-candidate
  applicable local and CI results.

Boundary:
Implement specification obligations 1–14 and the preserved behavior in its
proof boundary. Remove the superseded mechanisms and contradicted active
documentation. No new dependency, lifecycle crate, queue framework, webhook or
outbox product, generic business-closure replay, HTTP-idempotency redesign,
live migration, data repair, merge or deployment is included.

## Obligation reconciliation

| Accepted obligations | Current-to-target delta and owner |
| --- | --- |
| 1–5: leases, uncertainty, shutdown and counters | `claim.rs`, `attempt.rs`, `engine.rs` and worker shutdown replace upkeep/readback/registry with acknowledged-only admission, immutable deadlines, fixed fenced outcomes and supervisor result precedence; delete `lease.rs`. Observed attempt/cancellation metrics never claim exact durable attribution. |
| 6–7: completion, retry-after and snooze | `kind.rs`, `attempt.rs`, `enqueue.rs` and `lib.rs` provide the exact designed checked APIs, caller-connection transaction completion, propagated stale rollback, no-transition transactional uncertainty, database-time delay and once-only refund. Preserve ordinary handlers. |
| 8–9: JSONB/text and migration | `enqueue.rs`, `maintenance.rs`, forward migration and migration/adopter docs enforce UTF8 and pre-SQL NUL/size/key/delay validation, semantic JSON, exact text-key equality, unchanged historical migration, atomic compatible conversion and intact refusal. |
| 10: propagation | `trace_context.rs` replaces `traceparent.rs`; installed propagator plus explicit TraceState validation preserves bounded parent/state links and legacy rows, with no baggage. |
| 11: bounded observations | `maintenance.rs` supplies cap 1000/censoring, indexed due age, unavailable/zero/failure distinction and freshness contract; remove the unregistered aggregate. |
| 12: claim lock footprint | `claim.rs` materializes at most the selected batch IDs before locking, rechecks/guards generation, preserves disjointness and accepts documented contention underfill/fairness limits. |
| 13: reuse and consolidation | `claim.rs` returns PostgreSQL randomness; `attempt.rs` keeps fixed jitter arguments; `kind.rs` exports reusable const validation while retaining typed runtime rejection; enqueue test owner maps and retains every distinct e6 behavioral oracle before consolidation. |
| 14: future reuse and lifecycle disposition | Roadmap 10.5/10.6 and architecture/adopter docs adopt infra-jobs scheduling/attempt/completion reuse. Keep current process lifecycle owners; record the accepted extraction reopen condition. No new lifecycle implementation is needed. |
| Preserved behavior and companions | Existing queue tests/fixtures, worker callers, profile inventory, crate API docs and architecture leaves change with their real owner. Retain enqueue isolation/transaction fate, uniqueness, unknown-kind isolation, fencing, attempt cap, backoff, retention and profile inertness. |

## Mutable owners and useful lanes

The Acceptance-Unit Lead owns this whole result and serial integration. The
following disjoint writable scopes permit useful parallel coding; they are
subtasks, not independently accepted units or a mandatory fan-out. Writers
choose/write tests alongside their implementation. The Lead assigns a scope
before mutation, never writes concurrently in a delegated scope, and joins all
writers before final validation.

| Scope | Writable owner | Stable seam / handoff |
| --- | --- | --- |
| Core Lead | `crates/infra-jobs/src/{kind,claim,attempt,engine,maintenance,lib,lease}.rs`; `crates/jobs-worker/src/`; `test/src/jobs.rs`; `test/tests/jobs/{execution,process,main}.rs`; unit-local source tests/docs in these files | Owns all public exports, handler/supervisor integration, startup checks and shared fixture changes. Consumes checked delay conversion from enqueue and trace carrier API. Deletes lease module. |
| Storage lane | `crates/infra-jobs/src/enqueue.rs`; new `migrations/20260925000001_simplify_background_jobs.sql`; `test/tests/jobs/{enqueue,http_idempotency}.rs` | Accepted design closes delay domain, pre-SQL validation, text casts and forward conversion. Supplies crate-private checked delay conversion; agree its Rust name/signature with Lead before dependent callers consume it. Only this lane writes the new migration; never edits the old file. |
| Trace lane | Existing `crates/infra-jobs/src/traceparent.rs` and its replacement `trace_context.rs`, including adjacent tests | Supplies bounded capture/link functions under the designed installed-propagator contract; Lead alone updates imports/module declarations. Agree Rust function names with Lead before consumption. |
| Documentation/profile lane | `docs/background-jobs.md`, `docs/architecture/{async,persistence,runtime-lifecycle}.md`, `docs/roadmap.md`, `migrations/README.md`, `scripts/lib/template_profiles.json` | Writes accepted contracts and examples from ready design; both jobs migrations disappear when jobs=none. Reconcile example names with assembled public exports before returning. Does not write Rust crate docs or fixed phase inputs. |

Semantic dependencies are closed now. Storage, trace and documentation can start
from these contracts while the Lead implements the core. An implemented helper
or final public export is consumed when compiling callers/examples, not an
upstream approval or passing-test dependency. Shared fixture/caller repairs go
to their named writer. A newly discovered file/lock overlap is serialized by the
Lead; it does not manufacture another task or reopen closed design.

Exclusive locks:
- Public infra-jobs contract/module wiring and shared jobs test fixture: Core Lead.
- Jobs migration chain: Storage lane, only the named forward file.
- Profile removal inventory: Documentation/profile lane.
- Shared validation lock: delivery owner only after writers have joined, under
  existing repository validation rules; no concurrent CPU-heavy commands.
- No manifest, toolchain, generated carrier or new CI gate mutation is planned.

## Final validation

- Claim: the assembled code satisfies the specification's complete jobs
  replacement and preserved behaviors, removes obsolete machinery, and keeps
  supported jobs profiles/docs consistent.
- Checks: matching build and relevant tests under AGENTS.md; use mixed-surface
  validation routing for the Rust/SQL/profile/docs delta. Existing PostgreSQL
  and CI owners govern the accepted transaction, migration, lock/query and
  profile evidence. Implementation selects/writes cases and records final
  commands; Planning adds no test matrix or per-lane run. Keep heavy database,
  migration, profile and image gates in CI, without duplicating them locally.
  One fresh final integrated review covers the changed concurrency,
  transactional, shutdown, migration and cross-lane behavior after assembly.
- Observable: applicable local checks pass, blocking final findings are
  resolved, and the separate PR has applicable passing CI for its exact head.
  Local proof, CI results and absent live rollout evidence remain distinct.
  Required failures stay incomplete and return to their implementation owner;
  optional environments add no invented gate.

Reopen if:
Implementation evidence invalidates a named mechanism: reopen System Design
only for that decision; reopen Definition only if accepted behavior must change.
Missing test technique, helper names, caller repairs or routine SQL/Rust coding
choices remain Implementation work. New external effect or business data repair
requires its own authority; this unit stops before it.

## Written readiness walkthrough and carrier

The fresh Lead reads the ready behavior/design and existing source owners, then
assigns any useful disjoint scopes above. Accepted APIs and SQL/data contracts
permit coding immediately, without a live provider or test environment. Helper
names and module integration stay with their listed writer; no writer requires
another's test receipt. The assembled supervisor, storage and trace code then
consume those implementations; documentation/examples and profile inventory
are reconciled with the actual final tree. All code/test-writing and deletions
finish before the one final validation/review boundary. The PR is the requested
external result; merge/deploy never become implied completion actions.

Execution carrier: a fresh native general-purpose Acceptance-Unit Lead under
the current root continuation coordinator, using Implementation and this fixed
unit. No Ledger Orchestrator binding or synthetic ledger is needed. The Lead
may dispatch bounded disjoint lanes through the Codex harness and retains their
identities, integration and final delivery ownership. Root routes continuation
and consumes the final result; it does not accept a partial layer.

At proven closeout, apply [Cleanup](../../docs/spec-first-workflow/shared/cleanup.md).
Active guides/architecture/roadmap retain durable decisions and reopen
conditions. Remove completed execution-only planning state and the bundle once
no live owner needs it; Git/PR retain provenance. Do not remove a dirty worktree
or delete this accepted input while implementation/review still consumes it.
