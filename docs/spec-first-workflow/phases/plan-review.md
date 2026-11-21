# Plan Review

Fresh, static, read-only review of the assembled preparation bundle
(`specs/<task>/spec.md`, optional `specs/<task>/design.md`, `tasks.md`, and
`tasks/*.md`) before Implementation starts. Return [Review
Result](../shared/review.md#review-result) under [Review
Lifecycle](../shared/review.md#lifecycle).

## Topology And Brief Hygiene

- **2-axis parallel panel** when the bundle has `≥3` tasks, a parallel
  frontier across `≥3` mutated crates, or a wide refactor (`expand–migrate–contract`).
- **Single reviewer** for all other plans: evaluate Axis 2's cold-start checks
  using `tasks.md`, packets, cited anchors, and live code before reading
  uncited `research/*.md` or narrative context.
- Every reviewer runs with a **fresh context window** (no parent chat history
  or author rationale) and inspects primary code statically (no builds, tests,
  or live services). Every assigned axis must clear independently on the same
  fixed bundle; return `REVIEW_HANDOFF_INVALID` if required bundle files or
  linked packets are missing or unreadable.
- On a bounded **Delta Recheck** after local plan repairs, inspect only the
  edited clauses, refutations, and directly dependent tasks or anchors;
  escalate to a full review whenever repairs resliced tasks or changed
  interfaces.

## Checks

Report under explicit check headings, recording inspected primary files and
symbols in `evidence_boundary`. On a clearing check write `none — attacked
<strongest loophole, collision, or anchor tested>; closed by <clause or
locator>`. On a failing check give anchor, concrete witness, classification
(`PHASE_DEFECT` or `UPSTREAM_GAP`), and smallest owner.

### Axis 1 — Obligation Parity, Slicing & Unreviewed Inline Scope *(receives `spec.md`, optional `design.md` and `intent.md`, `tasks.md`, packets, `research/*.md`)*

1. **Obligation parity & design drift:** trace every requirement (`R<n>`),
   preserved guarantee (`G<n>`), `Must not` prohibition, non-goal, decision
   (`D<n>`), seam (`S<n>`), rollout gate, durable doc update, and carried
   `CONCERNS` / `[integration-first]` assumption into task packets and
   `Completion`.
   - `design.md` present without both `status: ready` and `Design review:
     PASS | CONCERNS` (including `status: draft`, `Design review: pending`, or
     missing receipt): immediate `FAIL` (`PHASE_DEFECT`, owner: [Technical
     Design](technical-design.md))—`design.md` must clear [Design
     Review](design-review.md) before Planning completes.
   - `design.md` present at `status: ready` with `Design review: PASS |
     CONCERNS`: verify zero drift between `spec.md`/`design.md` and task
     packets without re-litigating settled architecture; `FAIL` if packets
     silently alter a `D<n>`, crossing, `S<n>`, or rollout gate without
     downgrading `design.md` to `status: draft` (`Design review: pending`) and
     re-clearing Design Review.
   - No `design.md` (`spec.md` at `status: draft` with inline `##
     Architecture`): **verify none of the [Design Review
     Gate](technical-design.md#design-review-gate) triggers hold** (`FAIL`
     routing back to [Technical Design](technical-design.md) if any slipped
     through). Otherwise verify behavioral rules against [Behavioral
     Probes](specification/behavioral-probes.md), verify autonomous choices
     under user-delegated boundaries in `spec.md` stay within stated limits
     rather than flagging them as `UPSTREAM_GAP`, apply the deletion test to
     inline architecture, and check `quality/architecture.json`, `Cargo.lock`,
     and `docs/decisions/` against live code.
2. **Slicing & falsifiable claims:** attack the task cut under [Task Shapes And
   Granularity](planning.md#task-shapes-and-granularity) (delete redundant
   tasks, merge lock-serialized slices, catch horizontal layers). Verify every
   task's `Outcome`, shape-appropriate `Observable claim` (base-commit-failing
   public-seam witness for vertical slices, or the matching shape rule in
   [Planning](planning.md#ledger-and-packet-format) / [Conditional Claim
   Rules](planning/conditional-branches.md#claim-rules)), and `Additional
   proof` authority.

### Axis 2 — Cold-Start, DAG & Lock Isolation *(receives `tasks.md`, packets, cited anchors, and live code only)*

3. **DAG & concurrent collision audit:** verify topological order, acyclic
   edges, and parity between `tasks.md` and packets. Inspect every concurrently
   eligible pair for undeclared `Mutable owners`, [Exclusive
   locks](planning.md#exclusive-locks), or exhaustive public-type collisions.
4. **Callers, superseded paths & locked crates:** independently trace workspace
   callers and match sites across all targets (including tests), superseded
   code/tests, generated contracts (`api/`, `.sqlx/`), and non-obvious
   `Cargo.lock` sources. `FAIL` on callers broken at landing or missing compile
   prerequisites.
5. **Cold-start producer isolation:** walk each task with zero chat history.
   For every producer edge, attempt incompatible implementations from the
   producer packet and its `Sources` alone; `FAIL` if parallel Leads must invent
   shared names, signatures, error codes, config keys, or edge behavior.

## Zero-Delta Check

For a zero-coding-delta result (no `tasks.md`), one fresh reviewer inspects the
confirmed requirements (from the brief, `intent.md`, or `spec.md`), claimed
primary-source locators, live code, and callers to verify that every accepted
obligation already holds (or has an authorized scope exit), no integrated work
awaits acceptance, and any remaining external effects name their owner and
stage. Return `PASS` or `FAIL` only.

## Verdict

- `PASS`: every assigned check clears with proof of attack and primary-source
  inspection. Repo-endorsed patterns, behind-seam implementation choices,
  equivalent valid task cuts, and speculative out-of-scope scenarios never
  block `PASS`.
- `CONCERNS`: strictly a bounded downstream proof or external risk (e.g.,
  uncached crate with settled producer contract) over an otherwise closed plan,
  naming risk, owner, and closing action.
- `FAIL`: any surviving witnessed `PHASE_DEFECT` (repair locally) or
  `UPSTREAM_GAP` (reopen the user strictly for an actual product/risk
  contradiction or missing external authority).
