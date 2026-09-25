# Technical Design result

```text
status: ready
owner: Technical Design
result: design/system.md, design/ownership.md, design/rollout.md
review: Independent Technical Design Review below: PASS
movement_evidence: All material mechanisms and placement decisions are closed; both anchored review findings are repaired and independently rechecked.
reopen_owner: none
next_owner: Planning — docs/spec-first-workflow/phases/planning.md, then required Task Review / Readiness
```

Consume [Definition](definition-result.md), the ready [specification](spec.md),
[system design](design/system.md), [Ownership Map V1](design/ownership.md) and
[migration sequence](design/rollout.md). The root continuation coordinator owns
the next fresh phase actor. No requester-owned decision remains.

The design preserves the existing crate graph, transaction classifier, process
grace and CI-owned heavy gates. It deletes heartbeat/upkeep and outcome
attribution machinery; one supervisor retains each result through bounded
shutdown. Transactional completion has a typed stale rollback path and a
no-transition disposition after unknown commit. PostgreSQL owns randomness;
installed OpenTelemetry owns propagation. Observation is capped and exposes
failure/freshness. A stopped producer/worker migration atomically refuses
incompatible legacy bytes.

Deliberate dispositions: no new lifecycle crate now (actual duplicate signals,
deadline and cancel/join primitives were inspected; extraction cost and reopen
condition are explicit); no unregistered queue aggregate; no constant physical
I/O claim; selected-ID locking can underfill under contention. Future 10.5/10.6
reuse is documented without implementing their products.

## Independent Review Result V1

Reviewer: native `/root/jobs_design/technical_design_review`, requested
`gpt-6-astra`, `xhigh`, `fork_turns: none`; it completed initial review and one
bounded delta recheck. Review and evidence owners were loaded. Lifecycle
source evidence came from read-only `/root/jobs_design/lifecycle_evidence`,
requested `gpt-6-astra`, `high`; it supplied advisory evidence, not a verdict.
Both descendants completed and have no active assignments.

Candidate base: `4edd184ea3cc6b6fa2b225244700fce57150ae18`.
Verdict: **PASS**. Findings: none. Reopen owner: none.

The initial review found two mechanism defects. PostgreSQL can retain locks on
rows rejected after a concurrent update, so placing LIMIT only in the locking
SELECT did not bound retained locks. The repair materializes at most batch IDs
before any lock. SDK propagation also silently defaults malformed tracestate;
the repair validates it with the installed public TraceState parser first.
The reviewer re-applied both falsifiers and closed them. Unaffected review
reasoning was retained under shared Review's bounded-delta rule.

Other attempted falsifiers found no surviving gap: unknown claim dispatch,
double refunds, stale completion after business writes, lost commit
acknowledgement, ready-result versus force cancellation, pre-SQL NUL handling,
atomic migration refusal, stale/censored observations, profile removal, test
oracle retention and lifecycle ownership.

Reviewed SHA-256 identities before status-only finalization:

- `design/system.md`: `8901fd6300f15e7256ef9a16c596781069a93cc2723bd41534899572d6335fe6`
- `design/ownership.md`: `baa95e7f6b63d519e7116175cd9393aef98c2d89dfbf7c393981ea7fd395ae3f`
- `design/rollout.md`: `59f97790b485e08ea168f979c778ef727d85f4cce348b4399b4a8a7e317e71d5`

After PASS only those files' status lines changed to ready. Current identities:

- `design/system.md`: `7653b7a697d5397d4c445e99e2035d31981983476d543d1e97e89fd64416d45d`
- `design/ownership.md`: `58e686d1831a53b76e2be6fb59f90f434347cc266980dee9c92c52ac96abc04d`
- `design/rollout.md`: `7da8271961826aa3eb93a634505169babc7163f99696e1d8221c801a2d3d2aa0`

## Evidence boundary and continuation

Read-only source/API/SQL research and independent design review support the
mechanisms. Static relative-link and whitespace checks passed for the three
design files. No implementation files changed, no builds/tests/database
execution occurred, and no runtime or measured speedup is claimed. Applicable
implementation verification and exact-candidate CI remain future obligations;
existing CI-owned database, migration, profile and image gates stay there.

Planning may create the smallest dependency-ordered ledger from these closed
decisions; it need not invent a test-design phase or exhaustive proof matrix.
Reopen System Design only if implementation evidence defeats a mechanism;
reopen Definition only for a changed accepted behavior. One PR is authorized;
merge, deployment and live data repair remain outside this request.

Methods applied: System / Integration Design; Rust Code / Ownership Design;
rust-reliability, rust-tokio, rust-sqlx, rust-errors, rust-observability,
rust-performance, rust-coder and rust-structural-quality; test-audit only for
the test-consolidation boundary, with repository validation policy authoritative.
