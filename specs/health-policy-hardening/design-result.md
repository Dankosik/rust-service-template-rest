# Technical Design result

```text
status: ready
owner: Technical Design
result: design.md
review: Technical Design Review below, PASS
movement_evidence: R1-R4 mechanisms, failure/cancellation boundaries, clocks and metric absence, file ownership and feasible proof surfaces are closed; independent review found no material gap.
reopen_owner: Technical Design for an invalidated mechanism/placement; Specification for changed behavior or an incompatible admission bound; Intake for changed requester scope or external authority
next_owner: Planning
```

## Fixed candidate and independent review

Checkout: `rust-service-template-rest.codex-health-policy-hardening-20261005`.
Branch: `codex/health-policy-hardening-20261005`.
Base/HEAD: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.
Accepted spec SHA-256:
`0e36e32ef020b8f7aee5aa4012aced2ef8aec761834fbb862945a470d50b8c56`.

Reviewer: `/root/health_design/design_review`, fresh `reviewer-agent`;
native dispatch `gpt-6-astra`, `high`, `fork_turns: none`. The callable schema
accepted those settings and returned this running identity; no fallback was
used. The reviewer returned Review Result V1:

```text
candidate: design.md SHA-256 f0fadf1a6c71756c33a506ea582fcb149af88904218583658dad79969d8d5e12
verdict: PASS
findings: none
evidence_boundary: Fixed design and ready specification hashes checked before and after review; current health fold/publication/scheduling, pooled verification/cleanup, resolved API versions, architecture/validation owners and existing silent-relay proof feasibility. Source/API and static reasoning only, no build/test/runtime proof.
reopen_owner: none
```

Attempted falsifiers were rejected by the selected mechanisms:

- Ready expires while a round is running, or after an absorbed failure: the
  existing fold evaluates the previous completion age before retaining Ready.
- Refresh stops with Ready gauge at 1: completion timestamp and stale bound
  remain sufficient for clock-qualified observation; absence, drain,
  cancellation, failed completion and scrape limitations are explicit.
- Pool acquire/readback or rejection cleanup waits indefinitely: one outer
  verification timeout and the existing bounded close retain the original
  refusal; only verification success returns the native pool.
- Detection estimates omit initial phase or treat parallel probes as serial:
  serial rounds, Delay scheduling and the common round deadline support the
  accepted 6/11/14-second estimates and separate 16-second stale guard.

After PASS, only the design's status changed from `draft` to `ready`.
This preserves the review for unchanged semantic scope under Transition.
Ready design SHA-256:
`e1484f9c1a70971fd0f8b5abbf63c9f167c4c64ec41b220ae8af0c15a4093be0`.
This receipt records movement; it does not create a new technical decision.

## Planning continuation

One fixed Implementation unit can own the coherent health-policy repair.
Its production owners are `crates/health/src/lib.rs` and
`crates/infra-postgres/src/pool.rs`; proof remains at their colocated tests and
`test/tests/postgres.rs`, with only a necessary refinement of the existing
`test/tests/support/commit_proxy.rs`. The design's inverse file map identifies
the four existing documentation owners. No new crate, dependency, module,
config key, bootstrap/transport policy, vendor patch or external effect is
needed. Planning selects the execution carrier and preserves this map.

Executor-owned test selection and final validation remain downstream. No
build, tests, general-documentation edits, production/test edits, commit, push,
PR or external operation occurred in Technical Design. Only `design.md` and
this receipt were written. The eight design relative links resolve once this
receipt exists; static link checking is not the repository's `make docs-check`.
No mandatory runtime observation is claimed from the source review.

The original dirty checkout stays outside writable scope. Existing authority
for scoped implementation, validation, commit, push and one PR continues;
merge, deployment and infrastructure changes remain excluded. No user-owned
decision or required external input remains open for Planning.
