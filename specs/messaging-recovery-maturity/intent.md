# Intent: usable messaging recovery and measured operating limits

Status: ready. Requester meaning reconstructed on 2026-10-06 from the approved
research recommendations and the user's instruction to implement them and
deliver a separate pull request.

## Problem

The optional JetStream and PostgreSQL outbox profiles already preserve event
identity and distinguish publication uncertainty. Their recovery guidance is
ahead of the executable adoption and operating experience: important stream
admission omissions remain, recovery across independently restored stores is
not rehearsed end to end, consumers lack a compilable durable-effect example,
and broker DLQ recovery is only a reconstruction helper. Publisher capacity and
the shared worker failure domain have not been measured under representative
outage and catch-up conditions. A tracing-test flake and opaque validation-lock
waiting also make delivery feedback less dependable.

## Desired outcome

Make these optional capabilities useful to an adopter who must detect unsafe
topology, implement a durable idempotent effect, recover a selected dead letter,
rehearse backup/restore, and understand measured capacity and failure limits.
Deliver implemented improvements, executable examples and rehearsal tools,
bounded observations, matching documentation, and a reviewable pull request.
Writing a plan or documentation alone does not satisfy the request.

## Affected actors and systems

- Adopters integrating typed events and a PostgreSQL business transaction.
- Operators of producer PostgreSQL, source/DLQ streams, durable consumers and
  consumer effect databases.
- The Rust template's messaging adapter, optional outbox and jobs worker,
  relevant generated profiles, and repository validation feedback.
- The developer using local validation or interpreting the existing CI result.

## Scope and non-goals

Accepted work covers all six approved areas:

1. Admit only ACK-capable source/DLQ streams and a DLQ that can carry the
   supported normal envelope after transfer metadata is added. Preserve the
   storage admission and ACK-loss improvements already proposed in PR #239.
2. Ship and execute bounded recovery rehearsals using real producer PostgreSQL,
   source/DLQ storage, consumer positions and effect receipts. Include mismatched
   restore points, R3 one-node loss, storage exhaustion, and consumer outage
   across retention boundaries, with exact logical-ID evidence.
3. Provide a compilable, opt-in idempotent consumer example with receipt and
   business effect in one caller-owned transaction, including duplicate,
   restart, rollback and uncertain-commit behavior.
4. Provide a usable controlled broker DLQ workflow: inspect, reconstruct the
   selected record, publish with stable identity, and deliberately retire that
   exact record only after positive confirmation.
5. Supply measurements for backlog, outage, catch-up, queue age, PostgreSQL and
   broker load with a representative R3/TLS setup. Use the findings to decide
   whether publisher concurrency, effective consumer limits, or role separation
   warrant implementation.
6. Diagnose and close the reported object-storage tracing-test instability and
   make validation-lock ownership and waiting understandable without weakening
   heavy-validation serialization.

No default business feature, universal inbox framework, second delivery engine,
automatic DLQ sweep, perpetual retry policy, provider-effect reconciliation
service, or production infrastructure change is requested. Real service
business RPO/RTO/SLO and any acceptance of data loss remain adopter decisions.
There is no promised 10x throughput gain.

## Constraints

Local source/docs edits, necessary local validation, bounded ephemeral
rehearsal/measurement fixtures, commit, push, a separate or coherently related
pull request, and existing CI are authorized. The delivery owner may integrate
equivalent PR #239 work or update that PR before merge as needed for a coherent
candidate. PR composition is technical delivery work, not missing user intent.

Changes to actual deployed streams, consumers, broker settings, production
infrastructure, production sensitive data, purchases, merge and deployment are
outside this authority. Fixture effects must stay on explicitly owned test
resources; no host-wide security or compiler-cache policy change is authorized.
Native tools and existing libraries come first. Heavy local work remains
serialized under current repository policy.

Assumption: representative local/CI R3 and TLS measurements are adequate for
template findings, with their topology and host limits disclosed. They do not
certify independent production failure zones. Reopen Intake only if a real
service target or production observation becomes part of the requested outcome.

## Success signal

An adopter can run the delivered example and bounded rehearsals, inspect the
actual outcomes by logical identity, safely recover a selected DLQ record, and
read a defensible capacity/limit report. Admission refuses the concrete unsafe
settings; uncertain operations retain their recovery identity and are never
reported as confirmed. The reported feedback defects are causally resolved or
proved already resolved by current code. Required validation and existing CI
support the final candidate, and the pull request is ready for user review.
