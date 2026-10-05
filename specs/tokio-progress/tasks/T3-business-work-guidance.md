# T3 — Business-work admission and lifetime guidance

Outcome:
Replace incomplete guidance about blocking/CPU work and stale runtime descriptions with the accepted discipline: bound admission before submission and retain capacity, completion observation and shutdown ownership until actual execution ends.

Consumes:
- [Specification: Guidance](../spec.md#guidance-for-business-work) and [unchanged behavior](../spec.md#deliberately-unchanged-and-non-goals) — business-work rules and exclusions.
- [Mechanism: Business-work guidance](../design/mechanism.md#business-work-guidance) and [ownership](../design/ownership.md#files) — canonical owners and skill-carrier constraints.
- T2 Implemented and assembled — releases shared runtime/configuration document owners; this is implementation scheduling, not a proof gate.
- [Prompt Maintenance](../../../docs/prompt-maintenance.md) and [Skill Authoring](../../../docs/skill-authoring.md) — current method/carrier editing rules, loaded before governed edits.

Provides:
- Current runtime/contributor guidance and mechanically synchronized carriers where required, without a new runtime capability.

Boundary:
The existing runtime lifecycle document owns the distinction between short bounded sync work, blocking I/O, sustained CPU work and immediately-ready async loops. The rust-tokio method names that owner using permitted prose and retains actionable admission/lifetime rules. Link existing job cancellation/effect/fencing guidance to that runtime owner without changing job policy.

Explain permit custody beyond timeout, completion/panic observation, cooperative cancellation, residual shutdown execution, the shared blocking pool's file/DNS use, and why worker-count tuning or an await is not a progress guarantee. Keep cheap bounded computation inline; a separate CPU executor needs a concrete later workload decision. Correct the stale available-parallelism default description and any remaining in-scope prose that claims runtime timeout kills blocking execution. Preserve the logging semantics documented by T2. Canonical source is edited before any generated carrier, using existing repository generation only.

Mutable owners:
- Business-work/runtime guidance in `docs/architecture/runtime-lifecycle.md`, current runtime-default description in `docs/configuration-source-policy.md`, and relevant cancellation guidance in `docs/background-jobs.md`.
- Canonical `.agents/skills/rust-tokio/SKILL.md` and only its required generated carriers from existing repository tooling.

Exclusive locks:
- Runtime lifecycle and configuration-source documents, released by T2 before this unit begins.
- rust-tokio canonical skill and its generated carrier synchronization.

Final validation:
- Claim: Canonical guidance and carriers express one consistent admission/actual-lifetime rule and current runtime defaults, without inventing a new execution API or policy.
- Checks: Static consistency, repository documentation-link and instruction/carrier validation at the single final assembled boundary; Implementation selects the concrete existing commands. No agent benchmark or model-behavior claim.
- Observable: Guidance preserves bounded cheap inline work, explains cancellation's actual limits and holds admission through real execution; links/carriers resolve and agree with canonical sources.

Reopen if:
Technical Design if the named owner/carrier cannot express the accepted rule without a new responsibility; Specification if a new business admission or CPU policy is needed. Routine prose and carrier repairs remain with Implementation.
