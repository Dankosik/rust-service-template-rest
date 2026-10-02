# Infra-jobs reliability continuation

Status: done

The user authorized complete justified fixes and one separate pull request,
then explicitly requested continuation after interruption. Commit, push and PR
creation are authorized; merge, deployment and live queue recovery are outside
the request.

Root `/root` owns continuation. The isolated checkout is
`/Users/daniil/Projects/Opensource/rust-service-template-rest.codex-infra-jobs-reliability-20261002`,
branch `codex/infra-jobs-reliability-20261002`, source baseline
`67be869acea112af271ec8ba621cbc50ae9d36b7`.

## Current boundary

[Definition](definition-transition.md) and
[Technical Design](technical-design-transition.md) are ready with independent
PASS. [Planning](planning-transition.md) is also ready with PASS. Root is the
sole ledger writer. Narrow Technical Design adoption is ready with delta PASS.
Root fast-forwarded the isolated branch to immutable foundation
`5a683be7098fdba4981afffd774f59fed40145ed` and reconciled T1 inputs.
T1 is locally Accepted at `86b388431685ec950d5a0df7ec93aa5f3abf5bbe` with
claim-matched validation and independent PASS. All actors/readers/writers joined.
Root owns only outstanding PR #228 CI and closeout. Current main `546a381`
was integrated without conflict; pinned compiler is Rust 1.99.0.

CI `37034066726` and CodeQL `37034066787` completed successfully on exact head
`86b3884`. Global Completion is Accepted. Next: Git archive and remove completed
execution-only state under Cleanup, validate the changed documentation links,
then obtain the final published-head gate results without repeating local Rust
checks. Product-source bytes and their accepted review remain unchanged.

## Coordination and proof

A separate user-owned jobs-worker chat has overlapping work. Its artifacts
were inspected read-only and compatible Definition evidence was adopted.
Permission to message that chat was requested but has not been received;
do not send messages or edit its checkout. This branch remains independently
owned. PR #225 was inspected at immutable head
`a04b699f744038bf9b529bdee89eda76c00f59a6`, implementation commit
`ebb88eadce1b3cb30cde1686b8927e7a78d7bc95`. Its checkout has active CI repairs
and is read-only here; consume only immutable published code. The Lead returned
coverage and gaps without accepting or testing it. Remaining corrections:
batch disposal before replies, late-peer sampling freshness, operator budgets
and admission, output version/handled-set echo, and replay/restore guidance.
Review and validation still cover the assembled candidate; another task's
status does not prove this Completion.

Docker availability was refreshed to a working existing OrbStack context
(server 29.4.0), without host mutation by this task. Actual SQL metadata must
come from the existing `make sqlx-prepare` route. Final checks remain selected
by repository validation and CI owners, with CPU-heavy work serialized.
This branch delivered PR #228 with common implementation and all corrections;
it can replace the overlapping #225 candidate, without merging both. Local
checks and independent review passed after disk capacity recovered. Current CI
head is `86b3884`; old cancelled draft runs are not its active gate receipts.
