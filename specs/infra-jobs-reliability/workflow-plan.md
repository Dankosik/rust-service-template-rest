# Infra-jobs reliability continuation

Status: draft

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
T1 returned Implemented with all writers joined. Root merged current main
`546a381` without conflicts, bringing the pinned compiler to Rust 1.99.0.
Native actor `/root/infra_jobs_implementation` now owns assembled final
validation/review and serial in-scope repairs. Reconcile native status before
waiting or resuming this locator.

Next: consume final delivery proof/review, then commit/push and PR/CI delivery.
Preserve reviewed decisions; do not restart discovery.

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
No new implementation commit, push or PR was created by this branch yet.
Foundation integration includes the common implementation and known CI repairs.
Its presence alone does not establish Completion.
