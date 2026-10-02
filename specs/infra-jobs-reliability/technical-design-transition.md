# Technical Design Transition Result V1

```text
status: ready
owner: Technical Design — narrow immutable-foundation adoption reopen
result: specs/infra-jobs-reliability/design/system.md;
  specs/infra-jobs-reliability/design/ownership.md;
  specs/infra-jobs-reliability/design/rollout.md
review: specs/infra-jobs-reliability/technical-design-review.md
  (original PASS reused for unchanged scope; fresh adoption/delta PASS)
movement_evidence: equivalent available implementation adopted; D1–D6 close
  the remaining accepted contract and bypass gaps with exact existing owners;
  no surviving finding or new behavior/authority/dependency decision
reopen_owner: none
next_owner: Implementation — root reconciles the existing T1 packet for
  immutable foundation plus D1–D6, then resumes its implementation owner
```

## Immutable foundation and ready result

Worktree:
`/Users/daniil/Projects/Opensource/rust-service-template-rest.codex-infra-jobs-reliability-20261002`.
Branch: `codex/infra-jobs-reliability-20261002`.
Our source HEAD remains `67be869acea112af271ec8ba621cbc50ae9d36b7`; no source
integration was performed by this phase. Current workflow owners remain those
of that checkout. [Definition](definition-transition.md) and its B1–B6 meaning
are unchanged. Root `/root` retains the complete fix-and-one-PR outcome.

Adopt available PR #225 source at immutable
`a04b699f744038bf9b529bdee89eda76c00f59a6` (implementation
`ebb88eadce1b3cb30cde1686b8927e7a78d7bc95`). Its descendant
`5a683be7098fdba4981afffd774f59fed40145ed` is admitted with only the verified
SQLSTATE let-else rewrite and historical webhook-row projection adjustment.
Use those Git objects for serial integration; do not copy the active parallel
checkout or repeat its covered core implementation. Later heads require exact
delta reconciliation; a changed head alone neither invalidates the unchanged
mechanism nor transfers proof.

| Ready artifact | SHA256 |
| --- | --- |
| `design/system.md` | `2babd0927d7776d9be6bd3effcfff3984f1071d5d8d042f3228352f87f7c6d76` |
| `design/ownership.md` | `6ed5354c972bf1babc5414e97ce34953bcfe64df69153f734306eb444d94e02b` |
| `design/rollout.md` | `52117e720bef3264b0a9f36f70bdac45ab44d432b44c9da395ff3efee532d392` |
| `research/mechanism-evidence.md` | `b439dc746b44023f11cace3318ce8da4ed244bceea36dada04bfe5ff9fce2d4a` |
| `technical-design-review.md` | `aae2ee486b97b6dac4e34b514c71744026cacb6d5268316675cc047df4c11722` |
| `intent.md` | `927f762c4c7cd868c0370b7dd201f118b0b55b843392920d3ed0910ab2c3fd83` |
| `spec.md` | `c648efdde020c20a4e5a1628ddbd8281b19d170cb2303de2e8e2ee190aa9027f` |

Only system/ownership status lines changed after the fixed delta PASS. Review
receipts preserve the reviewed hashes and the original unaffected panel/result.
All review actors are finished; this phase releases its writable scope.

## Remaining implementation delta

D1–D6 in System Design are the complete intended correction over the foundation:

1. Retire all completion batch entries/custody and SQL buffers before any reply
   wake, including cancellation-induced sender closure.
2. Initialize new peer metric series and invalidate freshness on late admission;
   compare captured union and publish under the same peer mutex.
3. Make UTF8/writable/READ COMMITTED startup admission unconditional for every
   operator command, while read transactions remain read-only.
4. Apply `SET LOCAL statement_timeout = '2000ms'` before either recovery action's
   initial target lock. Retain the actual existing 12s operation backstop;
   the former five-second provenance was incorrect and belongs to startup.
5. Emit `schema_version: 1` and the validated normalized `handled_kinds` context
   for unhandled results, including post-admission failure results.
6. Complete the existing docs for indefinite replay versus finite dedup TTL,
   saved-command invalidation after restore, and B6 retry/poison compatibility.

Adopt equivalent recovery_history archives, caller-owned provisional Tx API,
RFC3339 microseconds, 500-row/1024-kind bounds and existing comma-list CLI.
No recovery_generation column, re-tagged failure writers, replacement pool API,
new framework, observer, SLO or whole-core rebuild is required. The inverse map
includes actual cli.rs/operator.rs and existing profile/generated authorities.
Ordinary shape/lint repairs and concrete in-scope defects found during final
validation remain Implementation work; they do not require another design
cycle absent changed mechanism or behavior.

## Authority, proof and stop

Implementation/validation/commit/push/one PR remain authorized. Merge,
deployment and live queue recovery remain outside the request. The pending
permission to message the parallel App chat has not become authorization.
Source adoption is serial and root-owned; no actor may mutate that parallel
checkout. Existing T1 outcome/acceptance boundary survives this smaller work
frontier; refresh its implementation inputs from this ready result.

This reopen used immutable source inspection and a fresh bounded review. It
performed no source edit, Rust build/test, database/broker action, migration,
commit, push or PR. Earlier static doc checks and baseline/foundation tests do
not establish assembled candidate proof. Final validation must evaluate reusable
evidence against the integrated candidate and run required current checks,
with genuine `make sqlx-prepare` metadata when fixed SQL changes and serialized
CPU-heavy work. Keep existing DB/migration/outbox/profile owners and the final
assembled independent review; no duplicate matrix or new environment is added.

Reopen System Design only for changed mechanism/rollout, Rust Ownership for a
changed responsibility/dependency/generated boundary, Specification for changed
observable behavior, or Intake for requester meaning/authority. The next action
is targeted implementation on the admitted foundation.
