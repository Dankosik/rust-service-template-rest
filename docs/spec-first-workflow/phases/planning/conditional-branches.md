# Conditional Planning Branches

Load only when one of these two execution shapes applies.

## Integration-first slice

When an unverified integration seam is the primary risk (or flagged in
`design/design.md` `Planning hooks`), plan the smallest production-real slice
(`Shape: integration-first`) that retires that uncertainty first. For several
independently falsifiable seams, plan separate slices plus an integrated check
in `tasks.md`'s `Completion`.

- Require real production wiring through the seam (fixtures or doubles belong
  only behind an accepted seam; stubs, TODOs, or test-only wiring fail). State
  the observable seam behavior `T<id>` must exercise locally (plus any accepted
  source-mandated check in `Additional proof`) rather than deferring
  verification to `unverified`, leaving concrete probe and fixture choices to
  Implementation so landing `T<id>` genuinely unlocks dependent slices.
- **Gate risk-dependent slices:** add coding edge `T<id>` in `tasks.md`
  (`T<id> — retires <assumption>` in the packet) on every task whose work would
  be discarded if the seam falsifies; keep genuinely independent tasks
  unblocked.
- Record the exact assumption and upstream reopen owner (`Technical Design` or
  `Specification`) in `Reopen if` so Implementation stops and reopens on a
  falsified seam.

## Wide refactor (expand–migrate–contract)

When one mechanical schema, type, or symbol change has a **blast radius** too
wide for one bounded task to land green atomically (or `Planning hooks` flags
it), sequence `expand -> migrate -> contract`:

1. **Expand (`Shape: expand`):** add the compatible new form beside the old so
   existing callers compile and stay green untouched.
2. **Migrate (`Shape: migrate`):** move caller batches sized by blast radius
   (per crate or module boundary) with disjoint `Mutable owners` and `Exclusive
   locks` so batches run in parallel; each batch depends on `Expand`.
3. **Contract (`Shape: contract`):** remove the old form and temporary shim
   once every consumer has moved, blocked on **every** `Migrate` batch.

- **Preserve `rollout.md` deploy boundaries:** when rolling mixed-version
  deploys separate `Expand` and `Contract` across releases, keep post-deploy
  `Contract` outside Release 1's ledger (record it in `Boundary -> Out of
  scope` referencing `rollout.md`, landing it in a follow-on release ledger
  after Release 1 deploys rather than placing a second-deploy code task in the
  same ledger).
- For `Expand`, `Migrate`, and `Contract` `Outcome` vs assembled `Observable
  claim` rules, follow [Task Packet
  V1](../../interfaces/task-packet-v1.md#field-rules).

Keep any refactor that stays valid and provable inside one Lead session as a
single atomic task.
