# T3 — Prove initialized release artifact seams

Outcome: Existing source/debug/projection evidence does not prove initialized
release artifacts. Add bounded artifact-only execution for canonical graphs
1, 7, 47 and 65 and route selected graphs serially through the existing image job.

Consumes:
- [Specification 3](../spec.md#3-derived-release-proof-covers-materially-different-binary-selections).
- [Design: minimum derived artifact proof](../design/design.md#minimum-derived-artifact-proof).
- T2's implemented container admission targets at artifact execution; an
  Implemented output suffices for coding, without an intermediate passing gate.

Provides: CI and make-verify select the same minimal initialized artifact proof;
records distinguish source candidate, initialized revision/lock, graph, immutable
image identity, command result and failure gate.

Boundary: Reuse the public initializer with complete preflight, canonical graph
tuples, one core harness and existing release/image/lifecycle targets. Build each
selected image once; retain source-image selection when its surface requires it.
Check service/migrator/worker presence or pruning and reuse provider-free lifecycle
proof. Preserve required aggregate handling of selected failed/cancelled jobs.
No lock-algorithm change, host release duplicate, database-suite multiplication,
new provider, migration execution or replacement validation framework.

Mutable owners:
- `scripts/ci/template-init-check.sh` artifact mode and existing command recorder;
  `scripts/ci/initializer-matrix.py` bounded graph selection and focused tests.
- `scripts/ci/changed-surfaces.sh`, `scripts/ci/verify.sh`, `make/template.mk` and
  `.github/workflows/ci.yml` shared selection/composition and their self-tests.
- Existing image helper integration for projected identity/filesystem/lifecycle
  proof, candidate-path inventory and portable custody only where needed.

Exclusive locks: canonical initializer runner/planner; shared image helpers,
Make/classifier/verify and inventory custody with T1/T2. Heavy proof and shared
validation lock are reserved for final validation after all writers join.

Final validation:
- Claim: The selected derived artifacts establish the four retained-binary seams
  with actual release/package/bin/default-feature selection and no pruned binary.
- Checks: Design's planner/routing/projection/script checks followed by selected
  CI-owned serial image proof. Preserve existing independent runtime coverage.
- Observable: Each selected graph records the built image and gate results;
  failure names its graph/boundary and stops the sequence. No historical source
  result substitutes for a newly selected initialized image.

Reopen if: A new binary, retention predicate or independent artifact seam defeats
the four selected representatives; Technical Design owns that coverage decision.
Ordinary runner, recorder and routing repairs remain Implementation work.
