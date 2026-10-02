# Definition Transition Result V1

```text
status: ready
owner: Definition (Intake and Specification)
result: specs/infra-jobs-reliability/intent.md;
  specs/infra-jobs-reliability/spec.md
review: specs/infra-jobs-reliability/specification-review.md (PASS)
movement_evidence: requester meaning reconstructed; every material audit point
  has a fix or retained limitation with reopen condition; equivalent prior
  reviewed decisions adopted and the actual local semantic delta independently
  reviewed; no surviving material divergence
reopen_owner: none
next_owner: Technical Design, starting with System / Integration Design
```

## Fixed authority and candidate

Worktree:
`/Users/daniil/Projects/Opensource/rust-service-template-rest.codex-infra-jobs-reliability-20261002`.
Branch: `codex/infra-jobs-reliability-20261002`.
Source HEAD: `67be869acea112af271ec8ba621cbc50ae9d36b7`.
Workflow revision: that same HEAD; execution owners are `AGENTS.md`,
`docs/spec-first-workflow.md`, shared `review.md` / `transition.md`, and
`docs/agent-harness.md` with its Codex adapter. The current owner's effective
model selection remains native; it is not supplied by this document.

| Authoritative ready file | SHA256 |
| --- | --- |
| `intent.md` | `927f762c4c7cd868c0370b7dd201f118b0b55b843392920d3ed0910ab2c3fd83` |
| `spec.md` | `c648efdde020c20a4e5a1628ddbd8281b19d170cb2303de2e8e2ee190aa9027f` |
| `specification-review.md` | `84291e00cbc29141dd3708da654dde277adb2676fd5672024df1a0af607ded25` |

The local files own this branch. Parallel artifacts supply only the reviewed
provenance captured in the review receipt; do not edit that checkout or wait
for its technical work to establish this branch's authority.

## Continued accepted outcome

The user asked to fix the justified infra-jobs reliability issues completely
and open one separate PR with the fixes. Local edits/validation, commit, push,
and PR creation are authorized. Merge, deploy, and live operator commands are
not. Root `/root` retains continuation coordination. This actor stops at the
reviewed Definition; a fresh actor owns Technical Design.

Accepted fixes are full-attempt and completion-bookkeeping capacity;
failed-job custody until explicit resolution; PostgreSQL-only payload-free
bounded inspection and fenced single-row redrive/discard; discoverability
against an explicit fleet handled-kind set; immutable outbox identity and
recovery history; capped failed-depth visibility; and consistent full-process
registered-kind observation with honest freshness.

Retain PostgreSQL/SQLx 0.9/Tokio, current retries and attempt policies, static
leases, one reserved publisher slot, combined dependency admission/failure,
and default 24-hour completed retention. No new freshness/recovery/throughput
SLO, benchmark claim, heartbeat, scheduler, broad admin API, queue-framework
replacement, dependency upgrade or publisher scaling is accepted. B6 and the
audit table carry limitations and objective reopen conditions. Consumer effect
identity must cover allowed replay, which may be indefinite; no exactly-once
promise is made.

## Smallest next work

Technical Design closes the full-attempt/cancelled-batch resource mechanism,
operator command/configuration projection, bounded query/cursor behavior,
non-reusable recovery version and retained failure-history representation,
consistent process sampling owner, and any append-only migration/mixed-version
admission. It researches supported existing mechanisms where a new capability
requires one and applies the architecture/persistence/configuration owners.
It also closes Rust ownership if placement is not mechanically forced.
These decisions stay within the accepted behavior; a behavior change reopens
Specification, and a requester meaning/authority change reopens Intake.

Old retention owners must all stop/upgrade before relying on failed custody;
rollback must protect unresolved work or keep old owners stopped. Already
expired/deleted rows are unrecoverable. No runtime migration or operator action
has been performed.

Planning and Implementation remain pending. Executors choose concrete missing
regressions and commands under the repository budget. Final delivery needs the
matching local build/tests and actual selected database/migration/outbox/profile
CI results, with one independent assembled delivery review. Preserve adequate
existing proof and avoid duplicate builds across unchanged dimensions. Definition
used static source/link checks and retry arithmetic only: no code, tests,
builds, migration execution, commits, pushes, PR, or runtime proof in this phase.
