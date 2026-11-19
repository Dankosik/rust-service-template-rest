# Conditional Planning Branches

Load only when one of these execution shapes applies (or is flagged in
`design/design.md` `Planning hooks`).

## Integration-first slice

When an unverified integration seam is the primary risk, plan the smallest
production-real slice (`Shape: integration-first`) that retires that
uncertainty first. For several independently falsifiable seams, plan separate
slices plus an integrated check in `tasks.md`'s `Completion`.

- Wire real production code through the seam (doubles belong only behind an
  accepted seam; stubs or test-only wiring fail). State the observable seam
  behavior `T<ID>` exercises locally instead of deferring it to `unverified`.
- **Gate risk-dependent slices:** add coding edge `T<ID>` (`T<ID> — retires
  <assumption>`) on every task whose work would be discarded if the seam
  falsifies; keep independent tasks unblocked.
- Record the exact assumption and upstream reopen owner (`Technical Design` or
  `Specification`) in `Reopen if`.

## Wide refactor (expand–migrate–contract)

When one mechanical schema, type, or symbol change has a **blast radius** too
wide for one atomic green task, sequence `expand -> migrate -> contract`:

1. **Expand (`Shape: expand`):** add the compatible new form beside the old so
   existing callers compile untouched.
2. **Migrate (`Shape: migrate`):** move caller batches sized by blast radius
   (per crate/module boundary) with disjoint `Mutable owners` and `Exclusive
   locks`, each depending on `Expand`.
3. **Contract (`Shape: contract`):** remove the old form and temporary shim
   once every consumer moved, blocked on **every** `Migrate` batch.

- **Preserve `rollout.md` deploy boundaries:** when mixed-version deploys
  separate `Expand` and `Contract` across releases, keep post-deploy `Contract`
  outside Release 1's ledger (record it in `Boundary -> Out of scope` citing
  `rollout.md`).

## Claim Rules

| Shape | `Outcome` (unit-local) | `Observable claim` (assembled) |
| --- | --- | --- |
| **Integration-first** (`[integration-first]`) | Production-real slice wired through the unverified seam retiring the assumption | Observable seam behavior failing on base commit (real wiring, no stubs) + paired `G<n>` |
| **Expand** (`[expand]`) | New form added beside old; existing callers compile green untouched | Later-release `contract`: new-form behavior + old-caller `G<n>`. Same-ledger `contract`: `superseded at assembly by T<contract>` |
| **Migrate** (`[migrate]`) | Batch's callers moved inside the batch's `Mutable owners` | Preserved `G<n>` + zero remaining old-form references inside `Mutable owners` |
| **Contract** (`[contract]`) | Old form and temporary shim deleted across compile/entry boundary | Old form absent/unreachable at compile or public seam + paired `G<n>` |
