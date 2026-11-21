# Evidence Contract

Proof boundaries, receipt reuse, extra required checks, and unavailable
infrastructure. Ordinary local work follows
[AGENTS.md](../../../AGENTS.md#validation-budget) without loading this file.

## Local Completion

[AGENTS.md](../../../AGENTS.md#validation-budget) owns ordinary local checks;
[Validation Routing](../../validation-routing.md) owns generated and mixed
surfaces.

- Tests must exercise changed behavior and actually run (or have valid reusable
  receipts): a written test, an empty test selector, or a skipped case is not a
  pass.
- Build affected deliverable entry points and required variants; report unrun
  optional checks only when they materially narrow the claim.

## Required And Optional Proof

- Extra mandatory checks must trace to an accepted requirement or external gate
  (which runs at its own stage rather than duplicating locally). A risk label,
  domain skill, or reviewer preference never invents a completion gate.
- Keep local tests inside existing fixtures, test doubles, and in-process
  routers/services rather than building throwaway compose stacks, remote
  environments, or matrices solely to claim local completion.
- If an optional check lacks Docker, credentials, or external access, record
  the material unverified scope and stop that path. If a required check cannot
  run, report `implementation complete; verification incomplete` with the exact
  missing prerequisite.
- Fix every reproducible in-scope defect (even when found by an optional
  check); leave unrelated historical defects as scoped notes.

## Design Proof

Specs and plans pin observable falsifiers and proving surfaces; Implementation
chooses test cases, fixtures, and commands. A falsifier does not mandate a live
production or Docker gate unless accepted requirements say so.

## Execution Evidence

- Retain commands, inputs, environment, results, exercised scope, and
  commit/tree identity across checkout or integration boundaries.
  [Unit Implementation](../phases/implementation/unit.md#steps) owns unit
  feedback; [Implementation Step
  4](../phases/implementation.md#4-final-verification-review-and-repair) owns
  assembled verification.
- **Reuse passing scoped receipts** whenever covered code, dependencies,
  command, inputs, and environment are unchanged—a handoff or unrelated doc
  edit does not invalidate them. Re-run affected checks whenever equivalence is
  uncertain.
- After a failure or repair, keep unaffected passing receipts and re-run only
  failed, invalidated, and not-yet-run checks. Full `just check` is not an
  automatic gate unless required.
