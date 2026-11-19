# Conditional Planning Branches

Load only when one of these two execution shapes applies.

## Integration-first slice

When an unverified integration seam is the primary risk (or flagged in
`design/`), plan the smallest production-real slice that retires that single
uncertainty first. When several seams can be falsified independently, plan
separate slices followed by an integrated proof in `tasks.md`'s `Completion`.
Require real production wiring through the seam (scaffolding, TODOs, mock
success, and test-only wiring do not satisfy the slice; fixtures or test
doubles support proof only behind an accepted seam). Record the exact
unverified integration assumption and its upstream reopen owner (`Technical
Design` or `Specification`) in `Reopen if` so Implementation stops and reopens
rather than hacking around a falsified seam. Keep local or already-proven work
on the standard vertical-slice path.

## Wide refactor (expand–migrate–contract)

When a **wide refactor**—one mechanical schema, type, or symbol change whose
**blast radius** spans too many callers for one bounded slice to land green
atomically—is required, plan `expand -> migrate -> contract`:

1. **Expand:** add the compatible new form beside the old so existing callers
   stay compiling and valid untouched.
2. **Migrate:** move caller batches sized by blast radius (for example, per
   crate or module boundary) with disjoint `Mutable owners` and `Exclusive
   locks` so batches run in parallel while both forms work. Each batch depends
   on `Expand`.
3. **Contract:** remove both the old form and any temporary compatibility shim
   once every consumer has moved, blocked on **every** `Migrate` batch in the
   same ledger.

Use one atomic task whenever the refactor can stay valid and provable in one
Lead session; never add compatibility machinery solely to split work.
