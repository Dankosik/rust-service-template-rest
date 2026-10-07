# Delivered laboratory replay inputs

These profile-owned inputs keep the opt-in command runnable in a fresh checkout
without task-local `specs/` artifacts. `manifest.json` binds each patch and its
P0 observer source to SHA-256. The referenced runtime baseline is historical;
each actual execution independently records the current committed Git head/tree,
complete source inventory and executable hashes before its target effect.
P0 is the comparison baseline. Apply each other patch independently to its
source copy, never cumulatively.

| Source | Admission | Complete batch | Pacing | Startup | Cadence |
| --- | --- | --- | --- | --- | --- |
| P0 | Unbounded | Existing provider limits | None | Immediate | Existing Delay interval |
| P1 | 5 seconds | 12 seconds | None | Immediate | 60 seconds after termination |
| P2 | P1 | P1 | 10 ms between full committed batches | Immediate | P1 |
| P3 | P1 | P1 | P2 | One family-local 0–30,000 ms spread | P1 |

All variants retain the same observations, batch SQL, transaction ownership,
retention predicates and native workload. Only the five private constants in
`crates/infra-postgres/src/observe.rs` vary. They are temporary source replay
machinery, with no environment or public API selecting a policy. After the
accepted matrix chooses a policy, the delivery owner removes every unused
runtime branch and reruns affected final proof.

If the delivered source has already removed experimental paths,
`restore_baseline_patch` identifies the minimal patch restoring this fixed
laboratory baseline inside the private build copy. The entry reverses it before
verifying that the copy matches the delivered source again. It is never applied
to the live checkout or exposed as production configuration. A null value means
the delivered source still contains the pre-selection baseline mechanisms.

`foundation-instrumentation.patch` is solely the diagnostic observation-cost
comparison. It reverses the P0 production delta to the named foundation while
retaining the native measurement target and the integrated queue dependency.
Apply it only to the calibration source copy, with the harness's foundation
instrumentation manifest. It cannot satisfy main-cell observation criteria.

Before execution, regenerate these inputs if any bound source changed, record
the exact accepted Q commit and integrated P source/tree identity, then build
all release executables serially. Verify patch/source/executable hashes before
the first target effect. These authored hashes are not execution receipts.
