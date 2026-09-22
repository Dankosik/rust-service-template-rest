# Intent: initialize and maintain derived Rust services

## Problem

The stage-8 template is runnable but still carries template identity and every
implemented capability. A derived service has no supported initialization or
safe way to adopt later portable tooling and agent instructions.

## Desired outcome

Deliver roadmap stage 9 locally: one initializer selects implemented profiles
and service identity, and a bounded sync adopts portable content without
replacing service-owned code, policy, or unrelated work. Complete the applicable
Definition, Technical Design, Planning, Implementation, review, and local
acceptance boundaries; mark the stage complete only after its exit criteria
are proved on main.

## Affected actors and systems

Template maintainers, developers initializing derived services, their coding
agents, and the existing local and GitHub Actions validation routes.

## Scope and non-goals

Service identity, supported existing profile combinations and physical removal,
profile markers, template.lock, template-owned.paths, check/apply/instructions-only
sync and refusal rules, purity, initializer CI matrix, and docs/template-sync.md.
Include stage-7 disciplines/skills/evaluation obligations only where this stage
reaches them, and the first dispatched ledger obligation if orchestration is
selected. No stage-10 capability, release, deployment, push, merge, or publication.
The Go template is a read-only source of problems and rationale; preserve Rust
crate boundaries, generated OpenAPI ownership, and accepted refactors.

## Constraints

Use existing or maintained mechanisms where they meet the requirements; record
the evidence for any template-owned gap. Technical decisions belong to agents.
Preserve unrelated work and obtain proof at the actual acceptance boundary.
Assumption: existing adapters may be selected individually or together; no new
adapter or runtime capability is implied. Reopen if the supported source set
changes before implementation.

## Success signal

Every supported selection initializes, builds, and passes the real make check
in the CI matrix; a derived repository adopts a committed source snapshot and
checks cleanly while retaining its identity and service-owned content. Refusals
protect local content before deterministic failure, and phase/review evidence
supports the final local claim on main.
