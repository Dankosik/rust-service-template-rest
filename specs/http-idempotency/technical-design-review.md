# Technical Design Review and movement evidence

Adapter: [Technical Design Review](../../docs/spec-first-workflow/phases/technical-design-review.md)
under shared [Review](../../docs/spec-first-workflow/shared/review.md).

- Baseline: `main` at `d24d1737d1647d9bfdf2b15ecb258630ec9cd326`. The current
  `main`, `ac6a6ffe248cf6dac5232438876c564030316ed1`, only makes
  `Dsn::admit_with_environment` crate-private.
- Candidate: the untracked `specs/http-idempotency/` bundle.

Final result: **PASS**, reached after one bounded delta recheck by the same
reviewer. Movement is permitted.

The reviewer, `a8421283bfd35edaa`, was a native `reviewer-agent` carrier
selected with the Opus model field, with fresh history and a read-only
boundary. It received:

- the fixed hashes;
- the adapter;
- the accepted Definition inputs, and the transition's ten decision groups
  as the checklist;
- the [ownership panel receipt](design/ownership-review.md) to consume
  without repeating it;
- the delivery context: real-PostgreSQL proof and the 16-graph initializer
  matrix run locally in Implementation;
- the rule that no network request may carry the user's email address or any
  other personal data.

## Review of c7

```text
candidate: design/system.md b198ac00aa5c8b81507a0d42b3591e7afcfbf101b6cc51b52b8a6611b9c1e620;
  design/ownership.md b2ae411d1f344c83a6757f0c678ec2403469512ff0c700a359296a808c1d76ab;
  consumed design/ownership-review.md 301716607c4a4f3797ca5d8a4171b3c6d4e99f9eff4719a64509e9c8741da1d1
verdict: FAIL
findings: F1 (blocking); C1 (concern, bounded risk); three non-blocking observations
evidence_boundary: static, read-only: the candidate, the receipt, the five authority
  inputs, the workflow owners and the architecture leaves they select, current code in
  crates/, test/, scripts/, make/, and .github/workflows/ci.yml, version-exact registry
  sources (sqlx 0.9.0, axum and tower, utoipa, metrics, httparse, sha2, humantime 2.4.0),
  and the recreated visprobe/ sources; no build, test, container, make target, network
  request, or subagent
reopen_owner: System Design (a bounded delta on sections 6.2, 10, and 13 and the guide row)
```

**F1: some admitted retention values break the record write.**

- `sqlx-postgres` 0.9.0 refuses to encode a `std::time::Duration` that has a
  sub-microsecond part (`types/interval.rs:102-110`).
- humantime 2.4.0 accepts nanosecond units (`duration.rs:323`).
- So a retention such as `1h 1ns` passes configuration, activation, and the
  startup check. After that, every execution fails at the bind with
  `WriteFailed` and answers 503.
- Section 6.2 had claimed that the duration encodes with microsecond
  precision.

**C1: release closure covered only a new route.** Converting an existing
operation, rolling that conversion back, or removing it leaves a window with
mixed versions. During it, an older replica ignores the key, so a same-key
retry that reaches one executes again.

**Non-blocking observations:**

- A fourth initializer cache presses the repository's 10 GB cache budget.
- In derived services, the classifier would not select P9 for `infra-http`
  or `infra-bearerauthn` changes.
- `make template-init-check` becomes Docker-backed while keeping
  `requires_heavy=false`.

Falsifiers that survived:

- **Two replicas.**
  - Concurrent replays never get 409.
  - A commit is visible before its lock is released.
  - The upsert never overwrites a live record.
  - Cleanup never disturbs an execution.
  - The writer check gates replays.
  - The work cannot change the isolation level.
  - The pinned vectors were recomputed and match.
  - The RFC 8785 alternatives stay rejected.
- **Cancellation.** A drop during the work, a drop during `COMMIT`, a panic,
  and a client disconnect all behave as designed.
- **Unknown commit and 504 precedence.**
  - The lost-acknowledgement proxy is feasible.
  - A commit not yet visible at readback answers 503 with no second run.
  - The readback stays inside its bound.
  - The pending future leaves 504 to the timer.
- **Outcome counting.** Exactly one outcome is recorded.
- **Startup.**
  - The verifier is bound.
  - Contract assembly moves safely.
  - The inert store is unreachable when active.
  - `Active` comes only from `agree`.
  - Cleanup cannot outlive a failed startup.
  - Agreement runs even when inactive.
- **Cleanup and shutdown.** The work fits the join and close budgets.
- **Schema and SQL.** The `query!` deferral premise and the startup-check
  mapping hold.
- **Profile machinery.**
  - The lock shapes are sound.
  - `none` `Cargo.lock` equality holds.
  - Service names fit the 64-character limit.
  - The graph database step works.
  - The CI split and the one-shot equality are feasible.
- **Necessity and enforcement.**
  - Every component and edge is needed.
  - Atomicity names its enforcing mechanism.
  - The adapter's bypasses (transaction-control SQL, the profile table) are
    accounted for by contract, as for `in_tx` today.
  - Features cannot reach the connection.

The reviewer found the consumed receipt compatible with c7.

## Repair: c8

The owner verified F1 at the primary sources before repairing it, then
repaired only this review's anchored items:

- **F1.**
  - `Store::new` drops any sub-microsecond part of the retention once. This
    happens after configuration has checked the value as written against its
    range.
  - Section 6.2, the section 4 comment, and the section 14 source-read line
    are corrected.
  - Three new cases cover it: a configuration case (which loads), an inline
    store test, and a database-suite write under such a retention. Without
    the truncation, that write fails.
- **C1.**
  - A section 13 row covers converting an existing operation, and rolling it
    back or removing it. The guarantee starts only after every replica serves
    the operation through the boundary, and a rollback or removal withdraws
    it at once.
  - The mixed-versions paragraph and the guide row match.
- **Observations.**
  - The `http-idempotency` CI part restores the `database-postgres` cache and
    saves none.
  - Where `mounted.rs` exists, `infra-http` and `infra-bearerauthn` changes
    select the database suite.
  - `requires_heavy` stays false, because `ALLOW_FULL` already gates the
    matrix.
- **One coherence correction that the classifier observation required.** The
  ownership map's region rule now covers service-owned files only.
  Template-owned files carry no markers and, like today's authentication and
  outbound rows, may name pack paths.

The repair left the outcome, the boundary, the accepted inputs, the public
interfaces, and the risk surface unchanged. That made it eligible for a
bounded delta recheck by the same reviewer.

## Bounded delta recheck of c8

```text
candidate: design/system.md 468c103bd7b129cf1fda214b7827927ceb9a35c812a197e2a57fa1c853dde80a;
  design/ownership.md 8a50f29c94e76d59944c3054707e019715a69dac482ae55c5b647df97022972c;
  delta c7 to c8 7c4b3136c94ae86ea07f06dab4581b7a448b47eb7a6a8d1d6546a247235b3bed (12 hunks)
verdict: PASS
findings: none (F1 and C1 closed); one non-blocking note
evidence_boundary: static, read-only: the delta, reverse-applied in memory to the c7
  hashes; the c8 files; the c6 and c7 snapshots with the c6-to-c7 delta; the cited
  repository files; no build, test, container, make target, subagent, or network request
reopen_owner: none
```

- **F1 closes.**
  - The range bounds are whole microseconds, so the truncation cannot move a
    value out of the range.
  - It changes no observable expiry.
  - `$7` is the only place the SQL uses the retention.
  - The database-suite case discriminates.
- **C1 closes.** The new row fills every release-closure column and matches
  the specification's rule that operations without the boundary ignore the
  key.
- **The observations are applied correctly.**
  - The cache key and the save step are the ones `ci.yml` uses today.
  - The classifier follows its existing file-existence check.
  - Every template-owned file the reviewer checked carries no markers, so
    the scoped region rule matches the repository and removes a real
    contradiction.
  - `ci_owned` returns on `requires_heavy` before it reaches the
    `ALLOW_FULL` check, so keeping the flag false preserves that gate.
- **The receipt's c6-to-c7 delta is now verified.** It is byte-identical to a
  fresh diff of the snapshots, and it contains only the receipt's listed
  items.
- **Non-blocking note.** The conversion row's readback is the operation count
  in `http_idempotency_active`. It cannot tell releases apart if the same
  release also removes another idempotent operation; `app.version` in
  `service_starting`, or the platform's rollout status, can. The reviewed
  design was not edited after the PASS. The note is carried to the guide
  author in the [transition](technical-design-transition.md).

## Lifecycle edit, receipt refresh, and final identities

After the PASS, the owner made two further edits:

- changed only the `Status` line of both design files, from `draft` to
  `ready`;
- refreshed the panel receipt, adding the c7-to-c8 repair section and the
  later candidate rows.

Under Transition's unchanged semantic-scope rule, the PASS applies to these
identities (SHA256):

- `design/system.md`: `47cf5fe908bc790a0dae2425a05e3e1fd3cae9ef0c1aa81bce547b8b9964510f`
- `design/ownership.md`: `80818a74412e3ff55be24086d41df3d22d6e3a4f353d06c7ce8f17e7ade7414e`
- `design/ownership-review.md`: `09843798c46b7b9feb67b1345d0d86f9dad8c6acdf54c11466fc48f81a2408a3`

## Static checks and evidence freshness

`make docs-check` passed after every candidate revision, and again over the
final bundle (706 links, 0 errors). It is a static link check, not
implementation evidence.

A coordinator restart cleared the design scratchpad partway through the
phase, so the earlier probe scripts are gone. System design section 14
records what each probe observed at the time. It also records the two results
re-established afterwards: the opaque-handle probe, and a fresh computation of
the section 8 vectors.

This verdict proves design coherence and feasible proving surfaces only.
Implementation still owns runtime code, the real-PostgreSQL suite, the
initializer matrix, CI, and the final delivery review. No remote delivery
evidence is claimed.
