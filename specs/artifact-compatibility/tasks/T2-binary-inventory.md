# T2 — Admit each retained binary's runtime inventory

Outcome: Existing OS/image scanning can succeed without establishing each retained
Rust binary's graph. Strengthen the existing security/SBOM targets so every
independently expected entrypoint has the selected identity, binary and admitted
native runtime graph before reports or publication consume it.

Consumes:
- [Specification 2](../spec.md#2-artifact-inventory-is-complete-enough-to-support-its-scan-claim).
- [Design: per-binary inventory](../design/design.md#per-binary-rust-inventory).

Provides: Existing container-security/container-sbom targets enforce the native
JSON admission contract and convert admitted data through pinned Trivy. T3 can
reuse these targets on projected artifacts.

Boundary: Retain separate auditable package/bin/default-feature builds, scanner
pins, exceptions, severity and publish ordering. Derive expected entrypoints and
identity independently from retained manifests/profile and Dockerfile; worker
retention is jobs OR messaging. Reject missing/pruned binaries and malformed,
root-only, disconnected or dangling graphs per Design. Preserve per-application
graphs in native SBOM conversion. Use private temporary outputs and fixed image
identity. No custom scanner/SBOM format, new dependency, whole-lockfile oracle or
independent machine-code/tampering claim.

Mutable owners:
- `scripts/ci/runtime-image-inventory.py` and existing image/security/SBOM scripts,
  optional small shared invocation helper, their existing focused fixtures/tests.
- `make/template.mk` target composition and existing runtime-image filesystem
  checks; `.github/actions/publish-image/action.yml` only for consuming the
  strengthened existing gates before its existing effects.
- Portable helper and candidate-path custody in their existing inventories.

Exclusive locks: image scripts/targets and fixtures; Make and portable/candidate
inventories shared with T1/T3. Serialize overlapping writers; no image/build
execution is scheduled as a task-completion gate.

Final validation:
- Claim: Every expected executable supplies its associated complete structural
  runtime graph to the unchanged security verdict and converted SBOM.
- Checks: Design's scoped admission/native-format and shell checks plus selected
  assembled CI image proof. Actual pinned output must exercise new assumptions.
- Observable: Admitted reports include each binary application and dependencies;
  missing identity, binary or graph content refuses before publishing output.

Reopen if: Pinned native output or a legitimate runtime graph contradicts the
selected admission semantics; Technical Design owns that decision. A coherently
false producer graph remains outside the accepted structural guarantee.
