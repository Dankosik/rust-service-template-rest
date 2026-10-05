# T1 — Source deployment covers image inputs

Outcome: The two documented Railway watch forms currently omit admitted image
inputs. Make them agree and cover every supported effective input family, with
actionable refusal of uncovered or unsupported context/policy forms.

Consumes:
- [Specification 1](../spec.md#1-image-changing-source-changes-trigger-source-deployment).
- [Design: source deployment coverage](../design/design.md#source-deployment-coverage).

Provides: A small policy-data checker in the shared docs-check route and cheap
coverage checking of projected trees. Both documented forms carry the accepted
watch set.

Boundary: Apply the selected conservative allowlist-family model; do not build a
general Dockerignore engine, execute the IaC example, add a provider carrier or
certify deployed Railway settings. Existing build context and profile pruning
remain canonical; unsupported context changes refuse rather than approximate.

Mutable owners:
- Railway watch forms in `docs/railway-deployment-profile.md`.
- New `scripts/ci/image-inputs-check.py`; shared docs-check composition and
  documentation-surface routing in `make/template.mk` and
  `scripts/ci/changed-surfaces.sh`, including their existing focused self-tests.
- Policy checking in `scripts/tests/template-profile-projections.py`; portable
  helper custody in `template-owned.paths` and existing candidate-path inventory.

Exclusive locks: shared Make/classifier/candidate-path inventory and portable
sync custody; Railway guide; profile projection runner. Other tasks writing any
of these owners serialize. Source Dockerfile/ignore are consumed, not changed by
this task unless a demonstrated in-scope coverage defect requires repair.

Final validation:
- Claim: Both policy forms cover current and newly admitted supported input
  families, including after profile pruning; drift/refusal reaches the common route.
- Checks: Accepted Design's scoped script, docs, classifier and projection proof
  within assembled final validation; no image build solely for this policy check.
- Observable: Supported coverage passes; an uncovered/ambiguous input identifies
  the offending family or syntax and policy owner. No provider observation claimed.

Reopen if: Actual context grammar invalidates the Design's supported model; return
that mechanism decision to Technical Design. Test/checker implementation choices
and ordinary routing repairs stay with the executor.
