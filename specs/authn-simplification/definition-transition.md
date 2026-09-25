# Definition transition

status: ready

owner: Definition (Intake, supporting Research, Specification)

result: [intent.md](intent.md), [spec.md](spec.md),
[research/synthesis.md](research/synthesis.md)

review: [definition-review.md](definition-review.md), original PASS retained for
unchanged scope plus fresh PASS for the R4/R7 numeric-null correction by native
reviewer `/root/authn_definition/numeric_null_review`.

movement_evidence: All ten review groups retain their disposition. Technical
Design's nbf:null issue is closed: JWT present null is 401 through library-native
Validation, active introspection present null is 503 provider evidence, omission
remains allowed. Fresh bounded review permits movement; no user-owned question
survives. Documentation links passed with zero errors.
Production files, builds, live providers and remote state were not changed by
this phase. Base `4edd184`, branch `codex/authn-contract-and-libraries`.

reopen_owner: none. Reopen only a rule invalidated by new evidence, preserving
reviewed independent decisions. Technical choices do not require user approval.

next_owner: Technical Design, resume `/root/authn_design`. Close full-router composition,
config/adapter owner boundaries, idempotency ordering, reusable trusted transport,
single-algorithm key admission, refresh/watch lifecycle and template projection
mechanisms. The root coordinator owns dispatch and later PR/CI delivery.

## Proof and authority boundary

Reuse pinned dependencies and existing adequate tests. Implementation chooses
concrete tests and final-validation scheduling; no new test-design phase or
multiplied build matrix is created. Final delivery needs independent review for
authorization/concurrency changes. Commit, push and separate PR are authorized;
merge, deployment, live-provider certification and independent Stage 10.2
behavior changes are outside scope.
