# Planning execution boundary

Authority: [specification](../spec.md), [selected design](../design/transport.md)
and its [reviewed transition](../technical-design-transition.md). This is packet
support, not another implementation unit. Status and dependencies live only in
[tasks.md](../tasks.md).

## Unit choice and obligation closure

The provider corrections are independently consumable outcomes. Each keeps its
native mechanism, adapter/config consumers, applicable delivery carriers, tests
and changed operator contract together. Native patches, config plumbing and
test execution are not separate ledger tasks. The untouched HTTP/Redis
documentation correction is independently consumable and needs no code change.

| Accepted obligation | Unit |
| --- | --- |
| PG literal IPv6, native TCP race, resolver-order failure, preserved return/LISTEN/finality and accurate rotation guidance | T1 |
| Lazy full gRPC dial bound, same-client recovery, existing stream/deadline semantics, keepalive/rotation guidance | T2 |
| Auth 2 s connect within 3 s total, native fallback and retained security/body policy | T3 |
| NATS per-server deadline, candidate shares, blocking trust load, native forced close/completion, raw Subscriber lifetime, trusted discovery/TLS-first, config and carrier closure | T4 |
| Smithy TCP timer propagation, SDK classification/retry/finality, vendor carrier closure, streaming/rotation guidance | T5 |
| Preserved outbound HTTP and Redis behavior; correction of DNS/live-socket/idle/rotation claims | T6 |
| No excluded fallback item, no new resolver/retry layer/provider migration, no upstream version upgrade | All, through the accepted design |

## Writable ownership and integration

The existing root may bind as Ledger Orchestrator and remains the sole ledger
writer. Each task has one Lead; the Lead may use disjoint implementation lanes
and remains responsible for its whole postcondition. No lane receives a
separate acceptance or validation gate. Preserve all other active work.

T4 and T5 share the exclusive `dependency-profile-delivery` resource: root
`Cargo.toml`, `Cargo.lock`, `.dockerignore`, `build/docker/Dockerfile`,
`scripts/lib/template_profiles.json`, `scripts/ci/changed-surfaces.sh`, and
existing shared initializer/classifier fixtures. One designated assembly writer
owns those files at a time. Serialize their task-owned carrier slices; reuse
the same Lead for T5 after T4's writers join if that is the cheapest handoff.
Neither source vendor directory is shared. All remaining packet scopes are
disjoint. A newly discovered shared fixture or manifest is reserved before
mutation; it is not permission for concurrent edits.

All six units have closed implementation inputs. T1/T2/T3/T6 and whichever of
T4/T5 holds the assembly resource can begin immediately; release and refill
the frontier as writers finish. There is no artificial code dependency between
the two vendors. Do not persist waves or wait for passing tests/review to start
another ready task.

The carrier writer obtains each checksum-verified published source and records
its upstream archive identity/revision, licenses, exact runtime diff and
retirement in the vendor's PATCHES.md before applying the native repair. Its
task's root patch, workspace exclusion and source-selection lock update use
the accepted dependency workflow; canonical profile markers/removal metadata
then drive Docker and initializer custody. Preserve pinned versions/features.
No derived profile may retain a vendor whose provider is absent. T2's own
crate manifest promotes the already-resolved hyper-util dependency; only the
shared assembly writer touches root lock data if that promotion requires it.
Such a lock adjustment is integration work for T2, not a new task or gate.

## One assembled completion boundary

After every task is Implemented and all writers have joined, one assigned
delivery owner covers all packet claims in a non-overlapping validation plan.
Choose concrete tests/fixtures/assertions/commands while implementing; add the
needed execution details to the owning packet for this final owner. Follow
[Implementation](../../../docs/spec-first-workflow/phases/implementation.md)
for bounded coding feedback rather than per-unit test runs.

The multi-crate/manifest/lock surface selects the repository's matching build
and workspace tests. Documentation selects docs-check. Consult current
[Validation Routing](../../../docs/validation-routing.md) and the real changed
surface classifier for other applicable local leaves; do not repeat covered
checks or multiply builds by provider, profile or harness. Private vendor
mechanism tests may use their existing harness when needed. All Cargo execution
is locked. Observed PostgreSQL claims use the existing persistence/database
proof owner. CI-owned heavy integration/image/initializer proof is obtained
from this PR's selected gates, without a duplicate local heavy matrix.

No additional local full-repository gate, infrastructure, standalone runner,
live-provider certification or deployment is required. Missing required proof
remains incomplete; optional missing runtime evidence is disclosed at its
actual scope. Existing finality, trust and concurrency invariants require one
fresh independent review of the final assembled delivery candidate. Repair
within the owning task and repeat only invalidated proof/review reasoning.

The root owns authorized commit/push/separate-PR publication after its external
effect requirements are satisfied and records actual CI state against the
published candidate. Local acceptance and external publication remain distinct;
the ledger is done only when its Completion is established. No merge/deploy.
