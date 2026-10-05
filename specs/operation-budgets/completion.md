# Operation budgets Completion

Status: in progress. The root assigned final delivery after all T001 writers
joined. `tasks.md` remains root-owned.

Initial code candidate: base `78aa3a832bfb4d7e9632ce5ebbbf1680705c31af`, 61 source
outputs, SHA256
`17ad0c00bdf94f0a7ae81bcd1174a5c4125f6fba438e8c903042900669a2408d`.
The [implementation result](implementation-result.md) records the source digest
method, interfaces, deliberate lock maintenance and compile-only feedback.

## Validation plan

`make plan` selected the workspace because manifests changed. The ordinary
criterion is `make build` and `make test`, plus the selected independent final
Implementation Review. The mixed-surface route additionally selects:

- Classifier, affected-crate planner, validation lock and verify self-tests.
- Architecture and duplication gates, their shared self-test, and the existing
  four checker projection representatives.
- Unused dependencies, dependency policy, formatting and workspace lint.
- ShellCheck of `scripts/ci/changed-surfaces.sh`, Dockerfile source checks, and
  offline Markdown link/fragment checks.

These existing local commands are run once, with focused repair and reuse of
unaffected results. All CPU-heavy execution uses the Git-common validation
lock. Rust commands use the installed pinned toolchain with command-local
`PATH`, task-owned `target`, `CARGO_PROFILE_DEV_DEBUG=0` and
`CARGO_INCREMENTAL=0`; optimization and debug assertions retain their defaults.
An initial checkpoint of 60 seconds observes actual output/stage, then adjusts
to progress. No heavy/full override or new environment is selected.

The route leaves initializer runtime/canonical projections, real database and
provider integrations, SQLx metadata verification, image lifecycle/security
and CodeQL to the existing CI owners. Instruction and schema gates were not
selected by this local diff. No live bucket, deployment or merge is included.

## Local evidence

Passed so far: classifier self-test, validation-lock self-test, formatting,
affected-crate planner self-test, declared architecture, unused dependencies,
and dependency policy. Existing cargo-shear redundant-ignore and registry
version-duplication warnings remain warnings; no registry package was upgraded.
ShellCheck of the changed script and docs-check passed after restoring the
inherited PATH (1326 links, zero errors).

The first static group used an overly narrow command-local PATH: native npx
was unavailable to the checker self-test and duplication detector; Docker was
likewise unavailable to the first docs/ShellCheck attempt. The corrected
commands prepend the pinned Cargo directory while retaining the normal PATH.
The verify self-test failed before producing its expected receipt; the corrected
PATH rerun is the next discriminating check, without changing that runner.

Projection admission rejected a directory-only candidate entry. It now lists
the new leaf's two exact files through the existing authorized-file path.
Clippy found two needless async extractor implementations and three S3
idiom/documentation/literal findings; these were repaired without changing
budget/finality policy. Remaining/invalidated local gates, workspace build and
behavior tests are pending. No final review or acceptance has occurred.

Existing compile-only diagnostics are retained at their original scope and do
not stand in for build or behavior tests.

## Review and delivery

One fresh integrated Implementation Review will consume the fixed candidate
and local evidence. Commit/push and one separate draft-to-ready PR are
authorized after local acceptance/review closure. Completion requires selected
CI gates, including `required` and `codeql-required`, to succeed at the current
PR head. No external result is claimed yet.

Current source identity after these mechanical repairs: `5231b3f34e9482d0863d5f57f33cd8328085b5b56a56fcc346a464f90b6b6bbd`
(61 outputs; same digest method as implementation).

## Draft and bounded repairs

The initial draft is [PR #252](https://github.com/Dankosik/rust-service-template-rest/pull/252).
Its first pushed commit, retained as a recovery identity, is
`aba175690c0dd726f6e3b3f0be7ae789eb67e898`. The root authorized rewriting only
this task's draft branch with an exact lease on that head, after readers join.
No other branch or ref is in scope.

The corrected-PATH verify self-test, shared checker self-test, duplication gate
and Dockerfile check passed. Workspace lint then found only one remaining
102-line table-driven OAuth test; a site-local lint reason keeps its shared
peer setup together. The [independent review](implementation-review.md) found
two profile-marker defects, now repaired without changing runtime policy.

The existing `_batch_blobs` source snapshot blocked for over five minutes in
the native projection run: it wrote all SHA lines before reading Git's stdout.
The baseline requests 48,954 bytes and this candidate 49,692, so the defect was
already present before this task. Only the hung child belonging to this
attempt was terminated; its failed receipt is retained at the Git-common
`codex/template-init/attempt.RVTDLv` locator. The root routed the narrow runner
repair into C13: native subprocess communication feeds stdin and drains stderr,
while an anonymous temporary output file removes the stdout-pipe dependency.
Declared-size parsing and the existing one-result blob dictionary remain the
owners; there is no second full stdout buffer in memory or new runner.

The first secret scan flagged ordinary Planning prose as `generic-api-key`.
The root reworded the sentence without changing its accepted meaning. No secret
or policy exemption was introduced; the rewritten draft history will be
rescanned before delivery.
