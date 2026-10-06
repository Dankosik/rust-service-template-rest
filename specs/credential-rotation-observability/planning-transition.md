# Planning transition

```text
status: ready
owner: Planning
result: specs/credential-rotation-observability/tasks.md and its four linked packets
review: specs/credential-rotation-observability/planning-review.md — PASS
movement_evidence: closed upstream decisions map to four coherent units; fresh Task Review passed their dependencies, custody, locks and single assembled delivery boundary
reopen_owner: none
next_owner: Implementation through the current root Ledger Orchestrator
```

## Candidate and custody

Checkout:
`/Users/daniil/.codex/worktrees/credential-rotation-observability/rust-service-template-rest`.
Branch: `codex/credential-rotation-observability-20261006`.
Base/HEAD: `699887b18594088a59bcc23a049d290d089f6da1`.
The independent PR targets main; PR #247 is not an implementation dependency.

| Ready result | SHA256 |
| --- | --- |
| [Ledger](tasks.md) | `f07cdb229bcd5cff9df5b4f34b94435ca37768e4fe87caebf34e4b53f6ecbe90` |
| [T1](tasks/T1-file-refresh-observations.md) | `5bc41523abf9919972435cbc06e1c2f44ea410c05cb1aeca5a78b162f4cf4d2f` |
| [T2](tasks/T2-jwks-acquisition.md) | `051908df1c98c916e5e94cf2946c49a2fc9eeaa5192d604840d00214baf99daf` |
| [T3](tasks/T3-valkey-authentication.md) | `39cda8c6eb75f1ef97d2dc1d207a9d45318958a079b3994d20b1e86ee5f4fc14` |
| [T4](tasks/T4-nats-authentication.md) | `c8d67c7d893cd4a78265375b6b654f2106095f6cba27bdfac610e4977ba5b8a2` |
| [Task Review](planning-review.md) | `0c7887daf9a62b279fef1abdf6faaa6adec4ca449c9dd32c77226690364d0c55` |

Review fixed the draft ledger; ready promotion changed only its lifecycle line.
All four packets retain their reviewed bytes. The sole review child
`/root/credential_followup_planning/task_review` returned its final result; no
descendant retains writable scope or runtime work.

Accepted upstream inputs remain unchanged:

| Artifact | SHA256 |
| --- | --- |
| [Specification](spec.md) | `ba8c0acc93bbc666b24104750302a4a9bd36c3cd5d69bf42c5201bf4b5851201` |
| [Design](design/design.md) | `e54d324706b45ffb6ea57d6ccd8046683b6d0ed628ab3e048f01747f83879d3d` |

[Intent](intent.md) owns requester scope and authority; the consumed
[Design transition](design-transition.md) carries its reviewed closure.
Planning writes only `tasks.md`, `tasks/`, `planning-review.md` and this receipt.
After root consumes this transition it is the sole persisted ledger writer;
Leads return candidate/results rather than editing task status or dependencies.

The selected policy is the current target checkout at the stated HEAD, not the
other checkout's older execution text. Current owner fingerprints:

| Owner | SHA256 |
| --- | --- |
| [AGENTS](../../AGENTS.md) | `23e288567b372c69aa50a272c583e82c40ea868cc8e2141be51d191e7dbbb00b` |
| [Planning](../../docs/spec-first-workflow/phases/planning.md) | `0c480826932153bbd35fd1f8579e06bdd96d71e7ec0a1e83c37f254c697a7d4b` |
| [Task Review](../../docs/spec-first-workflow/phases/task-review-readiness.md) | `5efc2193fa16d7675fce841672dde79122a9de214abf812950f9101597833fb2` |
| [Ledger Contract](../../docs/spec-first-workflow/phases/planning/ledger-contract.md) | `f6be4883de68fc875ad50a01a71b75545d68e047f6f50769ee111b0a27373ec4` |
| [Implementation](../../docs/spec-first-workflow/phases/implementation.md) | `8937cd07efa2b59cfbf4d7dce8aa0904347f20fa338f6063a64a024e761c38fc` |
| [Agent Harness](../../docs/agent-harness.md) | `1aa1029cd9fecf08fd65f3a313b12522ce9d3c190287b5fbf9553b634358d7a9` |
| [Codex adapter](../../docs/agent-harness/codex.md) | `23441c0c44bcd591172588d68e2c7224b867065e9bc609d61117c5b1555d41f9` |
| [Build Speed](../../docs/build-speed.md) | `d97cdc47f54d75d01522bc2903dee6307f1ce35d3f768b33c631205ff10d1777` |

## Reconciliation and scheduling

| Accepted obligation | Unit | Initial scheduling |
| --- | --- | --- |
| R1 completed file-refresh observations and provider meanings | T1 | Ready with current inputs |
| R2 admitted usable JWKS acquisition timestamp and clock/absence semantics | T2 | Ready; disjoint from T1 |
| R3 Valkey real maintained/new-connection authentication and recovery | T3 | Starts after T1 Implemented code and guide release |
| R3 NATS real old/expired refusal and replacement recovery; target, synthetic trust, harness/CI/profile removal | T4 | Starts after T1 Implemented code and guide release |
| Composition, policy preservation and accurate proof claims | Owning T1–T4 deltas; one global Completion | Final assembled delivery only |

T1 is one completed-file-observation capability across its existing provider
owners; its internal provider edits can be disjoint execution lanes. T2 has a
separate acquisition observable. T3 and T4 have separately consumable server
proof outcomes. Each authenticated target stays with the fixture and any
harness/consumer wiring needed to make it executable; none is a test-only
execution task. No later verification/review task or persisted wave is added.

Root binds `LEDGER_ORCHESTRATOR` under current
[Implementation](../../docs/spec-first-workflow/phases/implementation.md).
Assign a fresh general-purpose Acceptance-Unit Lead to each ready packet,
using native `gpt-6-astra` / `high` / `fork_turns: none` controls under the
[Codex adapter](../../docs/agent-harness/codex.md). Record each returned native
identity in the ledger at dispatch. Native selection must succeed; prompt prose
is not model verification. Current native inspection exposes identity/status,
so retain the accepted model/effort dispatch receipt without claiming a richer
effective-model readback.

T1 and T2 may share this checkout with disjoint writers. Once T1 returns
Implemented and its descendants are joined, T3 and T4 can start independently
while T2 continues. Do not wait for a whole group, unit proof or review. Packet
locks own the exact mutable surfaces; a discovered overlap serializes only the
conflicting writers. Every Lead returns Implemented with verification pending
final validation and remains available for bounded repair. No extra worktree,
App chat or isolated handoff is needed for these disjoint scopes.

Assign the T4 Lead as the one delivery owner. After all four units are
Implemented and assembled and no writer remains, root returns the whole fixed
candidate to that owner for consolidated final validation, scoped repairs and
the required fresh independent delivery review. This final review is selected
because authentication evidence and the shared broker lifecycle affect
authorization/concurrency semantics. Root records the returned Completion
result without repeating proof or acting as a second acceptance owner.

## Resource and evidence boundary

Use the existing shared wrapper `scripts/ci/validation-lock.sh` for any heavy
Cargo/build/test command not already protected by its make target. The lock
resolves to
`/Users/daniil/Projects/Opensource/rust-service-template-rest/.git/codex/validation.lock`
and serializes all related checkouts. It is not a new per-task gate. Keep this
checkout's own `target/`; no shared target directory or cache cleanup.

The coordinator reported about 4.8 GiB free on the Data volume before Planning,
with no build outputs in this new checkout. This is capacity context, not a
fresh measurement or proof waiver. The delivery owner applies
[Build Speed](../../docs/build-speed.md), selects a bounded current-checkout
strategy and reports capacity limits before repeated attempts. No machine or
global settings changes and no deletion of other sessions' outputs. The old
PR #247 checkout was retained after a managed ownership safeguard rejected its
archive; this work does not delete it or consume its evidence as new-PR proof.

The new target registration changes a crate manifest, so the existing
[Rust validation](../../docs/validation/rust.md#broad-rust-change) route calls
for one matching build and workspace tests at Completion; do not multiply
full builds by units, harnesses or database dimensions. Concrete test cases,
assertions and commands are chosen and recorded by executors. Eligible coding
feedback follows current Implementation; final lint/scans/aggregate work stays
at the assembled boundary. Changed docs, scripts/workflow, synthetic assets
and profile removal retain their applicable existing validation/CI owners.

R3 requires actual authenticated Valkey and NATS execution somewhere in the
accepted local/CI path, including the required denial/recovery outcomes.
The existing integration gates own the heavy CI route. Missing optional local
Docker does not block code or justify new infrastructure, and a compile-only,
skipped or zero-selected result cannot discharge the server claim. The delivery
owner retains exact selected/executed CI scope and preserves PR-required gates;
draft PR cheap checks alone cannot satisfy omitted integration proof.
Commit/push/new PR authority already exists. Merge, deploy, live credentials,
live configuration/infrastructure, TLS reload, max-stale policy and broad #247
import remain excluded.

Planning performed a written readiness walkthrough and static link/fragment/
whitespace inspection only. No builds, tests, containers, live probes, commits
or push occurred. The full Docker-backed `make docs-check` belongs to downstream
validation; this phase does not claim that gate passed.

## Reopen conditions

Use Planning for an invalid unit/dependency boundary; Design for changed owner
flow, fixture isolation or restoration; Research for contradictory pinned API,
base or #247 landing evidence; Specification for signal/authentication policy;
Intake only for changed requester meaning or external authority. A mechanical
lock/path adjustment stays with execution. Preserve unaffected ready inputs.
