# Task Review / Readiness

Independently review one fixed Planning result under [Review](../shared/review.md).
Use static primary evidence without builds, tests or services. Return
[Review Result V1](../interfaces/review-result-v1.md). For a result without a
ledger, use [No-Implementation Result](#no-implementation-result).

## Axes *(Draft Ledger)*

Planning assigns two fresh reviewers with no parent history to parallel axes
when the ledger has three or more tasks, three or more mutated crates, a wide
refactor or a rollout document. Otherwise one fresh `Full` reviewer covers both
axes. Each reviewer runs only the assigned checks.

Planning gives each reviewer its mode, method, and fixed ledger and packets.
Only the Axis 1 or Full brief includes upstream inputs, declined-hook reasons
and Planning concern closure locators. Axis 2 reconstructs the plan
from candidate files, cited sources and primary code. In Full mode apply that
same evidence boundary to the Axis 2 checks.

## Adversarial Falsifiers

Record a concrete attempted witness for each assigned numbered check in
`evidence_boundary`, including when it clears. Continue on independent paths
after a defect. In Full mode a slicing failure can short-circuit cold-start
detail on malformed packets. Identify the affected checks and continue looking
for upstream gaps and source contradictions.

**Axis 1 — obligations, slicing and falsifiability**

**1.** Independently reconstruct the [obligation map](planning.md#steps). Try to
   satisfy every packet while dropping an obligation, a Planning hook or its
   disposition, or an assembled completion condition. Challenge scope creep,
   inaccurate source anchors and each claim that an obligation already holds.
**2.** Attack the proposed cut under [Task Shapes And Granularity](planning.md#task-shapes-and-granularity).
   Try deleting a redundant task, merging serialized slices or splitting an
   overloaded one. For a conditional shape, challenge its risk or release
   boundary under [Conditional Planning Branches](planning/conditional-branches.md).
**6.** Locate a baseline witness per task and challenge its local outcome,
   assembled claim and overall completion under the [packet field
   rules](../interfaces/task-packet-v1.md#field-rules). Try a stub or partial
   assembly. Distinguish prerequisites from unavailable sibling work, respect
   shape-specific exceptions, and use the original baseline for integrated
   tasks. Check that additional proof has an accepted source.

**Axis 2 — DAG, source truth, locks and cold-start isolation**

**3.** Reconstruct coding edges under [Task Ledger V1](../interfaces/task-ledger-v1.md).
   Inspect every concurrently eligible pair for undeclared overlapping owners,
   lock surfaces or exhaustive-type changes. Record the pairs, surfaces and
   checker result.
**4.** Independently trace workspace callers and matches across all targets,
   including tests, and inspect superseded paths and locked crate sources
   under [Planning Step 2](planning.md#steps). Record symbols, searches and
   unavailable sources. Look for callers broken at landing, omitted
   architecture/profile changes or consumers missing a compile prerequisite.
**5.** Walk each task with zero chat history. For every producer edge, attempt
   incompatible implementations from the producer packet and its sources
   alone. Check the names, placement, contracts and edge behavior required by
   dependents.

## Delta Recheck

Supply anchored findings and the repair delta or refuting primary locators.
Recheck affected packets, edges, checker results, locks and producer contracts
under the [Review Lifecycle](../shared/review.md#lifecycle).

## No-Implementation Result

Planning assigns one fresh reviewer. Independently verify that all accepted obligations
already hold or have an authorized scope exit, no integrated task awaits
acceptance, and outstanding external effects or proof have named owners and
stages. Return `PASS` with verified primary evidence, or `FAIL` naming the
remaining Planning defect or smallest upstream gap.

## Blocking Bar And Verdict *(Draft Ledger)*

Classify findings under [Review Result V1](../interfaces/review-result-v1.md).
Anchor a blocker to its task, obligation or primary source and give the concrete
failure scenario. For a bounded concern name the affected task and its Step 1
home. An uncached crate can be a concern when the producer contract is settled.
Routine implementation choices, optional proof, semantic owners without file
lists and equivalent valid task cuts permit readiness. Each assigned axis must
clear independently with `PASS` or `CONCERNS`, subject to the shared review
lifecycle.
