# Intent: ready infrastructure cancellation and cleanup

status: ready

## Problem

A business-feature developer should use the template's infrastructure without
inventing cancellation and cleanup machinery for each operation. The accepted
cancellation research found one concrete local body-retention gap and several
usage distinctions that are easy to misinterpret as guarantees about remote
effects or completed shutdown.

## Desired outcome

Deliver the recommendations that are necessary now in one separate pull
request: repair the concrete cleanup gap and make the supported usage paths
easy to follow. Preserve infrastructure ownership of local lifecycle mechanics
while leaving business identity and externally visible outcome policy with the
feature or its integration adapter.

## Affected actors and systems

Feature developers, their provider adapters and HTTP handlers, the
`infra-object-storage` download wrapper, and the existing developer guides.
Other infrastructure components supply established behavior and usage context.

## Scope and non-goals

Accept the minimal necessary implementation and documentation from the reviewed
research. Research recommendations are inputs to scope selection, not a demand
to implement every hypothetical scenario or verify every unobserved guarantee.

Keep plain SQLx and the accepted temporary `sqlx-core` whole-return backport.
Do not restore an application pool facade, add a generic cancellation framework,
or turn direct S3-to-HTTP streaming into a new supported lifetime contract.
Production infrastructure and unrelated hotpath work are outside this change.

## Constraints

Implementation, scoped local validation, commit, push and opening a separate
PR are authorized. Merge and deployment are outside the requested outcome.
Use the isolated `codex/cancellation-cleanup-20261005` worktree and preserve
unrelated changes. Retain current APIs and error meanings unless a necessary
behavior correction is specified explicitly.

## Success signal

A failed retained download releases its owned provider body without requiring
the feature to destroy the wrapper. The developer can find ready recipes for
ordinary awaited work, buffered S3 reads and durable follow-up work, and can
distinguish stopping a wait, releasing local ownership and establishing a
remote effect's outcome. The separate PR contains the necessary fix and
guidance with appropriately scoped evidence and accurate limits.
