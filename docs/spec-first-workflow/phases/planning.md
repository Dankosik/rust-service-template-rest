# Planning

Prepare executable slices from accepted decisions. Work statically against
primary sources without running builds, tests, or services. Return open product
or architecture decisions to their upstream owner.

## Steps

1. **Reconcile inputs and remaining work.** Read the task's accepted intent,
   specification (or cited fixed external contract for skipped Definition),
   and applicable design and rollout decisions. When Technical Design ran
   without a design document, consume its reviewed no-open-decisions result.
   Check `status: ready` on present intent, spec, design and rollout documents.
   Read applicable research, active `workflow-plan.md` rules and existing
   ledger state. Route missing required authority through [Blocked Exit](#blocked-exit).

   On reopen, leave the ledger and packets intact during analysis. Preserve
   unaffected IDs and integrated work against its original `Base`.

   Map each accepted obligation once:

   | Input | Disposition |
   | --- | --- |
   | Intent decisions (`I<n>`, binding or rejected), hard constraints, exclusions and priorities | Preserve in slice boundaries, global constraints or completion criteria. |
   | Spec requirements, guarantees (`R<n>`, `G<n>`) and limits | Cite the source in the owning task and express its delta or preserved guarantee in the claim or assembled completion target. |
   | Spec downstream handoff and durable contract updates | Apply Planning constraints to slicing or scope. Include required updates in the landing slice. |
   | Design decisions and seams (`D<n>`, `S<n>`), forced owners, verified anchors, shared identifiers and mutated surfaces | Assign sources, mutable owners and locks. Include superseded paths, generated contracts and durable doc updates in the landing slice. Apply Planning hooks or give the obligations reviewer the reason for declining one. |
   | Rollout gates and deployment boundaries | Gate the consuming stage and keep later-release work outside the ledger. |
   | Carried concerns | Close Planning-owned concerns with a locator. Carry Implementation and Delivery Owner risks, owners and closing actions in global constraints or the affected packet's `Reopen if`. |
   | Upstream assumptions, decision reopen conditions and active workflow rules | Record cross-task rules in global constraints and task-specific triggers in `Reopen if`. |

   For an individual `R<n>`, `D<n>` or `S<n>` already satisfied on the
   baseline, record `<ID>: already holds (<primary locator>)` in global constraints.

   When no coding delta remains, take [No-Implementation Exit](#no-implementation-exit)
   if all integrated work has been accepted. If acceptance remains, keep the
   integrated tasks, packets and receipts and resume at Step 4 for Final Validation.
   For remaining coding work, continue with Step 2.

2. **Ground the proposed changes.** Inspect touched symbols, trait bounds,
   error paths, superseded code and tests, and the [packet's lock
   surfaces](../interfaces/task-packet-v1.md#field-rules). Locate production
   and test callers and match sites across all workspace targets for changed
   signatures and exhaustive public types. Verify non-obvious, unanchored or
   slicing-critical crate APIs against locked sources (offline metadata or
   cached paths). Record an unavailable source in `Reopen if` only if the
   task boundary and producer contract are settled. Delegate large multi-crate
   lookups under [Read-Only Delegation](../shared/read-only-delegation.md)
   and verify decision-critical anchors directly.

3. **Choose the task graph.** Apply [Task Shapes And Granularity](#task-shapes-and-granularity).
   Keep architecture, profile and manifest updates atomic with their slice.
   Cover every affected caller in the changing task or in a compatible staged
   migration. For a single task spanning three or more crates, mixed gates or
   an unverified seam, compare a split against the work one fresh Lead can
   hold. For multiple tasks, try merging coupled pairs and pulling forward
   risk retirement or a prefactor that simplifies two or more dependents.
   Remove a task if its deletion loses no accepted outcome or prerequisite.

4. **Write the draft.** Set `status: draft` in the [ledger](../interfaces/task-ledger-v1.md)
   and write its [packets](../interfaces/task-packet-v1.md), including for one task.
   Clear any resolved blocked stop, remove superseded orphan packets and
   discard runtime fields on a fresh draft with no integrated work.

5. **Check the candidate.** Run `make ledger-check TASK=<task>` and repair
   mechanical errors. Audit each producer using only its packet and sources
   against every contract its dependents assume. Check pairs with no coding
   path between them for undeclared owner, lock and exhaustive-type collisions.
   Locate the baseline witness for each local outcome, assembled claim and
   completion target under the packet's shape rules. Use the retained baseline
   for integrated tasks. Resolve Planning defects in place and route upstream
   contradictions through [Blocked Exit](#blocked-exit).

6. **Review readiness.** Dispatch the [readiness review](task-review-readiness.md)
   for the fixed draft. Apply the shared [Review Lifecycle](../shared/review.md#lifecycle)
   to findings and disputes. For confirmed Planning defects, repeat Steps 4–5
   on the repair. Recheck both axes when changed edges, locks or sources affect
   isolation or when changed slices, coverage or claims affect obligations.
   Reslicing requires fresh review. Send confirmed upstream gaps to
   [Blocked Exit](#blocked-exit) with the review result.

7. **Hand off.** Once all assigned axes clear, record bounded concerns in their
   Step 1 homes. Mark the ledger ready and retire pre-ledger coordination under
   [Artifacts](../shared/artifacts.md) before rerunning `make ledger-check TASK=<task>`.
   Return the ready ledger in [Transition Result V1](../shared/transition.md#transition-result-v1)
   with Implementation as next owner. Include checked execution waves, lock
   serializations, carried concerns and active reopen risks in movement evidence.
   End preparation at [Implementation Handoff](../shared/transition.md#implementation-handoff).

## Task Shapes And Granularity

Default to **1 task** whenever one fresh Lead holds the change in its **smart
zone** (one coherent outcome, bounded crate set, uniform risk/gate boundary).

- **Split** only for: (1) independent observable outcomes compiling and
  verifying alone; (2) genuine parallelism across disjoint `Mutable owners`,
  `Exclusive locks`, and exhaustive-type updates; (3) isolating an unverified
  integration seam or external gate; (4) smart-zone or blast-radius overload
  across independent subsystems; or (5) a prerequisite shape below.
- **Merge** tasks of one outcome sharing an `Exclusive lock` or `Mutable owner`
  unless risk/gate isolation (3), smart-zone overload (4), or a valid
  prerequisite shape (5) applies; tasks split solely for concurrency (2) that
  serialize on a shared lock or owner; horizontal layers (`types -> repo ->
  handler -> tests`); or standalone cleanup/docs tails (fold superseded cleanup
  and durable doc updates into the landing slice, except `Contract`).
- **Task vs lane:** parallel subtasks inside one outcome belong to Lead-chosen
  lanes, not separate ledger tasks.

| Shape | Tag | When to use | Guardrail |
| --- | --- | --- | --- |
| **Vertical slice (tracer bullet)** | *(default; omit)* | Narrow, complete path through touched layers delivering observable behavior (or a behavior-preserving/quantitative delta) at its seam alone. | Litmus test: *"With prerequisites met and other tasks deferred, what works or is provably changed at the seam now?"* |
| **Prefactor** | `[prefactor]` | Behavior-preserving cleanup of *existing* code ordered first to simplify the next slice(s). | Compiles alone; zero new feature types, schemas, or behavior. |
| **Shared contract** | `[shared-contract]` | Minimal compiling contract/schema (types, traits, proto, CLI/Make contract, migration) ordered first. | Valid **only** when `≥2` lock-disjoint downstream slices run in parallel after it lands; otherwise fold into the first vertical slice. |
| **Integration-first** / **Wide refactor** | `[integration-first]` / `[expand]`, `[migrate]`, `[contract]` | Unverified external seam is the primary risk, blast radius prevents one atomic green task, or `design.md -> Planning hooks` flags either. | Load [Conditional Planning Branches](planning/conditional-branches.md) only when triggered. |

## Blocked Exit

Use when a required upstream input is missing, unready or contradicted, or a
Planning concern or concurrent identifier needs an upstream decision.

- If `specs/<task>/tasks.md` exists and clean slices or already-integrated
  `[x]` tasks (`Base:` runtime state) survive the blocker, keep `tasks.md` at
  `status: blocked` with `Blocked stop:` naming the missing input and owner,
  and set `result: specs/<task>/tasks.md`.
- Otherwise delete any obsolete `tasks.md` and `specs/<task>/tasks/*.md` and
  carry collected blockers inline in `result:`.
- Emit [Transition Result V1](../shared/transition.md#transition-result-v1)
  (`status: blocked`, `owner: Planning`, `reopen_owner: <smallest upstream
  owner>`).

## No-Implementation Exit

Keep existing execution artifacts until the [no-implementation
review](task-review-readiness.md#no-implementation-result) clears. Submit the
accepted obligations and primary evidence, naming owners and stages for any
outstanding proof or external effects.

Handle disputed findings under the [Review Lifecycle](../shared/review.md#lifecycle).
If implementation work remains, resume at Step 2. Send confirmed upstream gaps
to [Blocked Exit](#blocked-exit) with the review result. Once cleared, remove
the obsolete ledger and packets, retire coordination under
[Artifacts](../shared/artifacts.md), and return a ready Planning transition
with the verified zero-delta evidence and the named external next owner or
`none`.
