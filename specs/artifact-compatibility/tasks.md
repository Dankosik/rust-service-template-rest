# Artifact compatibility delivery

status: ready

Completion: The separate PR implements all seven [Specification](spec.md)
requirements through the accepted [Design](design/design.md), with one assembled
final validation result, resolved final delivery review and selected CI proof.
Local proof, initialized-image proof and publication remain separately identified;
this work claims no deployed artifact or live recovery result.

Global constraints: [Intent](intent.md) owns authority;
[Design](design/design.md#file-ownership-and-validation-integration) owns mechanisms,
ownership and proof integration. Retain the lock projector, dependency/toolchain
versions and runtime/provider choices. Service policy owns retention, RPO and RTO.
Use the current [Implementation owner](../../docs/spec-first-workflow/phases/implementation.md)
and [Planning Ledger Contract](../../docs/spec-first-workflow/phases/planning/ledger-contract.md).
The root continuation coordinator binds as the sole Ledger Orchestrator; Leads
return Implemented, and one delivery owner validates the assembled candidate.
No per-task test/review gates, duplicate full builds or database matrices, live
providers, restores, merge or deployment. CI-owned heavy proof stays in CI.
The coordinator owns authorized branch publication and the separate PR.

## Tasks

- [x] T1: Documented source deployment watches cover effective image inputs and refuse drift.
  - Depends on: none.
  - Provides: shared docs-check coverage guard, including initialized projections.
  - Packet: [T1](tasks/T1-source-input-coverage.md).
- [x] T2: Every retained image entrypoint supplies an admitted Rust runtime inventory to scanning and SBOM conversion.
  - Depends on: none.
  - Provides: strengthened existing container-security/container-sbom boundary.
  - Packet: [T2](tasks/T2-binary-inventory.md).
- [x] T3: Selected initialized artifacts prove each retained-binary seam through existing CI image owners.
  - Depends on: T2's implemented image admission targets, consumed by artifact execution; no passing receipt is a coding prerequisite.
  - Provides: four canonical artifact representatives, bounded selection and serial recorded CI proof.
  - Packet: [T3](tasks/T3-derived-artifacts.md).
- [x] T4: Adoption and operating guidance states the real identity, compatibility and independent-store recovery boundaries.
  - Depends on: none; consumes the accepted Design, not future execution results.
  - Provides: existing guides and service production-contract fields aligned with requirements 4–7.
  - Packet: [T4](tasks/T4-operating-guidance.md).

The units are independently consumable outcomes: T1 prevents missed deployment
inputs; T2 strengthens the source image gate without needing derived coverage;
T3 adds initialized artifact coverage; T4 guides adoption and operation without
certifying runtime execution. Their internal implementation layers are not tasks.
Initially T1, T2 and T4 are decision-ready. Their shared writable owners require
serialization or narrower disjoint lanes chosen by their Leads; they introduce
no artificial code dependencies. T3 becomes fully ready after T2 is integrated.
All checks and any repairs are consolidated after all four implementations join.

Required final evidence follows the Design's final-validation boundary. The
executor chooses cases, fixtures and commands while implementing and records
them for that boundary. Selected CI artifacts must exercise the new native-output
assumptions; unavailable mandatory CI evidence leaves that claim incomplete.

## Execution

The root `/root` is bound as the sole Ledger Orchestrator. Implementation
checkboxes below remain completion of code only; global Completion owns proof.

| Unit | State | Execution owner | Current write boundary |
| --- | --- | --- | --- |
| T1 | Implemented; unverified | `/root/artifact_input_coverage` | Writers stopped; shared owners released; coordinated T4 marker entries integrated |
| T2 | Implemented; unverified | `/root/artifact_binary_inventory` | All image/Make/inventory writers stopped; strengthened targets available to T3 |
| T3 | Implemented; unverified | `/root/artifact_derived_images` | All initializer/planner/CI/Make writers stopped; shared owners released |
| T4 | Implemented; unverified | `/root/artifact_operating_guidance` | All documentation/comment writers stopped; shared guide owners released |

All four implementation units are assembled and their writers stopped. The
delivery owner `/root/artifact_derived_images` returned local `Accepted` with
independent integrated review `PASS`; [Completion](completion.md) records actual
scoped results and invalidated attempts. No successful aggregate `make verify`
receipt is claimed. Required CI remains outstanding, so global Completion stays
pending. The root now owns commit, separate pull request and CI readback.

T4's guide projection requires a mechanical `template_profiles.json` closure.
T1 owns that shared file: move the existing two worker-guide markers to the
worker predicate and add four profile-scoped Production Contract guide-link
markers. T4 authors matching document blocks without writing the inventory.
This preserves accepted jobs-or-messaging retention and optional-guide pruning;
it introduces no runtime or dependency change.

T1 returned `Implemented` with the policy checker, behavior cases, shared
docs-check/routing and projected-tree checks. Only Python compile/JSON parsing
feedback ran; behavioral, routing and projection claims remain for assembled
validation. No task acceptance or release proof is inferred from that handoff.

T4 returned `Implemented` for adoption/artifact/rolling/recovery guidance. Its
matching marker inventory came from T1; runtime commands, watch forms, dependency
choices and existing jobs custody wording are preserved. No documentation suite,
provider operation or restore ran for that unit; final consistency/link review
remains in the assembled delivery boundary.

T2 returned `Implemented` with per-binary filesystem/identity/native-graph
admission and pinned native JSON reporting/conversion at the existing targets.
Only Python AST and shell syntax feedback ran. Tests and actual native-output
assumptions remain for final assembled local/CI validation. T3 consumes that
implemented interface without an intermediate acceptance gate.

T3 returned `Implemented` with artifact-only execution, bounded representative
selection, fixed-ID forwarding and CI/verify parity. Only shell/Python/JSON/YAML
syntax feedback ran. The image-job 90-minute serial budget is a forecast based
on existing source-image timing plus preflight/native-gate allowance; first CI
timings must test it before any further budget change. No additional jobs,
provider suites or remote-cache namespaces were introduced.

## Planning custody

Base: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6` on
`codex/artifact-compatibility-20261005`.
Specification SHA256: `4473a7d109586ddfbb14374763f11a59df8a3a5771dfcee561301b8322d13f54`.
Design SHA256: `34725e5df0acd56aaade8e30b0234fb2c62c611e05fab9b90779dbf298b6aa67`.
Their [Definition](definition-review.md) and [Technical Design](design/review.md)
reviews are PASS. [Planning review and transition](planning-review.md) is PASS;
Implementation has not started.
