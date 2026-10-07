# Technical Design Transition Result

```text
status: ready
owner: Technical Design (System / Integration Design and Rust Code / Ownership Design)
result: design/mechanism.md, design/ownership.md
review: design-review.md — initial and D1 passes retained for unchanged scope; fresh S1 failed-body hint repair PASS
movement_evidence: native/repository alternatives compared; all G1/S1 material flows, original budgets, terminal custody, cleanup and profile/file ownership closed; fresh independent Technical Design Review passed
reopen_owner: none
next_owner: current T2 implementation owner — consume the corrected failed-body hint and consumer-proof obligation
```

## Fixed candidate and identities

Source `78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`, branch
`codex/overload-resource-isolation-20261005`, worktree
`/Users/daniil/.codex/worktrees/overload-resource-research/rust-service-template-rest`.
Consumed [Definition result](definition-result.md) SHA256
`80a32dd0e9b7a4b98c4a622a21293c2e30639d4fd7214deeed5d13933341c4a0`;
its ready intent/spec/dispositions/review hashes were independently rechecked
unchanged during this phase. Those artifacts remain Definition authority.

| Ready artifact | SHA256 |
| --- | --- |
| [Mechanism](design/mechanism.md) | `8e839978bbf5097d8aaf9a7ea2997e2c5f15f9a0e825654d6781b87ce9b38fb0` |
| [Ownership map](design/ownership.md) | `da6fa1adb82455962354a40278782e5394ec0d03820b8aca7414673f20f897fd` |
| [Technical Design Review](design-review.md) | `3ef6f0e3bbd4f8d2db93e39d425245514624cf67954a8104aae2208e8836b3f1` |

Initial reviewer `/root/overload_design/technical_design_review` and fresh narrow
repair reviewer `/root/overload_design/projection_design_review` were selected
natively with `gpt-6-astra/high`, fresh history and read-only scope. Initial PASS
remains valid for unchanged G1/S1. Planning finding F1 reopened only D1
documentation/projection custody; its repair passed the fresh narrow review.
After each PASS only artifact lifecycle statuses changed. Review records both
reviewed identities, the F1 disposition and independent attempted falsifiers;
shared Transition preserves only the unchanged semantic scope of each verdict.
Implementation then exposed the S1 exact-zero failure-hint consumer gap. Fresh
reviewer `/root/overload_design/failure_hint_design_review`, natively selected
`gpt-6-astra/high`, passed its narrow correction. The current result includes
all three review boundaries without claiming unaffected scopes were rerun.

## Settled mechanism and ownership

- G1: existing `infra-grpc/src/router.rs` owns a second independently shared
  semaphore using existing K. Opening scope is below original deadline, above
  auth, and releases at response head/failure/cancellation/unwind. Existing
  terminal shed/body owner, health bypass, zero opt-out and errors stay intact.
  Bounded reject drainage cooperates on ready frames without changing limits.
- S1: `infra-object-storage/src/lib.rs` captures one original GET end before
  work and prevents an expired first send poll/late decision. `download.rs`
  owns all active body/chunk/permit/observation state, one terminal transition,
  native cooperative polling and one Weak timer with pre-captured exit guard
  and an owned JoinHandle. Unpolled expiry extracts/releases active resources;
  success and stable failure remain final. Failed Body hints have unknown upper
  bound and remain non-EOF, ensuring ordinary HTTP/1.1 framing polls the error
  instead of optimizing it to a clean empty response; successful hints stay
  exact. Drop is synchronous resource release
  plus abort, with actual timer termination proved separately.
- Existing SDK/Tokio/Tower/HTTP-body facilities were considered. SDK operation
  timeout explicitly excludes returned ByteStream and polling-only wrappers
  cannot reclaim unpolled custody. Local specialization of existing gRPC body
  ownership is justified; no generic framework, queue, new dependency/feature,
  config key, service background task or shutdown stage is selected.
- The inverse map fixes exact existing Rust/doc owners and profile containment.
  It leaves executor choice of discriminating tests intact. Public/generated
  contracts, storage allocation policy and separate #248 patches are excluded.
  D1 replaces superseded guide/source promises and keeps service class/fleet
  decisions with their current owners. Production-contract stays unmarked,
  records capability-conditional obligations, and uses only file-level links to
  four always-retained owner documents. Their existing markers gate detailed
  optional-guide links; D1 adds no manifest registration. This closes F1 without
  changing G1/S1 or expanding runtime/manifest authority.

## Evidence boundary and continuation

Performed: current source/CodeGraph inspection; architecture and selected method
reads; resolved dependency source and official AWS/HTTP-body documentation;
source and accepted input hash checks; static design/review/result whitespace
and relative file-target checks; self-review of exact ownership; fresh
independent Technical Design Review and fresh narrow F1 repair review, including
current marker/removal inventory and parser inspection, plus fresh narrow S1
failed-hint consumer review against resolved Hyper 1.11.1. No complementary ownership panel was
triggered: placement stays in present owners and no crate boundary moves.

Not performed or claimed: product/test edits, compilation, tests, benchmarks,
full docs-check/profile validation, CI or provider/runtime measurement. The
Implementation owner selects and runs final validation under the repository
budget/shared lock. Independent assembled delivery review remains required for
changed concurrency safety. No additional infrastructure or proof environment
was introduced.

This actor wrote only `design/*`, `design-review.md` and this result. Definition,
historical research, product files and root-owned workflow-plan remain untouched.
This actor performed no Planning or Implementation work. F1 remains closed.
The root can return these revised ready identities directly to the current T2
writer for the failed-body hint correction and actual HTTP/1.1 consumer proof.
Existing files, accepted behavior, writable ownership and input/dependency graph
are unchanged; no new Planning phase is required for this narrow repair. T2
and final validation own product edits and executed evidence.

Reopen Technical Design only for demonstrated custody/race/placement defects or
a necessary changed mechanism. Reopen Definition for behavior, resource scope,
new knob/dependency/feature or incompatible source/provider drift. Refresh only
affected immutable PR collision evidence if it changes before delivery; do not
import unmerged patches. Existing user authority continues through local
implementation and separate commit/push/PR, with no merge/deploy/infra or guessed
workload quotas. No blocker or unresolved user decision remains.
