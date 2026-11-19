# Planning

Use when structured work needs a ready task ledger for implementation. Own task
slicing, dependency ordering, writable boundaries, and final validation claims
from accepted inputs. Preserve upstream behavior and architecture; leave test
cases, fixtures, proving layers, and exact test commands to Implementation.

## Steps

1. **Reconcile inputs and check delta.** Read all accepted artifacts
   (`intent.md`, `spec.md`, `design/`, `rollout.md`, `research/*.md`, and
   `workflow-plan.md` when present). Map every material obligation—including
   limits, exceptions, and preserved consumer guarantees—to a task delta,
   current evidence proving it is already met, or an authorized scope exit.
   - Missing required product, mechanism, or ownership decision: stop and return
     [Transition Result V1](../shared/transition.md#transition-result-v1) with
     `status: blocked` and `reopen_owner` naming the smallest upstream owner.
   - No implementation delta remains: create no ledger and submit the supporting
     evidence (naming owners and stages of any outstanding proof or external
     effects) directly to step 5.

2. **Design the task graph before expanding packets.** Slice under [Task Shapes
   And Granularity](#task-shapes-and-granularity). Load [Conditional Planning
   Branches](planning/conditional-branches.md) when either (a) an unverified
   integration seam is the primary risk (**integration-first slice**), or (b) a
   **wide refactor** with broad **blast radius** cannot land green atomically
   (**expand–migrate–contract**). Place any required external action at its
   consuming stage with its owner and success condition (a change to release
   tooling can be its own task; approval to publish gates publication).
   Complete this step only when:
   - every accepted obligation traces to at least one task;
   - every task's `Provides` feeds a downstream task or `Completion` (merge or
     delete any task that fails this deletion test);
   - every task fits one valid shape and one fresh Lead session;
   - all tasks reach `Implemented` before final validation (acyclic; no coding
     edge depends on `Accepted` or a post-implementation gate).

3. **Persist draft ledger and packets.** Write `specs/<task>/tasks.md` at
   `status: draft` using [Task Ledger V1](../interfaces/task-ledger-v1.md) (even
   for one task), migrating active execution instructions from `workflow-plan.md`
   into `Global constraints`. Write each `specs/<task>/tasks/<ID>-<slug>.md`
   using [Task Packet V1](../interfaces/task-packet-v1.md). Record a predecessor
   only when its output or an accepted sequencing constraint is required,
   attaching each dependency to its earliest consuming stage with identical IDs
   and stage tags in both files.

4. **Run the Readiness Dry Run (static — run no builds, tests, or services).**
   Verify **every task** against all four checks before review:
   - **Completion coverage:** try to satisfy every `Outcome` and `Observable
     claim` while breaking an accepted obligation; close any gap.
   - **Source truth (all tasks):** read current code and—for external crates—the
     source of the version locked in `Cargo.lock` to confirm the existence and
     semantics of every consumed symbol, type, trait, config key, or crate API
     (reusing shared checks). Record verified non-obvious anchors in each
     packet's `Sources`. If a locked crate source is uncached locally, flag the
     unverified assumption in `Reopen if` instead of relying on memory; if
     source contradicts planned use, repair the plan or reopen upstream.
   - **Cold-start & base-commit falsifiability:** walk every task from
     `tasks.md`, its packet, cited `Sources`, and current code alone (zero chat
     history). Confirm a fresh Lead needs no guesswork on behavior, mechanism,
     or placement, and every `Outcome` and `Observable claim` fails on the base
     commit and distinguishes accepted behavior from a plausible incomplete
     change (or satisfies the removal/prefactor rule in Task Packet V1).
   - **Ready Frontier & write-lock disjointness:** identify every individually
     ready task (coding prerequisites met, including both tasks when two
     otherwise ready tasks share a free lock; dispatch chooses priority) and
     every pair of tasks able to run concurrently. Confirm their `Mutable
     owners` and `Exclusive locks` are either genuinely disjoint or declare the
     shared lock so dispatch serializes them; add missing locks or sequence
     colliding tasks.

5. **Submit for independent review.** Route the fixed `draft` ledger (or
   no-implementation evidence) to a fresh reviewer running [Task Review /
   Readiness](task-review-readiness.md) under [Review](../shared/review.md).
   Repair plan-local `FAIL` findings in place; return `status: blocked` with
   `reopen_owner` in [Transition Result
   V1](../shared/transition.md#transition-result-v1) for upstream gaps or source
   contradictions.

6. **Finalize and hand off.** After review returns `PASS` or `CONCERNS`:
   - persist any bounded `CONCERNS` risk into `Global constraints` or the
     affected packet (`Reopen if` / `Final validation`) so the fresh
     Implementation session receives it;
   - set `status: ready` in `tasks.md` and delete `workflow-plan.md` when
     present ([Artifacts](../shared/artifacts.md));
   - return [Transition Result V1](../shared/transition.md#transition-result-v1)
     (`status: ready`, `result: specs/<task>/tasks.md`, `next_owner:
     Implementation`) for [Implementation
     Handoff](../shared/transition.md#implementation-handoff). Stop here; never
     start implementation inside Planning.

## Task Shapes And Granularity

- **Smallest valid ledger first.** Default to one cohesive task when the change
  fits one fresh Lead session without mixing independent outcomes or lock
  contention. Split only to isolate independent outcomes or external gates, fit
  one context window, unlock parallel disjoint scopes, or extract a prerequisite
  below.
- **Four valid task shapes** (horizontal layer splits like `types -> repo ->
  handler -> tests` for one feature are invalid):
  - **Vertical slice (tracer bullet):** default—a narrow, complete path through
    all touched layers that compiles, stays valid, and delivers an observable
    behavior alone. Litmus test: *"Assume its prerequisites are met and all
    remaining work is deferred: who can use this outcome now, and what
    observable behavior can they verify end-to-end?"*
  - **Prefactoring / Shared seam:** design-authorized behavior-preserving
    refactor or shared Rust seam (types, traits, signatures, schema/migration)
    ordered first *only* when it stays compiling/valid alone and either unlocks
    ≥2 parallel downstream slices or materially simplifies the next slice ("make
    the change easy, then make the easy change").
  - **Integration-first slice** and **Expand–migrate–contract**: see
    [Conditional Planning Branches](planning/conditional-branches.md).
- **Task vs lane:** parallel work inside one outcome belongs to Lead-chosen
  lanes, not separate ledger tasks.
