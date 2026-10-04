# Technical Design transition

```text
status: ready
owner: Technical Design
result: specs/postgres-pool-resilience/design/design.md; specs/postgres-pool-resilience/design/ownership.md; specs/postgres-pool-resilience/design/dependency-custody.md
review: specs/postgres-pool-resilience/design-review.md (PASS); matching three-lens Rust Ownership panel PASS
movement_evidence: current reviewed Definition consumed; native-library mechanism, exact source/custody, locked resolution, diagnostics, delivery/profile owners, operating guidance and proof scope closed without downstream invention
reopen_owner: none
next_owner: Planning
```

Worktree:
`/Users/daniil/Projects/Opensource/rust-service-template-rest.codex-postgres-pool-resilience-20261004`.
Base/workflow revision: `67be869acea112af271ec8ba621cbc50ae9d36b7`.
Continuation coordinator: `/root`; Technical Design owner: `/root/pool_design`.

## Current authoritative identity

| Artifact | SHA256 |
| --- | --- |
| intent.md | `f8a0f671878ddeeae9a01c2af9cad7859cf3d7d5b3f3a73e81404add0eecfab4` |
| spec.md | `0411915db5fbe82fcde601ca6f1621248046cfc3b491f3151983c59e606ff480` |
| design/design.md | `01093c8dc4584ad01e578b41e55f816b8fe89d8ff3c0a6929102fd007c53fac6` |
| design/ownership.md | `e5d1fa567ec7d821b6b7b1157118dfcb3c1d1f25a6cec28a5b54c9cec68ec492` |
| design/dependency-custody.md | `d35f34e3daee259b8a8d89515617f7ea1256230ee5d4766ffac9fc6caf1dc9d7` |

[Technical Design review](design-review.md) records fixed pre-status hashes,
fresh review provenance and the semantic-scope-preserving ready refresh.
[Ownership review](design/ownership-review.md) records all three compatible
PASS lenses. The earlier facade proposal/review stops are superseded; the
reviewed reopened Definition and current design alone own continuation.

## Selected result and next action

Planning can package the dependency/source plumbing, ordinary acquisition
observation at the named callers, operating guidance and bounded final proof.
It must not invent another pool mechanism or move the source/custody choice
into Implementation. The selected library remains native SQLx; a verified
published sqlx-core 0.9.0 is vendored with one upstream-aligned five-second
whole-return timeout. It is a temporary backport, not a released fix. No
application Pool/Executor/cleanup facade or extra streaming dependency remains.

The [custody record](design/dependency-custody.md) fixes the approximately
649 kB/110-file payload, root path patch, workspace exclusion, exact source-only
lock projection, real vendor source in the Cargo-chef cooked stage,
PostgreSQL profile removal and matching existing gate selectors. The projection
removes only the verified sqlx-core lock record's registry source/checksum,
then requires locked metadata/graph/build proof; no unlocked Cargo command or
registry-cache edit is allowed. Any larger resolution delta reopens Design.

The [runtime design](design/design.md) fixes one ordinary acquisition observer
returning native SQLx types, finite slow/timeout events and named caller
coverage, while retaining transaction histogram meaning and finality. Current
three-second acquisition, five-second dependency-close ceiling, capacities,
readiness, session budgets, credential handling and supported deployments
remain. A caller can time out while bounded cleanup is still running. Local
slot recovery never establishes immediate server-session termination or COMMIT
outcome. Sizing counts replica overlap, worker pools/LISTEN, migrators, other
users and reserves, with separate pooler client/backend accounting.

## Evidence and limits

The retained driver probe ran on PostgreSQL 18.6 under existing local Compose
ownership: native cancellation retained capacity and timed out after 3002 ms;
a one-second owned-return candidate recovered and served SELECT 1 after
1035 ms; healthy return reused the backend; silence after success released in
1002 ms; unpolled discard released synchronously. Its source/log and cleanup
caveat are in [research](research/sqlx-release.md). This proves mechanism
feasibility, not acceptance of the selected five-second library patch.

Current reviews are static. Implementation must establish the actual patched
source/locked graph, behavioral and observational proof, profile containment
and applicable delivery results at its final-validation boundary. Reuse
existing pool/finality/pooler/health evidence and fixtures; no new database
or profile matrix, cloud environment or full-repository claim is added.
Final make --silent docs-check passed after the review/transition receipts:
909 links, 403 unique, zero errors. The subsequent addition of this numeric
receipt changes no link or reviewed semantic decision.

No production Rust, permanent test, manifest, Cargo.lock, Docker, registry cache
or main-checkout file changed in this phase. The temporary probe target was
removed; no remote writes, spending, deployment or next-phase work occurred.
All delegated reviewers/specialists have completed; no delegated mutation or
validation remains running.

Reopen Technical Design for invalid source/graph/drop ownership, delivery/profile
containment or named observation coverage. Reopen Specification for changed
behavior/finality/budgets/support; Intake only for changed requester meaning or
authority. Retire the backport after an acceptable released SQLx fix passes the
relevant proof, removing its temporary source/delivery exceptions. Planning is
the next authorized owner; no user confirmation of library internals is needed.
