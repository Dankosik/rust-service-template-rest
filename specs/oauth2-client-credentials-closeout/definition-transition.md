# Definition transition

status: ready

owner: Definition

result: [Intent](intent.md), [behavior spec](spec.md), and unchanged [supporting research](research/baseline-and-libraries.md). Ready intent SHA-256: `f9ba40ce215cd16c1c5b2baf26957093bade7831fce8803f281a8c6d3f8d2602`. Ready spec SHA-256: `bc78c10a723502a01bc40d91db174910e6fed6a9e18f1cc39e9bd1b862af3236`.

review: [Fresh bounded lifecycle/compatibility review](definition-review.md), PASS with no surviving findings. Unchanged behavior retains the initial reviewed scope from `e9e57b3a11b2fcab75b8533472ecee4d453c84e6`. Draft-to-ready status changes after review are mechanical only.

movement_evidence: Definition now explicitly permits the necessary bounded source-level credential construction/composition change to provide a driven production completion owner. Last external-owner release cancels refresh and permits observed completion. Dropping/completing the lifecycle owner closes surviving credentials: new calls return Timeout if their deadline already elapsed, otherwise existing Unavailable without token/resource dispatch, regardless of cache contents. Admitted calls may finish within their budgets and completed effects are not retroactively cancelled. These deltas have fresh independent PASS; other accepted behavior and authority remain unchanged. No runtime code changed or runtime/provider/CI proof is claimed.

reopen_owner: Definition if a new behavior or compatibility delta becomes necessary; Research for dependency-evidence drift.

next_owner: Technical Design, continuing its bounded lifecycle mechanism/API, ownership and library decisions before Planning.

## Required design input

Technical Design may change credential construction and integration composition to expose the necessary completion ownership. It owns the exact API, mechanism and placement; Definition does not mandate a particular driver or task arrangement. Preserve supported active-integration HTTP/gRPC semantics and update all affected examples/consumers in this PR. A public shutdown method, global registry, new readiness dependency or provider/platform migration is not required.

Cancellation-only proof, synchronous final Drop, an abandoned task handle or private detached reaper cannot satisfy observed completion. The supported production composition must drive the lifetime operation while credentials remain active and await completion within its existing teardown budget. Newly invoked calls on a closed owner fail locally as specified; unmanaged fallback is forbidden.

Retain the original fair library comparison, including optional signer/HTTP seams. DPoP deployment, merge and deployment remain outside scope. The root owns continuation and PR publication; no user technical approval remains necessary for the accepted bounded change.

## Static verification

Final `make docs-check` passed with 866 total links and zero errors; the amendment passed `git diff --check`. No runtime build, provider integration or CI was run by this Definition actor.
