# Evidence Contract

Evidence semantics for local completion, proof reuse, extra required checks,
and unavailable infrastructure. Ordinary local work follows
[AGENTS.md](../../../AGENTS.md#validation-budget) without loading this file.

## Local Completion

[AGENTS.md](../../../AGENTS.md#validation-budget) owns the ordinary local bar;
[Validation Routing](../../validation-routing.md) selects commands for mixed or
generated surfaces.

- Tests must meaningfully exercise changed behavior and actually run (or have
  valid reusable receipts). A written test, an empty test selector, or a
  skipped case is not a pass.
- Build affected deliverable entry points and required variants, reusing
  adequate existing coverage rather than re-proving unchanged code.
- Once local checks and explicit additions pass, finish local verification.
  Report unrun optional checks only when they materially narrow the claim; CI,
  runtime observations, and deploys require their own evidence and authority.

## Required And Optional Proof

- Extra mandatory verification must trace to an explicit accepted requirement
  or an applicable external gate (which runs at its own stage rather than
  duplicating locally). A risk label, domain skill, or reviewer preference
  never invents a new completion gate.
- Keep local tests inside existing fixtures, test doubles, and in-process
  routers/services; do not build throwaway compose stacks, remote environments,
  or validation matrices solely to claim local completion.
- If an optional check lacks Docker, credentials, or external access, record
  the material unverified scope and stop that path. If a required check cannot
  run, report the exact missing prerequisite and incomplete scope honestly.
- Fix every reproducible in-scope defect (even when found by an optional
  check); leave unrelated historical defects as scoped notes.

## Design Proof

Specs and plans pin observable falsifiers and proving surfaces; Implementation
owns choosing test cases, fixtures, and commands. A proposed falsifier does not
automatically mandate a live production or Docker gate unless accepted
requirements say so.

## Execution Evidence

- Retain actual commands, inputs, environment, results, and exercised scope
  (attaching commit/tree identity across checkout or integration boundaries).
  [Unit Implementation](../phases/implementation/unit.md#steps) owns unit
  coding feedback; [Implementation Step
  4](../phases/implementation.md#4-final-verification-review-and-repair) owns
  the single assembled verification boundary.
- **Reuse passing scoped receipts** whenever covered code, relevant
  dependencies, command, inputs, and environment are unchanged—a handoff or
  unrelated doc edit does not invalidate them. Keep original candidate and
  scope; re-run affected checks when equivalence is uncertain.
- After a failed check or repair, retain unaffected passing receipts and re-run
  only failed, invalidated, and not-yet-run checks. Running full `just check`
  is not an automatic completion gate unless required.
- When required final proof cannot run, report `implementation complete;
  verification incomplete` for that scope with the exact gap.

## Evidence Result V1

Use one record per formal verification claim when reporting across boundaries:

```text
claim: <assertion>
candidate: <commit/tree/bounded diff identity>
scope: <package/files/behavior>
command: <exact command>
inputs: <changed paths or stable input identity>
environment: <relevant tags/services/toolchain>
result: pass | fail | blocked
duration: <wall time>
status: verified | partially_verified | not_verified
gap_or_next_owner: <none, unverified remainder, or owner>
```

A summary inherits the weakest status among its covered claims.
