---
name: idea-refine
description: "Turn a raw or solution-led idea into one concrete problem, a smallest falsifiable MVP bet, and clear exclusions. Stops at a converged direction ready for Intake."
metadata:
  invocation: user
  kind: workflow
disable-model-invocation: true
---

# Idea Refine

Converge a raw, vague, or solution-led pitch into one testable product bet
before committing to specification or architecture.

## Steps

1. **Pin one real problem and one primary operator.**
   - Strip away any pitched feature, tool, or vendor solution to one concrete
     moment where a specific user or operator is blocked from deciding or
     acting, and name the real cost of that block.
   - Select the **single primary operator** whose success defines MVP
     correctness. If the situation or primary operator is unclear, ask 1–2
     opening questions through
     [Grilling](../../../docs/spec-first-workflow/shared/grilling.md) before
     proposing directions (and verify existing behavior in code/docs if the
     idea targets this repository).

2. **Compare 2–3 genuinely distinct directions and cut to the smallest MVP bet.**
   - Contrast `2–3` meaningfully different ways to solve the problem—not
     cosmetic variants of the same feature—and converge on the direction that
     tests the riskiest value assumption with the least irreversible scope.
   - Define **success** as observable behavior, outcome, or risk reduction
     inside the team's control (for prevention work, measure risk shrinking
     rather than fallback usage; mark unbacked numeric targets explicitly as
     `discovery placeholder`).
   - Define **MVP** as the smallest slice proving the core bet, and list
     concrete excluded functionality, audiences, integrations, and guarantees
     under **`Not Doing`**.
   - Write each load-bearing assumption as a falsifiable `"We believe ..."` bet
     paired with an observable **kill, shrink, or pivot trigger** (label
     anecdote as weak evidence). Cover ordinary missing detail with labeled
     assumptions; resolve direction-changing or high-impact forks (policy,
     money, identity, privacy, safety, or destructive action) using
     [Grilling](../../../docs/spec-first-workflow/shared/grilling.md).

3. **Converge and stop at the handoff gate.**
   - Present a compact convergence brief: selected direction, eliminated
     alternatives with evidence, `MVP` + `Not Doing`, success signal,
     falsifiable bets with triggers, and labeled assumptions—or surface the
     single open decision blocking convergence.
   - Leave architecture, schema, API, and task breakdowns to downstream phases.
   - Wait for explicit user confirmation before entering
     [Intake](../../../docs/spec-first-workflow/phases/intake.md) (carrying
     converged decisions forward without re-asking them).
