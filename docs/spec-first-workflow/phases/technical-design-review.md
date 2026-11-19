# Technical Design Review

Once all applicable System and Rust Ownership design decisions are ready, the
Technical Design owner fixes their combined result for independent
[Review](../shared/review.md), even when only one branch applied. The phase
owner hands the reviewed result to Planning under [Transition](../shared/transition.md).
Use the same lenses and threshold for a requested standalone design review.

## Lenses

Start from the accepted Specification and affected canonical contracts.
Independently identify material obligations added, changed, removed, or
deliberately preserved, including those of existing consumers. For each
obligation, reconstruct its trace through the proposed design:

`behavior/drivers -> viable same-level alternatives or no-fork evidence ->
selected mechanism -> material flow/finality -> contract/truth -> responsible
owners -> required input -> observable result and proving surface`.

Anchor a missing trace at its source obligation and a broken trace at its first
unsupported or incompatible edge. Continue the unaffected traces. Trace each
included component, edge, store, or dependency back to the obligation or
constraint that justifies it. Where flows interact, independently try to break
their shared guarantee using [Material Flow](system-integration-design.md#material-flow).

Activate only lenses exposed by current evidence: flow and authority,
current contract/runtime contradiction, comparison of viable mechanisms under
the same scenario and accepted drivers, required-input availability, Rust
ownership or panel-receipt compatibility, performance/scale including the
stated unit cost and its bound, operator signal for each failure, degradation,
or recovery path, proof feasibility, and release closure.

Credit current [Rust Ownership Review](rust-code-ownership-design.md#rust-ownership-review)
receipts for the lenses they cover on this candidate. The panel can satisfy
the phase review under [Review](../shared/review.md#route) when its coverage
spans the whole candidate and all triggered lenses. Otherwise review the
remaining lenses and their fit with the panel findings.

`FAIL` if a material obligation lacks a supported trace, a triggered lens
exposes a design defect, or an unresolved question could change behavior,
mechanism, contract, ownership, or how Planning must divide the work. Reopen
Specification for behavior, System or Rust Ownership Design for a technical
decision, Research for evidence, or the authority that owns a required input.

`PASS` requires support for every material obligation and triggered lens.
Implementation owns routine coding choices. Permission needed only to deploy
belongs at deployment. Use `CONCERNS` only for bounded later proof or risk
over an otherwise closed design. Name its owner and the action that needs it.
Both `PASS` and `CONCERNS` permit handoff when Planning has its required
inputs and authority.
