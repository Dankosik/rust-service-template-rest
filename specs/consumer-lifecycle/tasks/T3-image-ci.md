# T3 — Complete native image proof on two lanes

Outcome:
The native workflow schedules source image work independently of serial derived
shapes, then admits only complete selected results through the stable `image`
aggregate, with enough existing-recorder evidence for a matched speed/cost
comparison. C2 improvement is a final measured result, not implied by this code.

Consumes:
- [C1–C3](../spec.md#c1-comparable-ci-measurement) — same proof and measured
  improvement obligations.
- [CI design](../design/ci.md) — fixed two-lane schedule, native evidence and
  retry custody, cache policy and matched serial control.
- [Ownership](../design/ownership.md) — existing workflow, artifact runner,
  measure and classifier owners; admitted PR250 source `2cb8718…`.
- Native exact-candidate CI and comparable measurement gate Completion.
  Template PR/push/normal native CI authority already exists; included-quota
  availability and unchanged execution envelope are checked before the bounded
  comparison. Extra spending or a changed effect envelope returns to root.

Provides:
- `image-source`, serial `image-derived`, stable `image` aggregation and
  unchanged required-gate selection, including native retained-lane reruns.
- Existing receipt/measurement extensions for exact identity, producing
  attempt, gate time and cost; a final matched-control comparison is executable
  without a permanent second CI framework.

Boundary:
Preserve source plus shapes `1,7,47,65`, draft selection, existing release
profile, pins, hardened lifecycle, vulnerability policy, SBOM and artifact
inventory. Keep source as the sole trusted cache exporter. Aggregate native
job results and mandatory upload outputs; no receipt-driven replacement gate,
cross-workflow proof reuse, image transfer, profile/harness matrix or four-way
fan-out. Extend recorders only where accepted identity/timing is absent.
Temporary serial control and measurement observations belong to Completion.

Mutable owners:
- `.github/workflows/ci.yml` image lanes/aggregate and necessary existing
  `required`/routing parity only.
- `scripts/ci/template-init-check.sh`, `scripts/ci/measure.sh`, existing
  recorder/selector self-tests for the changed contract.
- Existing `changed-surfaces.sh`/self-test, `make/source.mk`,
  `make/template.mk`, `verify.sh` only if lane/routing parity requires a delta.

Exclusive locks:
- CI workflow image admission and shared artifact-runner/recorder owner.
- Shared source/self-test/classifier/make routing when mutated; serialize
  conflicts with T1/T2 while independent implementation proceeds.

Final validation:
- Claim: Selected lanes, shapes, identities and mandatory evidence remain
  complete under success/failure/skip and native rerun semantics. C2 additionally
  requires a shorter comparable selected-gate critical path at retained proof
  and permitted runner/storage cost.
- Checks: Consolidated script/workflow/routing checks, exact native candidate
  CI, and Design's bounded serial-versus-split comparison with matching build
  inputs, proof, observed runner image, cache condition and cost accounting.
  Implementation chooses focused cases/commands. Serialize comparison arms;
  do not rerun successful unchanged gates merely to relabel receipts.
- Observable: Native selected jobs and required aggregate pass with exact
  producing attempts/artifacts; source and every selected derived image retain
  their own bindings. Whole selected-gate elapsed time and total allocated
  runner/cache/storage work support the comparison. The historical 2cb run is
  context, not a causal speed result; unmatched/noisy evidence leaves C2 open.

Reopen if:
Native aggregation/rerun semantics cannot preserve admission, or comparable
measurement fails the accepted improvement/cost boundary: reopen only the CI
Design decision. Do not drop shapes/gates or silently change optimization/pins.
