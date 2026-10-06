# Operation budgets implementation review

Reviewer: `/root/budget_implementation/final_review`, fresh read-only native
reviewer, `gpt-6-astra` with `xhigh` effort. Method: integrated
[Implementation Review](../../docs/spec-first-workflow/phases/implementation-review.md#integrated-candidate).

## Initial result

```text
candidate: aba175690c0dd726f6e3b3f0be7ae789eb67e898 against 78aa3a832bfb4d7e9632ce5ebbbf1680705c31af
verdict: FAIL
findings: R1 and R2, TASK_DEFECT, C13 profile construction
evidence_boundary: independent fixed-source review of C1–C13, changed tests, docs, graph and profile parser; no execution; required final validation remains Lead-owned
reopen_owner: T001 Lead, no upstream behavior/design reopening
```

- R1: the closing introspection marker in
  [bearer verification](../../crates/infra-bearerauthn/src/lib.rs) followed a
  closing brace on the same line. The initializer accepts full marker lines,
  so it saw an unterminated block. The reviewer traced the literal through
  `_MARKER_RE.fullmatch` and `_apply_markers`.
- R2: the new [operation-budget guide](../../docs/operation-budgets.md) nested
  the optional OAuth/gRPC marker inside the outbound-auth marker. The existing
  parser rejects a second begin marker while its stack is nonempty.

The reviewer independently traced opening/response separation, auth waiter
isolation, prepared-call cutoffs, cache recovery lineage, autonomous S3 resource
release, confirmed mutation retention, pending unknown outcomes, and message/job
custody. It reported no additional surviving runtime defect. This is source
review evidence, not behavior or CI proof.

## Repair scope

The original reviewer is retained for a bounded recheck of R1/R2 and the root-
routed snapshot runner repair discovered by the local profile gate. Required
local results will be supplied before final PASS. No replacement reviewer or
second whole-candidate review is requested.

## Final bounded result

```text
candidate: 928963fd32f8ff9676987a49df0af3903a0261a2
source_sha256: df35e6753a154d932dcb6ff832f32c5bbe0c9dc87a07ff5b576220fcb823959b
verdict: PASS
findings: none; R1 and R2 closed
evidence_boundary: prior integrated review retained; marker/batch-reader and gRPC test-placement deltas independently checked; actual local build, scoped test repair, negative-control, projection and lint results consumed; requested current-head CI remains pending
reopen_owner: none for local review; T001 Lead retains CI delivery
```

The reviewer checked the test relocation preserves both the public cutoff and
parent-cancellation assertions while leaving production metrics and the
257-series expectation unchanged. It consumed the successful workspace scopes
and focused gRPC/jobs-worker runs without calling the failed aggregate a pass.
It also verified that the C7 baseline case failed only after successful
compilation at the late-dispatch assertion and that the restored code passed.
All four profile representatives and their terminal passed receipt were
consumed. No duplicate validation or source edits were performed by the reviewer.

Subsequent receipt-only commits preserve this source identity and reviewed
semantic scope. CI execution remains a separate requested obligation, not an
inferred result of this PASS.


## Main integration review: 2026-10-06

Reviewer: `/root/budget_implementation/integration_review`, fresh read-only
native reviewer, `gpt-6-astra` with `xhigh` effort. The main runtime-progress
changes invalidate only their interactions with the operation-budget candidate;
unaffected original review reasoning and proof remain retained.

```text
candidate: main 699887b18594088a59bcc23a049d290d089f6da1 integrated into rewritten task 41af344b34bb60b02e40c03bf37323bf06cbf205
source_sha256: 394926e9b17681efedec4c2a2dc69a4d97836a83db7ae530a5b0be79734033bd
source_serialization: sorted path, NUL, format(stat.st_mode, "o"), NUL, exact bytes, NUL; 64 non-spec outputs
verdict: PASS
findings: none
evidence_boundary: fresh read-only integrated-candidate review; actual local build/lint, original passing test scopes, two scoped repair reruns, final profile/docs/secret checks consumed; selected current-head CI remains delivery-owned
reopen_owner: none for local review; T001 Lead retains CI delivery
```

Falsifiers covered cache cancellation and retirement before permit reuse; S3
response limits, confirmed versus unknown mutation outcomes, autonomous resource
release and unread-tail collection; HTTP/gRPC opening versus response custody;
messaging expiry versus settlement; webhook job-context propagation; canonical
documentation and profile pruning. All 577 incoming-main registry identities
and checksums were independently confirmed unchanged.

The reviewer inspected the deterministic HTTP tracing-callsite control and the
assertion-preserving panic-fixture consolidation, then consumed 88/88 HTTP
library tests and the unchanged 17/17 lifecycle rerun. The original aggregate
remains recorded as failed. It independently reproduced the final source digest
and consumed all four profile projections, documentation and zero-finding
secret scans. No reviewer source edits or duplicate validation were performed.
Current-head CI, provider/runtime certification, merge and deployment are not
inferred from this PASS.
