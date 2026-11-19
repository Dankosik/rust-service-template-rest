# Conditional Planning Branches

Load only when one of these two execution shapes applies.

## Integration-first slice

When an unverified integration seam is the primary risk, plan the smallest
production-real slice that retires that single uncertainty first. When several
seams can be falsified independently, plan separate slices followed by an
integrated proof at Completion. Scaffolding, TODOs, mock success, and test-only
wiring do not satisfy the task; fixtures or test doubles may support proof only
behind an accepted seam. Keep local or already-proven work on the standard
vertical-slice path.

## Wide refactor (expand–migrate–contract)

When a **wide refactor**—one mechanical schema, type, or symbol change whose
**blast radius** spans too many callers for one bounded slice to land green—is
required, plan `expand -> migrate -> contract`:

1. **Expand:** add the compatible new form beside the old so existing callers
   stay valid and green.
2. **Migrate:** move caller batches sized by blast radius (for example, per
   crate or module boundary) with disjoint write scopes so batches can run in
   parallel while both forms work. Each batch depends on Expand.
3. **Contract:** remove the old form once every consumer has moved, blocked on
   every Migrate batch in the same ledger.

Use one atomic task whenever the refactor can stay valid and provable; never add
compatibility machinery solely to split work.
