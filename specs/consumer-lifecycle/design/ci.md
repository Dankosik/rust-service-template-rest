# CI scheduling and comparable evidence

Status: ready. Implements [C1–C3](../spec.md#c1-comparable-ci-measurement)
under [System Design](system.md). Improvement is an execution result still to
be measured, not a premise of this design.

## Selected schedule

Split the current image job into two independent native jobs after `changes`:

- `image-source` observes the source image when `runtime_image=true`.
- `image-derived` runs the selected subset of `1,7,47,65` once, serially, through
  the existing `template-init-artifacts` target when `artifact_graphs` is
  nonempty. It keeps one shared staged generator target, local BuildKit reuse
  between its derived shapes, and at most one loaded derived image at a time.
- `image` becomes the lightweight `always()` aggregate for these jobs and stays
  the stable admission result consumed by `required`.

Both execution jobs retain the current draft-PR deferral, runner class,
architecture, toolchain, locked graph, release profile, selected shape set and
proof commands. Selection still comes from the existing classifier and
`initializer-matrix.py`; no new profile × harness × database matrix exists.
The source lane retains migration rehearsal in place of ordinary lifecycle
when selected. Each derived image retains generation, build, exact image ID,
hardened lifecycle/inventory, vulnerability policy and SBOM.

Both builders may import the existing `runtime-image` GHA cooked-stage cache;
only the existing trusted-event source path exports it. Derived shapes do not
overwrite that scope or add four persistent scopes. A split introduces another
checkout/builder allocation/import and may lose source-to-derived local reuse;
that cost is measured. Keep the existing finite 90-minute execution ceiling
until measurement justifies a narrower bound; do not increase it to mask a
regression. No paid runner or remote-cache product is selected.

Source/derived independence removes a known serial edge. More sharding,
changed LTO/allocator/toolchain, dropping shapes and weakened gates are not
part of this strategy. Reopen scheduling only if the measured two-lane result
fails the accepted improvement/cost boundary.

## Evidence transfer and aggregate admission

Retain evidence in separate native Actions artifacts, not Docker image
archives. The aggregate uses native `needs` results and declared upload outputs;
it does not download and re-parse the existing runner's receipts. Extend current
recorders only for missing observable identity/timing facts. PR250's graph
selection, private candidate and fail-fast gate chain remain the execution owner.

Each bundle identifies workflow run/attempt, actual checked-out Git revision
and source tree, selected lane/shapes, command/gate status and duration,
immutable image ID, and log/SBOM hashes. Derived evidence additionally retains
private source candidate revision/tree, its binding to the checked-out source,
each initialized output revision, normalized profile tuple, OpenAPI/lock hashes
and expected inventory. These IDs must not be collapsed into `github.sha`.

The aggregate directly needs `changes`, `image-source` and `image-derived`,
runs under `always()`, and checks:

1. Classifier succeeded and selected surface/shape inputs parse under the
   existing owner. Draft deferral is recognized only through that explicit
   existing policy, not a lane's unexplained `skipped` result.
2. Every selected lane succeeded. A required lane missing, failed, cancelled or
   unexpectedly skipped fails. Unselected lanes need no fabricated receipt.
3. Each selected lane exposes nonempty `artifact-id` and `artifact-digest` from
   its required `actions/upload-artifact` step. Record the native artifact link;
   archive digest and OCI image digest remain different identities.
4. Each lane passes the classifier's selection directly to the unchanged
   command owner. The derived runner's validated distinct graph set and
   unsuppressed command failures mean native job success requires all selected
   shapes and their gates. No independently supplied receipt chooses a shape or
   satisfies a missing lane.
5. Required uploads use `if-no-files-found: error`; preserve diagnostic evidence
   on failure without letting an upload override failed execution. Use immutable
   attempt-specific names `image-proof-source-<attempt>` and
   `image-proof-derived-<attempt>`, with `overwrite: false`.

GitHub reruns retain the original SHA/ref and run ID while incrementing attempt.
A failed-job rerun may reuse a successful prerequisite and its original job
outputs. Accept that prior-attempt successful source/derived lane when candidate
and selection are unchanged; record its producing attempt without requiring
equality with the aggregate's attempt. This is native same-workflow recovery,
not cross-workflow artifact reuse. An absent required output or invalid native
result fails the small aggregate. A changed candidate/selection starts new
proof and cannot reuse the earlier lane.

These guarantees come from the [native rerun contract](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/re-run-workflows-and-jobs),
[needs context](https://docs.github.com/en/actions/reference/workflows-and-actions/contexts#needs-context),
[pinned upload action](https://github.com/actions/upload-artifact/blob/043fb46d1a93c77aae656e7c1c64a875d1fc6a0a/action.yml)
and its immutable artifact outputs. The runner's [maintainer explanation](https://github.com/actions/runner/issues/1961#issuecomment-1163227296)
supports output carry-forward from retained successful prerequisites. No second
receipt parser, hash verifier or per-shape admission engine is selected.

The small aggregate predicate lives in retained
`scripts/ci/image-results.py`, invoked by `ci.yml`, with a bounded native-needs,
job-result and upload-output self-test. It consumes the platform values above;
it does not download artifacts or re-adjudicate graph/receipt content. Keep it
in initialized consumers because their source-image aggregate calls it. No new
portable-sync ownership is introduced. This is implementation placement of the
selected aggregate, with unchanged candidate and successful-lane retry semantics.

Existing selector/recorder proof plus the bounded aggregate failure/skip/output
checks observe the changed behavior. `required` and local `make plan`/`make
verify` retain their matching gate route.

## Measurement and attribution

Use the existing `scripts/ci/measure.sh` and artifact per-command measurements.
Capture generation, package/cache preparation, build dependency/link work,
lifecycle/inventory, security, SBOM, upload/download/aggregate costs. Retain
BuildKit cache-hit/transfer and stage logs; command process CPU/RSS alone does
not measure Docker daemon work. Native workflow/job timestamps establish queue,
setup/teardown, image-path and all-selected-gate elapsed time.

The historical `c3a3f18…` image job's 2,478 seconds is context only. The
coordinator's exact `2cb8718…` run supplies a fresher baseline, but differences
in source, graphs or cache prevent direct causal attribution. Create a bounded
native comparison of the final candidate under serial and split scheduling:
the serial control is a reviewable temporary branch differing only in that
schedule, with the same proof/measurement code and identical image build-input
fingerprints. Include workflow revisions and the unavoidable distinct
`VCS_REF`/consumer output commit identities in the result. No permanent second
CI framework or always-running benchmark workflow is added.

Compare the same selected source plus four shapes, runner label **and observed
runner image**, architecture, toolchain, locked package graph, compiler profile,
Dockerfile/base/tool pins, relevant source/build-context hashes and proof policy.
The initialized source trees may differ in excluded workflow metadata and
recorded commit identity; explicitly list those differences and verify their
exclusion from the compiled/build-context comparison. A runtime/code/dependency
change between arms invalidates attribution and requires a new matched control.

Use one declared cache condition per comparison. The primary production-like
comparison uses warm imports, no experimental cache writes and recorded hits,
cache-entry identity/availability and concurrent-writer interference. If cache
availability cannot be matched, classify that result as inconclusive. A bounded
imports-disabled/no-cache pair may answer the cold condition without deleting
shared caches, but a cold observation cannot substitute for the warm-path claim.
Do not build both arms concurrently against the account quota; serialize the
comparison runs so cross-arm contention does not create the apparent winner.
Within the split arm, the selected two-lane scheduling is the treatment.

Report both per-command time and whole selected-gate critical path, with total
allocated runner-seconds for image lanes and the selected workflow, cache
transfer time/bytes where observable, cache-storage size/evictions, temporary
disk usage and evidence-storage growth. Report missing metrics explicitly;
unknown cache behavior cannot support a cache-sensitive attribution.

Accept C2 only when comparable native evidence shows a shorter selected-gate
critical path with all required proof, and the additional runner/storage cost
stays inside the accepted execution envelope. There is no invented percentage
or minute target. If noise, changed inputs, quota queueing or missing evidence
prevents that conclusion, retain C2 as incomplete and reopen only the measured
scheduling/cache decision. A green new workflow alone is not an improvement.
