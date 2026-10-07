# Planning transition

```text
status: ready
owner: Planning
result: specs/transport-resilience/tasks.md and its linked packets
review: specs/transport-resilience/planning-review.md (PASS; no findings)
movement_evidence: Every accepted obligation has one independently consumable
  task, closed input, writable owner, final observable and dependency timing.
  Shared delivery/dependency carriers have one exclusive writer. The fixed
  candidate passed fresh independent Task Review / Readiness; only lifecycle
  status changed afterward.
reopen_owner: Planning for invalid unit/ownership/dependency boundaries;
  Technical Design for unavailable native mechanism or non-mechanical ownership;
  Definition for changed/excluded behavior, scope or compatibility
next_owner: Implementation through the existing root as Ledger Orchestrator
```

Workspace: `/Users/daniil/.codex/worktrees/transport-resilience/rust-service-template-rest`.
Branch: `codex/transport-resilience-20261005`.
Base: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.

Current SHA256 identities:

- Ready ledger: `6172f94ec8ba4fd953af45feed883f1ff3f9219e58ff2b5d8ae3768379044406`.
- Planning review: `d011ac369d4a0429fbce309fc10781fcc1f84a0905949716b17c97c1df792430`.
- Packet identities are unchanged from the [review receipt](planning-review.md).
- Accepted design: `16eaf1629936d1f66a73dc6011760cca7322cc275a74cabda09293ad16bd9545`.
- Accepted specification: `cbd092c9c29e462424a43324454ae4e1cc72089f0b50fdb7b5e15e902ad6d51f`.

Current worktree AGENTS and workflow owners governed Planning; Codex
collaboration remains the authorized carrier. The existing root is the sole
continuation coordinator and may bind as Ledger Orchestrator/sole ledger
writer. It dispatches ready unit Leads through Implementation and records
their native Execution locators. This Planning actor and its fresh reviewer
have finished; no Implementation work was performed here.

## Ready frontier and writable owners

All six tasks consume accepted design and existing source, with no missing
business input or external implementation gate:

- T1 owns PG Dsn/SQLx TCP repair and persistence guidance.
- T2 owns native gRPC lazy connection, crate dependency and gRPC guidance.
- T3 owns auth provider connection sub-budget and authentication guidance.
- T4 owns native NATS attempt/close, messaging config/consumer wiring and all
  messaging vendor/profile/Docker/classifier companions.
- T5 owns Smithy propagation and all object-storage vendor/profile/Docker/
  classifier companions.
- T6 owns documentation corrections for preserved HTTP and Redis transports.

T1/T2/T3/T6 and either T4 or T5 may begin immediately. T4/T5 serialize the
`dependency-profile-delivery` lock through one assembly writer; this is a
writable-resource constraint, not an invented semantic prerequisite. Reusing
the same Lead for these related units after writers join is allowed. Any T2
root-lock adjustment is handed to that same writer. Refill the ready frontier
as resources release, without per-task proof/review gates. Exact scopes and
source-to-carrier order live in [execution boundary](tasks/execution-boundary.md)
and packets, not in chat history.

## Final validation and publication

One delivery owner validates after all six tasks are Implemented and writers
have joined: matching assembled build/workspace tests, documentation and
applicable selected local routing, real-database evidence for observed PG
claims, and one fresh independent review of the assembled delivery candidate.
Executors choose and write tests with their implementation; no preliminary test
plan or separate verification task is required. Keep CI-owned heavy
integration/image/initializer checks on the authorized PR and record their
actual scope/state. Do not multiply full builds across task/profile/harness
dimensions or create infrastructure solely for completion.

The root retains commit/push/separate-PR publication authority and responsibility.
No merge or deployment is authorized. No code, tests, dependency/config carrier,
build, container, database or external effect was changed/executed in Planning.
Static path/fragment and whitespace checks passed for the plan; docs-check and
runtime/CI proof remain with the assembled execution boundary. This is a ready
plan, not an implementation or runtime acceptance claim.
