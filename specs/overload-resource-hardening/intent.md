# Intent: close necessary overload and resource-lifetime gaps

Status: ready.

## Problem

The reviewed queue/admission/resource-isolation research identified places where
an existing count or timeout stops before the work it is expected to protect.
It also identified workload choices that a reusable service template cannot
settle with arbitrary quotas. The user wants the necessary project corrections
implemented and delivered together in a separate pull request.

## Desired outcome

Disposition every material recommendation against current source and existing
parallel work, implement the remaining necessary corrections, and publish one
separate GitHub PR with matching validation and independent final review.
Record what was already adequate, belongs to another existing PR, or requires
a concrete service workload instead of presenting all recommendations as code
requirements.

## Affected actors and systems

Service adopters, gRPC callers, object-storage consumers, and operators sizing
HTTP/gRPC, PostgreSQL, provider calls and background workers. Optional profiles
retain their existing activation boundaries.

## Scope and non-goals

Scoped source, tests and documentation changes in this isolated template
worktree are authorized, together with local validation, commit, push and PR
creation. No merge, deployment, production read/write, infrastructure change,
load-test infrastructure, or destructive action is part of this outcome.
The original dirty checkout and unrelated concurrent PR implementations are
excluded. Historical research remains unchanged.

## Constraints

Use existing owners, configuration and dependencies where adequate. Do not add
a generic admission framework, distributed semaphore, speculative CPU executor,
or guessed workload quotas. An open PR is evidence of separately owned work,
not merged behavior or proof for this candidate. Preserve the distinction
between source inspection, local proof, exact-head CI and live runtime evidence.

## Success signal

The separate PR closes every necessary correction assigned to it by the
reviewed specification, lists the remaining service-owned decisions and
separately owned PR work, and includes the repository-required proof for its
actual changed surfaces. It makes no production capacity or deployment claim.
