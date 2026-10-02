# Definition transition

status: ready

owner: Definition

result: [Intent](intent.md), [behavior spec](spec.md), [supporting research](research/baseline-and-libraries.md). Ready spec SHA-256: `052fcdfd88df8b6c7d730cbc9442c61175ae49dd99d9ecd4a519b3be94e5f85b`.

review: [Fresh Specification review](definition-review.md), PASS with no surviving findings. Changing draft to ready after review changed no semantic scope.

movement_evidence: Requester meaning, failure suppression, expiry, cache targets, refresh ownership and documentation deltas are closed. Existing library-absence claims have current counter-evidence. No runtime code was changed. `make docs-check` passed with zero errors on the three reviewed artifacts and repository docs; `git diff --check` passed. No runtime, Keycloak or CI result is claimed.

reopen_owner: Definition for changed observable behavior/compatibility; supporting Research for version or dependency evidence drift.

next_owner: Technical Design, to select the library mechanism and close runtime ownership, bounded lifecycle completion, cache accounting and file responsibility before Planning.

## Required design input

Join/observed completion is inherited from CONTRIBUTING and rust-tokio and was explicitly reviewed. Cancellation-only proof, an abandoned task handle or API preservation cannot waive it. Identify the narrow compliant completion mechanism; reopen Definition if it cannot fit the accepted compatibility boundary. The spec's absence of a mandated new public shutdown method does not prohibit necessary owned completion.

Fairly compare retained code, oauth2 and Huskarl using the research, including Huskarl's supported optional crypto/HTTP seams. Do not treat library presence as a migration mandate or optional native crypto as unavoidable. DPoP deployment, provider/platform migration, merge and deployment remain outside scope.

The root remains continuation and PR owner. Authorized next action is a fresh Technical Design actor; do not seek technical approval already delegated to the agent. This actor stops at reviewed Definition completion.
