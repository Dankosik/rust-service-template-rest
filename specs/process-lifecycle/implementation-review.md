# Process lifecycle Implementation Review

## Initial assembled candidate

Reviewer: `/root/lifecycle_delivery/implementation_review`, a fresh native
`reviewer-agent` dispatched with `gpt-6-astra`, `xhigh`, no inherited turns.
The delivery actor independently received the native completed result.

```text
candidate: branch codex/process-lifecycle-20261005; HEAD/base 5927ffbba351af2f7fb8635316bbfa4ae5b31da6; tracked git diff --binary HEAD SHA256 bdbb6432b6579346e0792be8cbfb03e0d1d024b1cc77b55defb564862901fbdd; plus accepted task artifacts
verdict: FAIL
findings: R1 TASK_DEFECT, worker second-stop consumption
evidence_boundary: read-only assembled L1-L8 source and adjacent test review; no extra runtime execution
reopen_owner: original Implementation Lead through the continuation root
```

R1: `crates/jobs-worker/src/bootstrap.rs:839` receives the first
`signals.wait()` result and calls `pending_end`; its consuming
`signals.pending()` at line 263 can receive a second notification, for example
SIGTERM followed by SIGINT. Returning the same `Ended::Signal` loses the
distinction. The subsequent drain wait then requires a third notification and
can spend its full allocation with unfinished work. This mechanically
checkable trace falsifies L7's retained repeated-signal expedite behavior.
No runtime reproduction was executed. The smallest repair boundary is worker
stop selection in the original Implementation Lead's scope.

The reviewer attempted falsifiers for cancellation during admission; retained
pool, broker, provider and listener ownership; partial bind failure; required
task/listener failure before readiness; panic after cancellation including the
native engine guards; forced acknowledgement before dependency close;
unwind/error precedence; the diagnostics connection-only exception; whole-tail
arithmetic; operator signal callers; and source profile removal markers. No
other blocker survived those source checks. Adjacent regression tests were
inspected but not executed. The delivery actor's architecture/dependency
results were available; build, test and docs execution was still pending.
Generated-profile, database, messaging, cache and provider runtime behavior
was not claimed observed.

The reviewer completed without editing the candidate. The same reviewer is
retained for the bounded R1 repair and invalidated evidence under the shared
[Review](../../docs/spec-first-workflow/shared/review.md) contract; this does
not reopen unaffected review reasoning or create a new unit.

## Corrected candidate: bounded R1 recheck

```text
candidate: HEAD/base 5927ffbba351af2f7fb8635316bbfa4ae5b31da6; corrected tracked git diff --binary HEAD SHA256 feeee793c49468d2f384127484f89bb27e32e668ea1061283f0fce90a8f6e8c3; plus accepted task artifacts
verdict: PASS
findings: none surviving; R1 closed
evidence_boundary: retained unaffected independent L1-L8 review plus bounded R1 delta; completed actual local validation supplied by delivery owner and test log independently read
reopen_owner: none
```

The same reviewer independently verified the corrected source identity.
`crates/jobs-worker/src/bootstrap.rs:839` now checks sticky failures without
consuming another stop notification; receiver errors still reach their
canonical failure disposition. The regression queues independently
acknowledged SIGTERM and SIGINT, exercises production `wait_for_stop`, and
requires the second notification to remain immediately available without
changing `first_stop`. It executed successfully in the completed workspace
test run (`/tmp/process-lifecycle-test-low-footprint.log:1016`). No runtime
failure on the old source is claimed.

The reviewer retained the initial unaffected source reasoning for admission
cancellation, partial-resource cleanup, failure observation, cancellation-time
panics, forced completion acknowledgement, unwind precedence, whole-tail
arithmetic, the diagnostics exception, operator compatibility and profile
markers. No new source blocker survived the bounded recheck.

Before PASS, the reviewer received the actual architecture, dependency policy,
current-review secret scan, build, documentation and test results, and
independently read the completed test log: 875 passed, zero failed and one
ignored case. This includes the relevant lifecycle regressions. Successful
matching build/test execution used two compiler jobs, disabled incremental
storage and omitted debug information. Assertions, overflow checks,
optimization and panic behavior were unchanged. The earlier default-profile
build also passed; ENOSPC attempts establish no test result.

The ignored Go-wire fixture case remains CI-owned. Linux-only gRPC process
tests and disabled integration suites executed zero cases locally.
Generated-profile, real-database, messaging, cache integration, provider/runtime,
CI and deployment results are not claimed. The reviewer made no edits,
acceptance decision or transition. Local acceptance remains with the delivery
actor; publication and selected CI remain with the continuation root.
