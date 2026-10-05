# Intent: predictable template artifacts and operational compatibility

## Problem

The completed read-only reproducibility and recovery investigation found gaps
between what the template builds or proves and what its adoption and operating
guides imply. A source-only change can be omitted from Railway deployment
triggers; image and initializer checks do not establish every claimed artifact
property; recovery guidance leaves interacting stores and old replicas unclear.

## Desired outcome

A cloned and initialized template has predictable build, update, rollback and
recovery boundaries with minimal infrastructure adjustments. Fix the necessary
template-owned correctness, evidence and documentation gaps in one separate PR.

## Affected actors and systems

Template maintainers, derived-service developers, release reviewers and
operators; initializer and sync tooling, CI/image artifacts, Railway source or
image deployment guidance, PostgreSQL, retained messaging/jobs and object storage.

## Scope and non-goals

Accept the necessary recommendations from the completed investigation through
[Specification](spec.md). Improve existing owners and gates. Preserve current
service-specific deployment and recovery choices. No application feature,
provider migration, generalized backup platform, automatic restore, dependency
upgrade, fully hermetic build or guarantee of byte-identical historical rebuilds.

## Constraints

The user requested: “все рекомендации, которые ты нашел и реально считаешь
нужным исправить в нашем проекте, сделаем отдельным pull-реквестом”. This permits
scoped local changes and validation, branch publication and a separate PR;
merge, deployment, production backup and provider/configuration mutation are
outside the request. Preserve unrelated work and existing fail-closed initializer
contracts. Do not choose service RPO/RTO, spend, mixed-version window or retention
horizons. Reuse current tools and evidence without multiplying expensive proof
across independent harness, profile and infrastructure choices.

## Success signal

The separate PR closes the accepted template gaps with matching scoped evidence
and required review. Users can distinguish a locked source build, a validated
artifact, a retained rollback artifact and an operator-proven restore, and can
identify the service decisions still required before live operation.
