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

The historical `c3a3f18…` image job's 2,478 seconds and the coordinator's
`2cb8718…` run are context only. Different source, graphs or cache conditions
prevent causal attribution. C2 remains a measured execution outcome under
[C1–C3](../spec.md#c1-comparable-ci-measurement), which require comparable inputs
and one declared cache condition, but neither a warm-cache result nor a numerical
speed target.

### Final candidate and matched control

Freeze the repaired final candidate F before the comparison. Use the existing
native workflow with `workflow_dispatch` and actual all-surface selection in
both arms: source image, shapes `1,7,47,65`, source migration rehearsal and all
other selected gates. The temporary serial control differs from F only in the
reviewable schedule and necessary native output transport: one checkout/builder
runs source then derived proof; its lightweight derived relay preserves the
existing aggregate and counts as control overhead. The split arm uses F's
unchanged two lanes. Preserve every proof command and immutable upload in both
arms. No permanent comparison workflow is selected.

Read back actual control/F Git trees and workflow blobs. Match every tracked
non-scheduling path, all Docker context inputs, initializer inputs and retained
output content, proof/measurement commands, runner label **and observed runner
image**, architecture, toolchain, locked package graphs, compiler profile,
Dockerfile/base/tool pins and selected gates. List unavoidable `VCS_REF`,
commit-time, workflow-metadata and initialized commit-identity differences, and
show their exclusion from compiled/build inputs where applicable. Do not erase
those identities from the evidence. A runtime, generator, quality-command or
other executed-input difference invalidates the whole selected-gate comparison,
even when Docker context hashes match.

That boundary applies to the current repair. Control
`3fa14ecb728184cf8b44eb978287378d959004dd` and native run
[37530417527](https://github.com/Dankosik/rust-service-template-rest/actions/runs/37530417527)
belong to old source `8cf53b2818ffdb55320cd5fafeb7e9a75129a3d8`.
The existing readback found 1,358 matching non-scheduling tracked paths and
698 matching Docker inputs for that pair. Repaired code
`a3853f32b1fb8374b42cf1a6e75176f9bb19165e` changes Make/verify execution and the
portable manifest outside the Docker context. Therefore preserve the old run
as exact old-candidate proof and a cost/cache diagnostic; it cannot become F's
CI result or timing control. There is no whole-gate evidence bridge from Docker
equality alone. Do not dispatch the old split arm merely to complete that
superseded final-candidate comparison.

A later repair affecting execution or its inputs requires a newly fixed matched
pair; unchanged receipts remain useful only for their named old scope. A
mechanical receipt/identity refresh may retain unchanged semantic proof under
[Transition](../../../docs/spec-first-workflow/shared/transition.md), but cannot
relabel measured source, artifact or timing. Completion owns F's actual identity
and final native CI, separately from this decision.

### Selected cache condition

Select **normal imports enabled, with the shared `runtime-image` cache observed
absent** for the bounded C2 comparison. Retain `type=gha,scope=runtime-image` in
both arms and existing `workflow_dispatch` export suppression. Do not add cache
writes, cache deletion, a warming run, new scopes, a paid runner or a storage-limit
change. This is an observed native cache-miss condition; it does not mean all
Cargo, tool, registry or local BuildKit caches are cold.

The before-dispatch and during-run snapshots for the old control each contained
15 entries totalling 10,479,073,385 bytes and no `runtime-image` index/blob entry.
They support selecting this condition, but do not prove actual imports/hits for
that run or availability during a future arm. Cache storage is subject to
[GitHub access and eviction rules](https://docs.github.com/en/actions/reference/workflows-and-actions/dependency-caching#usage-limits-and-eviction-policy);
[BuildKit GHA scope](https://docs.docker.com/build/cache/backends/gha/#scope)
identifies the imported object. A new source ref does not recreate it.

Before and after each arm, preserve paginated cache API identity/availability
and storage snapshots, with timestamps and accessible ref scopes. Retain actual
BuildKit import and stage logs from every source/derived build. Admit this
condition only when those logs and snapshots establish no shared cooked-stage
restore in both arms; API absence alone or an ambiguous importer log is not a
cache-hit measurement. Record any lookup failure/timeout separately; an
asymmetric failure or unavailable cache evidence defeats attribution. Existing
within-run local reuse remains enabled: source-to-derived reuse in the serial
builder and derived-to-derived reuse in either arm are part of the schedule's
measured effect, not evidence of a warm remote import.

For the other selected jobs, compare actual restored cache keys/versions and
hit/miss states, including Cargo and tool caches. Key requests alone do not prove
matching restore results. Record concurrent trusted writers and any changed
entries/evictions; if their effect on a required comparison input cannot be
excluded, retain C2 as inconclusive. Unrelated entry changes need not invalidate
an otherwise evidenced match. Never infer transfer bytes, daemon resource use,
cache hits or evictions from elapsed time or total cache size alone.

The allowed claim is: under the recorded native cache condition, final F's split
schedule completed all selected gates faster than its matched serial control,
with the same required artifact proof and reported cost. It is not a warm-path,
universally cold, statistically typical, or every-run improvement claim. Warm
performance remains unmeasured; the existing production import/export policy
and correctness-safe rebuild behavior are unchanged.

### Alternatives and bounded execution

| Alternative | Disposition and consequence | Reopen evidence |
| --- | --- | --- |
| Require warm imports before C2 | Not selected: current snapshots provide no retained shared cache, and a new source/ref does not repair its availability. Warming/export or storage changes add work and consequences unnecessary for C1–C3. | A warm-path claim becomes an accepted requirement, or normal operation supplies stable warm evidence for a separately bounded comparison. |
| Native imports enabled with observed absence | Selected: exercises the existing correctness-safe native path without new mechanisms. Accept the narrower claim and the possibility that the pair is inconclusive. | Cache condition, executed inputs, runner image, noise or cost cannot be matched, or no shorter whole selected-gate path is observed. |
| Disable imports or force `--no-cache` | Not selected: changes normal behavior and can destroy useful intra-run reuse. It would answer a different cold condition and spend another pair. | A later bounded diagnosis specifically needs that condition; it cannot substitute for warm or normal-import evidence. |
| Bridge old control to repaired F | Rejected for whole-gate C2 and final CI: unchanged Docker bytes do not cover changed Make/verify or generated content. Keep old image/input observations at their original identity. | Only an execution-neutral identity refresh can use the unchanged-scope rule; these repairs are not such a refresh. |

Completion first collects the already-running old control's terminal status,
producing attempts, available logs/artifacts, gate times and costs within its
existing timeout. Do not cancel/repeat it merely because the cache is absent.
Use that diagnostic to identify proof failure, timeout or unavailable measurement
before allocating another full pair. A known failure that prevents a comparable
result returns to its existing repair or design owner first.

After repairs and design integration are fixed, permit at most one fresh matched
serial/split pair within the already accepted native included-quota, runner,
90-minute image-job and storage/retention envelope. Check remaining quota and
the declared cache condition before each dispatch. Existing unchanged workflow
job ceilings bound the other selected jobs. Count the old diagnostic, any
failed/retried work, control relay and both new arms in the execution cost record;
no new spending or increased ceiling is authorized here. Retire the earlier
warm-only instructions in Completion's comparison plan before dispatch.

Serialize the comparison runs so cross-arm quota contention cannot create the
winner; the split arm's two concurrent image lanes are the treatment. Do not
start the second arm while the first's required proof remains failed, cancelled,
missing or materially incomparable. If the first arm finishes with intact proof
and matching preconditions, complete the second even when the eventual result
may be slower. Native failed-job recovery remains valid for artifact admission,
but report all producing attempts and recovery elapsed time; a rerun is not a
fresh whole-workflow timing sample. A run needing recovery cannot silently stand
in for an uninterrupted comparable arm.

If this pair cannot be admitted or cannot establish the accepted reduction/cost
boundary, keep C2 incomplete and return the exact mismatch and surviving evidence
to this design owner. Do not loop over reruns, wait indefinitely for warmth,
change cache storage or schedule blanket extra experiments. Unaffected consumer,
native recovery and artifact preparation may continue.

### Result and acceptance boundary

Report both per-command time and the whole selected-gate critical path through
`required`, using native run/job/step timestamps with an explicit common start
and finish definition. Report queue/wait separately and include it in the
end-to-end workflow elapsed result; a queue-only apparent win does not establish
a schedule improvement. Show which path determines completion, so a faster
image lane cannot mask an unchanged or slower required workflow. A reconstructed
image-only counterfactual is diagnostic, not whole-workflow evidence.

Report total allocated runner-seconds for image lanes and the selected workflow,
cache transfer time/bytes where observable, cache storage/entry changes,
temporary disk usage and evidence-storage growth. Missing metrics are explicit
limitations; they cannot be assigned zero or support an unsupported cost/cache
claim. Enough observed cost evidence must remain to establish the accepted
execution envelope. Command CPU/RSS alone does not measure Docker daemon work.

Accept C2 only when the fixed pair supports that narrower claim with a shorter
comparable selected-gate critical path, all required native/artifact proof and
cost inside the existing envelope. Noise, changed inputs, quota queueing or
missing required evidence leaves C2 incomplete. A green workflow is correctness
evidence for its candidate; this ready design is measurement authority, not an
improvement, final CI or overall completion receipt.
