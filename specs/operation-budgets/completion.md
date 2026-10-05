# Operation budgets Completion

Status: local validation complete; final independent verdict and requested CI
completion pending. All writers and local check processes are joined. The root
owns `tasks.md`; this Lead owns the integrated result and PR delivery.

The single [PR #252](https://github.com/Dankosik/rust-service-template-rest/pull/252)
is still a draft against `main`. It will be marked ready after the final bounded
review verdict. No merge, deployment or live-provider conformance is included.

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
and OAuth/gRPC blocks are siblings. The same reviewer retains the final verdict
and will consume the current test-placement delta and execution evidence.

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

After final review, publish the current test-only repair, mark the existing PR
ready, and obtain terminal success for the actual selected CI and CodeQL gates
on that head. Initializer runtime/canonical projections, real database/provider
integrations, SQLx metadata, runtime image/security and actual-Go compatibility
retain their existing CI owners. The revised route still selects no local
instruction or schema check. No local heavy/full override is used.
