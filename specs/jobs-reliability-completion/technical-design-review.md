# Technical Design Review

```text
candidate: design/system.md + design/ownership.md at HEAD ccf16c3ca29955701639ce3f522626228c699b75; source baseline ac88395be87cba3a1e0587f533dc50a71e358c8d
verdict: PASS
findings: none
evidence_boundary: fresh independent read-only Technical Design Review; artifact and bounded current-source inspection; no runtime execution
reopen_owner: none
```

Reviewer: `/root/jobs_design/technical_review`, fresh history, native
`reviewer-agent`, `gpt-6-astra` / `high`, 2026-10-06.
Method: [shared Review](../../docs/spec-first-workflow/shared/review.md) and
[Technical Design Review](../../docs/spec-first-workflow/phases/technical-design-review.md).
Authority: [specification](spec.md), [Intent](intent.md),
[Definition Transition](definition-transition.md).

The candidate was fixed before dispatch. SHA256 matched before and after review:

| File | Reviewed SHA256 |
| --- | --- |
| `design/system.md` | `ed3bf9e2ddb738bed2fe7e85931731c927039491e7b3244aacb6d41b9ab8e491` |
| `design/ownership.md` | `7073ae5be6bc4ff267abd33552397041b4e3a6d3baa97a0ed29132604c62b594` |

## Independent falsifiers and disposition

| Attempted falsifier | Evidence and disposition |
| --- | --- |
| Pending Drop interrupts persistence, or supervisor disappears without failure custody | Design arm-before-submission guard, one-time protected destruction and per-start failure latch close the gap confirmed in `attempt::drive`, `dispatch_known` and `engine::Guard`. Existing worker live and shutdown readers consume `Started::failed`. PASS |
| Caught panic payload destructor panics again | The design routes secondary unwind into supervisor failure rather than promising continued work, leaking/accumulating payloads or looping disposal. The [standard-library contract](https://doc.rust-lang.org/std/panic/fn.catch_unwind.html) permits this secondary panic; abort/double-panic exclusions remain. PASS |
| Producer restore erases effect truth, or unknown authorizes replay | Actual kill/exit and dump/restore preserve separate receiver DB; old writers stop, operator re-inspects, and unavailable readback leaves an isolated manual hold. Post-snapshot loss remains an explicit RPO gap. PASS |
| Marker substitutes for business mutation, or ambiguity repeats fanout | Separate request/marker/aggregate; one Tx for marker, mutation and fenced job completion; acceptance creates the fixed three intents once. Current Tx, complete_in_tx, outbox and webhook APIs support the flow. PASS |
| Pool minimum/slot count is wrong, or timing/pool observation proves a different scenario | 3 ordinary + 1 outbox slots, pool 8 under `N + 5`, 128 operations and 5/180/300 s envelope are explicit. `Job::pool` supplies the actual pool. Ownership transitions, not sparse gauges alone, establish structural bounds. PASS |
| SQL-only fixture or portable sync is called a working upgrade | Initialized baseline executes service-owned CLI/actual worker before adoption; preserved data/feature execute after portable sync plus explicit exact runtime source delta. Existing fixture, registration and sync ownership support it. PASS |

No surviving material finding. Review confirms a complete, feasible design,
not implemented reliability or observed crash/restore/load/upgrade results.
Concrete cases and commands remain Implementation-owned. Parent-reported
docs-check/diff-check were not treated as independently executed checks.
Reviewer made no edits and did not accept or move the phase.

After PASS, the phase owner changed only artifact status and added this review
link. This is a mechanical lifecycle refresh under
[Transition](../../docs/spec-first-workflow/shared/transition.md); the reviewed
mechanism, boundaries, ownership and proof obligations are unchanged.
