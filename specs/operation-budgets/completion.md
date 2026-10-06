# Operation budgets Completion

Status: resumed local Accepted; fresh integration review PASS; publication and
selected current-head CI are pending. All writers, reviewers and local check
processes are joined. The root remains the sole `tasks.md` writer.

The delivery remains [PR #252](https://github.com/Dankosik/rust-service-template-rest/pull/252).
The assembled source incorporates main `699887b18594088a59bcc23a049d290d089f6da1`
and preserves its runtime-progress and resource-custody changes. Current main's
numeric policies are unchanged by this integration. No merge to main, deployment
or live-provider certification is included.

## Resumed candidate: 2026-10-06

The comparison base is main `699887b18594088a59bcc23a049d290d089f6da1`.
The fixed source bundle has 64 non-`specs/` outputs, SHA256
`394926e9b17681efedec4c2a2dc69a4d97836a83db7ae530a5b0be79734033bd`.
For this resumed receipt the exact serialization is each sorted changed path,
NUL, Python `format(path.stat().st_mode, "o")`, NUL, exact file bytes, NUL.
Thus a regular file uses `100644`, without a `0o` prefix. There are no deleted
or untracked source paths. Receipt-only changes do not affect this identity.

The previous remote head was `667b67971ae91fb67feeb4bb4ac4d345cf0b8d7c`.
Its historical receipt commit triggered a Gitleaks false positive on the public
comparison commit ID. Native interactive rebase changed that wording only;
rewritten task head `41af344b34bb60b02e40c03bf37323bf06cbf205` retained the exact
old tip tree `7f9aa868a31fc02403a1a3e7c807ec1e81c77810`. Both local receipt and
ledger edits were backed up, restored and verified byte-for-byte. No scan ignore
or policy waiver was introduced. Publication replaces only this task's branch
with an exact lease on the previous remote head.

The main integration keeps cache admission and synchronous abandoned-exchange
retirement before slot reuse alongside the operation context. S3 retains main's
response-limit interceptors, fair upload-body polling and unread-tail allocation
while keeping autonomous deadline/cancellation cleanup. Transport observer and
drain custody, sanitized panic handling, prepared DLQ publication and yielding
webhook preparation remain composed with the budget changes. Cargo resolved the
lockfile natively from the merge base; all 577 registry package identities and
checksums from incoming main are unchanged, including its vendored async-nats
selection. The canonical configuration/integration documentation now describes
context-aware auth, cache and complete-download budgets.

## Resumed local proof

Commands used pinned Rust 1.99, locked Cargo, the task's own target, one build
job, dev/test debuginfo zero and incremental disabled. Optimization and debug
assertions kept their defaults. CPU-heavy commands ran serially under the
Git-common validation lock. No shared cache or external environment was changed.

| Scope | Actual result |
| --- | --- |
| Matching build | `make build` passed in 156.71 seconds. Later changes only consolidate a test fixture. |
| Workspace lint | `make lint` passed; two test-only repairs satisfy incoming main's lint policy: remove an unused import and scope a deliberately synchronous fixture exception to its one statement. |
| Ordinary tests | `make test` recorded 963 passes, two failures and three ignored entries. This aggregate remains a failed run; its affected targets are closed below. |
| HTTP failed target | A deterministic control reproduced the missing recovery event when an uninstrumented sibling first registered the shared tracing callsite. The duplicate panic scenario was consolidated into the capturing fixture, preserving status, content type, request-id/header equality, sanitized body and exact event assertions. Temporary diagnostic code was removed. Workspace-feature compilation passed; the same HTTP library passed 88/88 tests. |
| Lifecycle failed target | The privacy-log fixture initially exceeded its readiness wait. Its focused test passed unchanged, then the original workspace-feature lifecycle binary passed 17/17. No service, timeout or assertion change was made; a specific startup cause is not claimed. |
| Final test delta | `make fmt-check` passed; `make lint-changed PKGS=infra-http` passed in 23.96 seconds. No production lint was relaxed. |
| Profile custody | `make template-quality-projections` passed all four existing representatives in 159.92 seconds: minimal, retained, outbound-only and inbound-only. Snapshot `7de1c13353590e7b37d39ebc963dd453e53aa277`; receipt `codex/template-init/attempt.Wip9Hv`. |
| Documentation | `make docs-check` passed: 2019 links, zero errors. Receipt edits preserve the already checked link targets. |
| Secrets | Worktree scan and all four rewritten branch commits passed with zero findings. The publication commit receives a final scan before push. |
| Independent review | Fresh integrated-candidate review returned PASS with no findings on the source identity above. It consumed the actual proof and bounded fixture repair; unaffected original review reasoning remains retained. |

The ordinary test obligation is closed by the original passing scopes and the
two scoped reruns, without manufacturing a successful aggregate receipt. The
three ignored entries are the actual-Go compatibility case (CI-owned), the
Linux release CPU-quota case (CI-owned), and a child-only blocked-stdout fixture
that its passing parent process test invokes. Provider/database integrations
remain CI-owned; no missing suite is counted as a local pass.

Resumed logs are under `/tmp/operation-budgets-`: `resume-local-proof.log`,
`resume-lifecycle-focused.log`, `resume-lifecycle-workspace.log`,
`http-panic-causal.log`, `http-panic-causal.patch`,
`http-panic-repair-compile.log`, `http-panic-repair-tests.log`,
`resume-static-proof.log` and `resume-secrets.log`.

## Remaining delivery

Publish the resolved merge and receipts to the existing ready PR, then obtain
actual selected CI and CodeQL success on that published head. This includes the
selected runtime-progress, integration, image and initializer gates and the
`required`/`codeql-required` aggregators. Earlier-head passes do not establish
this result. The historical runner outage below is no longer the current stop.
The root retains final ledger completion after those results. Final remote
readback can be recorded without an extra source/CI cycle for a self-referential
receipt commit.

```text
unit: Completion
verdict: local Accepted; requested delivery pending
candidate: main 699887b18594088a59bcc23a049d290d089f6da1; source SHA256 394926e9b17681efedec4c2a2dc69a4d97836a83db7ae530a5b0be79734033bd
review: PASS; fresh integration review and causal fixture recheck complete
external: existing PR252 ready; resolved candidate publication and current-head CI pending
next_owner: T001 Lead publishes with the exact lease and consumes selected current-head CI; root owns final ledger completion
```

## Historical receipt: 2026-10-05

The following is the earlier snapshot, retained as evidence of its scope,
failures and recovery. Its candidate and external statuses are historical.

Status: local Accepted; requested CI completion Blocked by the confirmed GitHub
Actions hosted-runner outage. The independent final review is PASS. All writers, reviewer turns and local check processes are joined. The root
owns `tasks.md`; this Lead owns the integrated result and PR delivery.

The single [PR #252](https://github.com/Dankosik/rust-service-template-rest/pull/252)
is ready for review and mergeable against `main`, at remote head
`667b67971ae91fb67feeb4bb4ac4d345cf0b8d7c`. Base remains
`78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`. No merge, deployment or
live-provider conformance is included.

## Candidate and environment

Base: `78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`. Current source bundle: 62 outputs,
SHA256 `df35e6753a154d932dcb6ff832f32c5bbe0c9dc87a07ff5b576220fcb823959b`. The digest method in the
[implementation result](implementation-result.md) excludes `specs/`, so receipt
updates do not change this code identity.

Local proof used macOS aarch64, pinned Rust 1.99, locked Cargo, the task-owned
`target`, command-local dev/test debuginfo zero and incremental disabled.
Optimization and debug assertions retained their defaults. CPU-heavy commands
used the Git-common validation lock. The final test retry used one Cargo build
job. No shared cache, unrelated worktree or external infrastructure was changed.

## Local evidence

| Scope | Actual command/result |
| --- | --- |
| Workspace build | `make build` passed in 55.72 seconds on `c64716166750e225b230b7c186c9e0ff20bca68b`. The later delta only relocates/renames a test. |
| Workspace ordinary tests | `make test` with one build job executed 878 passing tests and found one gRPC fixture failure; a jobs-worker lib-test artifact damaged by the earlier ENOSPC could not execute. All unaffected passing scopes are retained. The two failed scopes are closed below; no synthetic aggregate receipt is claimed. |
| gRPC repair | `make test-package PKG=infra-grpc` passed 13 unit and 29 transport tests. The observation assertion remains 257 series. The same public PreparedCall case was moved to the existing transport binary to avoid poisoning process-global metric handles with a noop recorder. Its name now describes parent cutoff/cancellation ownership. |
| jobs-worker recovery | The exact 1968-byte mode-0644 truncated test executable was removed. `make test-package PKG=jobs-worker` rebuilt and passed 12 unit plus 8 process tests. No assertion or production code changed. |
| Budget regression falsifier | The retained `synchronous_preparation_cannot_restart_the_local_cutoff` case compiled against the actual base outbound HTTP owner and failed at `expired preparation must not dispatch`: the expired preparation still opened a connection. Restoring the current source byte-for-byte and running the same case passed. Compilation failure was not used as negative evidence. |
| Formatting/lint | Workspace lint plus its focused OAuth repair and final `make lint-changed PKGS=infra-grpc` passed their combined scopes. `make fmt-check` passed after the test move. The source repair does not relax a production lint. |
| Graph/security | Architecture, duplication, unused-dependency and cargo-deny gates passed. All 578 registry package identities, versions and checksums remain unchanged. Existing warning-only duplicate versions/redundant ignore were retained. |
| Routing/checkers | `changed-surfaces-check`, `affected-crates-check`, `validation-lock-self-test`, `verify-check` and `quality-check-self-test` passed. |
| Profile custody | `make template-quality-projections` passed all four existing representatives: minimal, retained, outbound-only and inbound-only. Snapshot `0309f645658de528e9bda7c196e5586b30c4b076`; Git-common receipt `codex/template-init/attempt.rRKxmi`. |
| Shell/image source | Changed-script ShellCheck and `make dockerfile-check` passed, no Dockerfile warnings. |
| Documentation/secrets | `make docs-check` passed 1331 links with zero errors; later receipt edits preserve its relative link/fragment targets. Worktree and rewritten commit-range Gitleaks scans passed. Later test relocation adds no credential-like data. Current-head CI remains the final scan owner. |

The ordinary workspace test obligation is closed by the original run and its
scoped repair reruns, with unchanged code/dependencies/inputs outside those
scopes. The only ignored case is `rust_production_wire_exports_for_go`; its
actual-Go input and execution remain explicitly CI-owned. Disabled real-database
and provider suites are not counted as local passes. Compile-only receipts
remain compilation evidence only.

Local logs are retained under `/tmp/operation-budgets-`: `workspace-proof-01a10d69.log`,
`workspace-tests-01a10d69-r1.log`, `grpc-metric-interaction-before.log`,
`grpc-metric-repair-tests.log`, `jobs-worker-repair-01a10d69.log`,
`c7-baseline-failure.log`, `c7-current-pass.log`,
`projections-01a10d69-r2.log` and `final-grpc-lint-01a10d69.log`.

## Repairs and review

The [independent review](implementation-review.md) found two C13 marker defects.
Both have a bounded source recheck with no surviving defect: the introspection
end marker remains a full line through repeated rustfmt, and the guide's OAuth
and OAuth/gRPC blocks are siblings. The same reviewer returned final PASS on commit
`928963fd32f8ff9676987a49df0af3903a0261a2`, consuming the test-placement delta
and actual execution evidence. The receipt-only publication commit preserves
its source identity and semantic scope.

The real profile run exposed an existing Git pipe deadlock in `_batch_blobs`.
The root routed its narrow repair into C13: native subprocess communication
feeds stdin/drains stderr while an anonymous stdout file preserves streaming
blob parsing and the single in-memory result dictionary. The original failing
receipt `codex/template-init/attempt.RVTDLv` is retained. The final unchanged
projection command now passes; no new runner, dependency or policy was added.

The first test build exhausted local disk during parallel linking. Recovery
removed only this task's metadata-only check artifacts and completed test
executables, retaining source, logs and dependency libraries. One serial retry
reached actual test execution. The later corrupt executable was rebuilt at its
own package boundary. No error was relabeled as a pass.

The gRPC failure was reproduced on the existing binary: observation alone
passed, but PreparedCall followed by observation failed 256 versus 257. Moving
the public API test between existing test binaries closed that interaction;
production metrics, limits and expected labels are unchanged. The case retains
an independent public contract: finishing a stopped prepared call cannot cancel
its supplied parent.

## Remote history and remaining CI

The first draft head `aba175690c0dd726f6e3b3f0be7ae789eb67e898` is retained for
recovery. Gitleaks had classified benign Planning prose as a key. The root
reworded it with unchanged meaning and authorized an exact leased replacement
of this task's branch only. The rewrite to
`a9fb45ad79ad3097cdfed081b0a4db001a40eb10` succeeded; no ignore/waiver was added.
The next normal push published `c64716166750e225b230b7c186c9e0ff20bca68b`.

The initial CI/CodeQL failures were platform failures before command execution:
GitHub annotations state `The job was not acquired by Runner of type hosted even
after multiple attempts`, with empty runner names and zero steps for the failed
jobs. A passing old `required` job did not establish any unexecuted gate. Those
results are not acceptance evidence for the next head.

The accepted code and review receipt are published at
`667b67971ae91fb67feeb4bb4ac4d345cf0b8d7c`; PR #252 is ready. The actual
ready-for-review CI run is `37372866606` and CodeQL run is `37372866564`, both
for that head. After cleanup of only this PR's superseded draft runs, both
current workflows are `queued`, with only their `changes` job queued and no
terminal conclusion. Their `required` and `codeql-required` gates have not
executed; no selected integration, image or initializer result is claimed.

GitHub's official incident `3q1yb5m7ltvb` is `investigating`, impact `critical`,
Actions `major_outage`; it explicitly reports hosted-runner assignment and
workflow-start delays. Incident: [GitHub Actions outage](https://www.githubstatus.com/incidents/3q1yb5m7ltvb).

Superseded draft runs `37372588809` and `37372588974` were holding workflow
concurrency on unexecuted always-run aggregators after their other jobs were
cancelled. Supported native force-cancel requests closed both as `cancelled`
and released the current ready runs from `pending` to `queued`. No current
ready run, other PR, setting, workflow, or infrastructure was cancelled or
modified. No blind rerun or waiver was used.

This final external-state receipt is intentionally local and uncommitted, so
its bookkeeping does not change the accepted remote head or restart CI. The PR
body carries the same local-PASS/external-pending distinction. The root retains
sole ledger ownership. No automation or scheduled follow-up was created.

External-state snapshot: 2026-10-05 21:11:32 UTC.

## Reopen trigger and Completion result

Resume when GitHub restores hosted capacity and these ready runs can execute.
Read back the PR head/base and run inputs first. Reuse unchanged local evidence;
consume these ready-for-review runs, or rerun only the matching platform-failed
current-head jobs after capacity recovery. Source failures return to the same
T001 owner and invalidate only their affected proof. Completion still requires
actual selected gates, `required` and `codeql-required`, to succeed at the
current head. Draft skips and an unexecuted green aggregate are insufficient.

```text
unit: Completion
verdict: Blocked
candidate: base 78aa3a832bfb4d7e9632ce5ebbbf1680705c31af; remote head 667b67971ae91fb67feeb4bb4ac4d345cf0b8d7c; source SHA256 df35e6753a154d932dcb6ff832f32c5bbe0c9dc87a07ff5b576220fcb823959b
local: Accepted; consolidated workspace build/test and selected local gates passed
review: PASS; fresh integrated review and bounded causal repair rechecks complete
external: PR #252 ready; CI 37372866606 and CodeQL 37372866564 queued, required gates unexecuted during confirmed Actions major outage
next_owner: GitHub restores hosted capacity; T001 Lead resumes current-head CI observation and any evidenced repair
```
