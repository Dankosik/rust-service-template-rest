# Definition transition after maintenance-simplicity steering

```text
status: ready
owner: Definition (bounded Intake / Specification reopen)
result: specs/postgres-pool-resilience/intent.md; specs/postgres-pool-resilience/spec.md
review: specs/postgres-pool-resilience/definition-review.md, Current candidate (fresh PASS)
movement_evidence: changed intent/spec passed fresh full Specification Review; all four outcomes and steering have dispositions; no unresolved user-owned input
reopen_owner: none
next_owner: Technical Design
```

Worktree:
`/Users/daniil/Projects/Opensource/rust-service-template-rest.codex-postgres-pool-resilience-20261004`.
Base and workflow revision: `67be869acea112af271ec8ba621cbc50ae9d36b7`.
Continuation coordinator: `/root`. Definition owner: `/root/pool_definition`.

Current artifact SHA256:

- `intent.md`: `f8a0f671878ddeeae9a01c2af9cad7859cf3d7d5b3f3a73e81404add0eecfab4`.
- `spec.md`: `0411915db5fbe82fcde601ca6f1621248046cfc3b491f3151983c59e606ff480`.

The reviewer fixed the pre-transition spec hash recorded in the current review;
only its lifecycle sentence changed after PASS. The behavioral scope is identical.

The active intent/spec supersede the assistant-selected retain-SQLx exclusion,
three-second cleanup assumption and universal pool-interception requirement.
All four original outcomes remain. Maintenance simplicity and ready-made library
ownership now explicitly govern mechanism choice. The ordinary acquire budget
remains three seconds; whole-return cleanup gets five seconds. An immediate
replacement acquisition can time out during cleanup; eventual recovery after
cleanup remains required. Local release does not prove physical server-session
termination, rollback or a known COMMIT outcome.

Next action: resume `/root/pool_design` to replace the unreviewed
facade draft using the [reopen evidence](design-transition.md),
[alternatives comparison](research/alternatives-reopen.md), and
[dependency custody proposal](design/dependency-custody.md). The selected
technical direction is a narrow temporary backport into published sqlx-core
0.9.0, with native pool ownership and ordinary acquisition observation. It is
not a released upstream fix. Technical Design must close source, delivery,
profile and retirement custody and obtain its own fresh review before Planning.

No production workload or target was supplied; deliver a template improvement,
local evidence and operational sizing guidance. Preserve pool capacities,
readiness, PostgreSQL 14+, supported PgBouncer, optional profiles and existing
failure/finality policy. No remote writes, spending or deployment are authorized.
This Definition phase changes no production/test source and runs no new database
scenario. Reopen Specification for changed behavior/compatibility and Intake
only for changed requester meaning or authority.

The prior Definition review and its bounded clarification remain historical in
[definition-review.md](definition-review.md). Prior docs-check results (850 links,
zero errors after receipt creation) apply only to that earlier document set.
The reopened document set passed `make --silent docs-check`: 896 links,
402 unique, zero errors. The subsequent lifecycle and review/transition updates
add no links. This phase's proof remains static; the retained driver experiment
is scoped by its research owner and does not establish the selected backport.
