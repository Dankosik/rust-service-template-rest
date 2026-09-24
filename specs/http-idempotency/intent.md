# Intent: optional PostgreSQL-backed HTTP idempotency

## Problem

A derived service on this template has no safe way to let clients retry a
mutating HTTP request whose outcome they did not observe. A lost response, a
client timeout, or a request timeout can hide whether the mutation happened, so
a blind retry can repeat a business effect. The PostgreSQL profile already
offers one transaction and a commit-outcome distinction, but nothing binds a
client-supplied `Idempotency-Key` to a single committed effect and a replayable
response across replicas.

## Desired outcome

Deliver roadmap stage 10.3 locally as an optional, default-off initializer
profile. A selected derived service can mark an operation `x-idempotent: true`
and get: one committed business effect per verified caller, operation, and key
inside a published retention window; the replay of that success to later
retries with the same input; stable, distinct outcomes for key reuse with
different input, concurrent attempts, an unavailable or read-only database, and
an unknown commit outcome. The business effect and its replay evidence commit
in one PostgreSQL transaction or not at all. Complete the applicable repository
phases, independent reviews, implementation, and local acceptance. Stop before
PR, push, merge, or deployment.

## Affected actors and systems

Derived-service developers and initializer users; API clients that retry
mutating requests; operators of the PostgreSQL writer and service replicas;
template maintainers. Systems: the HTTP contract and generated OpenAPI document,
the hardened chain and protected-operation composition, the PostgreSQL pool,
transaction seam, migrations and real-database proof, service configuration and
bootstrap, the closed problem catalog, the initializer, lock, markers, sync
rules, and profile/harness validation.

## Scope and non-goals

Include the `x-idempotent: true` declaration and its agreement with served
handlers, the `Idempotency-Key` grammar, key scope and request fingerprinting,
one-transaction execution and replay, retention and cleanup, concurrency,
failure and uncertain-commit outcomes, configuration, schema migration, adopter
guidance, profile markers, initializer selection and lock recording, and real
PostgreSQL proof.

Preserve the existing authentication, outbound HTTP, and PostgreSQL contracts.
Do not add a product operation, a non-PostgreSQL store, idempotency for
external side effects such as outbound HTTP calls, profile migration for an
already initialized service, or any other stage-10 capability (jobs, webhooks,
messaging and outbox, gRPC, OAuth client credentials, object storage, reference
service).

## Constraints

Treat the committed Go template (`749473366bd4544c96cd640d09cb4b6ff7ff6473`) as
evidence of problems and reasons, not code to copy; Rust idiom outranks Go
parity, and deviations are recorded with reasons. Research current primary Rust
and PostgreSQL mechanisms and the existing crate ecosystem before choosing
mechanism. Keep the public initializer's complete locked metadata, formatting,
and OpenAPI preflight before any target write. Validate profile/harness
combinations through canonical projections and build/test each distinct runtime
graph once, without multiplying full builds by harness
([validation boundary](../../docs/template-sync.md#validation-boundary)).
Reopen only a decision contradicted by new evidence.

## Success signal

A selected service with an idempotent operation, against a real PostgreSQL,
executes one business effect for concurrent and repeated same-key requests,
replays the committed success, refuses different input and a read-only writer,
never re-executes after a lost commit acknowledgement, and leaves nothing behind
after a rollback. Unselected output contains no idempotency runtime pack;
selected output is complete, inert until an operation opts in, and documented.
Independent review and matching local validation establish the fixed
candidate, with no remote-delivery claim.
