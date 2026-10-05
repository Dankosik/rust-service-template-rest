# Intent: One reliable process lifecycle for service components

Status: ready

## Problem

The service and jobs worker already own startup, readiness, cancellation,
drain, dependency cleanup, and telemetry. Research found gaps where partial
startup, stalled admission, failed tasks, and forced shutdown bypass or
misrepresent that ownership. Business components must not need another
supervisor to obtain the promised lifecycle.

## Desired outcome

Repair the genuinely necessary lifecycle gaps and deliver one separate pull
request. A component added through the existing composition root can use the
process's startup and shutdown contract; operators can distinguish clean stop,
forced or incomplete cleanup, and startup/runtime failure.

## Affected actors and systems

Service authors wiring process-owned asynchronous work; operators sending stop
signals or diagnosing failed startup; clients and worker jobs affected by drain;
the service and jobs-worker entry points and their existing provider adapters.

## Scope and non-goals

Scope includes startup cancellation, partial-resource cleanup, task and listener
failure observation, truthful bounded shutdown, budget arithmetic, and the
documentation needed to use that contract. Existing lifecycle owners should
carry these responsibilities. A new public service registration API is not an
accepted requirement; Design must justify any added interface using actual
composition needs.

No new supervisor, general component registry, verification harness, business
feature, infrastructure, deployment, or merge. Native lifecycle APIs continue
to own library-internal tasks. This work does not promise forced termination
of already-running blocking code or universal recovery from process abort.

## Constraints

Keep current configuration keys, values, defaults, retained profiles, request
contracts, and durable job/message semantics. Correcting validation and total
budget arithmetic is authorized. Preserve unrelated work. Local edits and
validation, commit/push, and separate PR creation are authorized; publication
belongs to the delivery owner after the required validation and review.

## Success signal

The existing lifecycle owners provide an explicit, usable component contract;
the identified in-scope failure paths have bounded and truthful dispositions;
matching repository validation and independent delivery review support the
change; and a separate PR contains the reviewed repair with accurate evidence
and any material unverified scope.
