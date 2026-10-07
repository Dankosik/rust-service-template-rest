# Specification: messaging recovery maturity

Status: ready. Definition candidate D2, 2026-10-06.

Requester meaning: [intent](intent.md). Current evidence and drift dispositions:
[current-state research](research/current-state.md). This contract owns behavior
and proof expectations; Technical Design owns mechanisms, placement and the
empirical decision path, and Implementation chooses concrete tests.

## Outcome and unchanged authority

The adopter receives executable recovery and adoption paths, honest evidence of
failure/capacity limits, and usable development feedback. Each of the six Intent
items is accepted below as B1 through B6; none is satisfied by a documentation
recommendation alone. Final delivery includes a reviewable pull request and
the actual applicable validation/CI outcome, with any incomplete scope explicit.

Messaging/outbox remain opt-in and inert when omitted. Preserve the Go wire,
logical event identity, immutable publication identity and payload, finite
outbox retry/failed custody, source settlement only after successful handling or
confirmed DLQ transfer, and the native resource/lifecycle guarantees on main.
The [outbox](../../docs/postgres-transactional-outbox.md) continues to use the
caller's transaction and the existing jobs engine. Broker deduplication and
consumer ACK state are not durable business-effect identity. At-least-once
delivery and lack of global ordering remain explicit.

Service policy remains local: no template business event, receipt TTL, SLO,
permitted-loss target, replay horizon, or production topology is invented. A
receipt must remain valid for every permitted replay or be retired only with
an adopter-owned reconciliation/replay restriction. External effects cannot
be made atomic by a local receipt; their provider identity and reconciliation
remain required adopter responsibilities.

## Terms and finality

- **Logical ID** identifies the same business event through retry, duplicate
  delivery, restore and redrive. It is the example's durable-effect key within
  its defined consumer/effect scope.
- **Publication ID** identifies one immutable publication. Retry after possible
  dispatch preserves it; explicit DLQ redrive derives its stable identity from
  the actual selected DLQ record while retaining the original logical ID.
- **Confirmed** publication means positive PubAck for the expected source/DLQ
  stream, duplicate PubAck included. It proves that publication boundary only.
- **Ambiguous** means dispatch/effect may have happened but its authoritative
  result is unavailable. It is neither absence, successful retirement nor
  permission to generate a replacement identity.
- **Recovery complete** applies to a named scenario and known logical-ID set:
  required bytes/effects are accounted for and permitted replay has produced
  the expected durable effects. An unexplained gap is incomplete, even if
  aggregate counts match. A scenario may instead finish with a verified stop
  identifying missing data or required business reconciliation.

## B1. Admit an ACK-capable, usable transfer path

At messaging startup, inspect every stream the selected role publishes into:
source for a publisher/outbox, source and the resolved DLQ for a consumer.
Reject `NoAck=true` before starting publication/consumption, with the affected
resource role and a closed, sanitized reason. Consumer acknowledgment policy is
a separate setting and must not be mistaken for stream publication ACKs.
Disabled messaging performs no admission. Publisher-only operation does not
invent a DLQ requirement. No stream is created, rewritten or repaired by startup.

Carry forward PR #239's file-storage/default-persistence admission or equivalent
behavior when integrating its accepted changes onto current main. Explicit
asynchronous persistence and memory storage are incompatible with that durable
profile. R3 placement, fsync and production failure-zone claims remain operator
properties, not consequences of successful startup.

A consumer's effective server and DLQ message-size limits must carry each
supported normal message after required DLQ transfer metadata is added. Here
normal means a valid envelope for a configured supported route, within the
accepted source and adapter payload/header bounds, including admitted trace
context; it includes a valid delivery rejected permanently or exhausted by its
handler. The comparison covers the actual transfer wire bytes, including
original-subject, reason, publication and expected-stream headers. A positive
finite DLQ limit below that supported bound is refused; an unset/unlimited DLQ
limit is constrained by the effective server bound. Admission must not certify
a bound it cannot establish. The concrete bound calculation belongs to design.

This does not promise transfer of arbitrary legacy/malformed oversized records
or repair of messages with unbounded unsupported metadata. Any runtime transfer
that is rejected or ambiguous retains source custody and uses the current
visible failed-transfer path; it cannot ACK away the only retained record.
Later operator configuration drift never turns an inconclusive publish into
success. Startup admission does not promise continuous topology enforcement.

Nearest falsifier: an ACK-disabled stream is admitted, or a supported maximum
normal transfer is refused solely because its required metadata exceeded a
topology admitted as compatible; or failed transfer causes source retirement.

## B2. Execute recovery rehearsals with identity evidence

Provide a repeatable, opt-in rehearsal using real PostgreSQL and the pinned
JetStream provider. A run owns its synthetic data and temporary resources,
records versions, topology, storage/retention settings and backup boundaries,
and captures producer business state/outbox, source and DLQ bytes, durable
consumer positions, and consumer receipts/business effects. Native backup and
restore facilities are preferred; simulating a restore by only editing rows or
counts does not satisfy the accepted scenario.

The delivered capability and its bounded demonstration cover these outcomes:

| Scenario | Required observable result |
| --- | --- |
| Coherent backup and restore, including source, DLQ and consumer state | Recovered known logical IDs and exact payloads agree with the selected backup boundary; required replay produces the expected durable effects without duplicating already recorded effects. |
| Older broker backup with newer producer PostgreSQL | Identify publications whose source bytes are absent, distinguish still-publishable retained intent from terminal/deleted intent, replay only from retained authoritative bytes under the same logical identity, and stop explicitly where bytes/reconciliation are missing. A completed producer job alone does not reconstruct broker data. |
| Older effect database with newer consumer position | Identify events passed by the consumer whose effects/receipts are absent from the restored effect database. Recovery must deliberately replay retained authoritative events or stop with exact missing identities; resuming from the newer ACK position alone is not success. |
| Loss of one node in a three-replica stream | With the other two replicas healthy and the selected records within retention, every publication positively acknowledged before the fault remains recoverable with the same logical ID and exact payload. Distinguish later confirmed, rejected and ambiguous publications and account for every fixture ID through recovery. Unexpected loss of an acknowledged retained record fails the rehearsal; naming the loss is not a passing stop. A three-container local result is not independent-zone certification. |
| Exhausted broker storage/capacity | Establish that the exercised resource boundary is actually exhausted. Distinguish refusal/ambiguity, retain recoverable producer/source custody, then demonstrate bounded recovery once capacity returns or a visible terminal stop. |
| Consumer outage and retention pressure | Demonstrate catch-up for retained events and explicit identification of expired/missing events when the recorded retention boundary is exceeded. Successful reconnect or a drained backlog alone cannot prove no loss. |

For each known logical ID, retain enough evidence to distinguish retained bytes,
replayed publication, pre-existing effect, newly applied effect, duplicate
suppression, missing bytes and unresolved reconciliation. Expected loss/stop cases
are the explicitly mismatched restore points or exceeded retention boundaries
above; they pass only when that declared limitation is detected and reported as
such, and are not labeled lossless recovery. Unexpected missing acknowledged
data within an exercised durability guarantee fails the scenario. Actual backup/restore
and catch-up durations are observations, not adopted RPO/RTO guarantees.

An incomplete setup, skipped scenario or absent required receipt is reported as
unverified, never a passing rehearsal. Existing appropriate scenarios may be
reused, and one run may satisfy multiple obligations. The delivery owner chooses
an opt-in/local/CI execution boundary without adding the heavy matrix to every
commit. No deployed data or stream changes are authorized by this tooling.

Nearest falsifier: swapping one lost logical ID for a duplicated ID passes the
rehearsal because counts agree, or a mismatched restore silently claims success.

## B3. Compilable durable-effect adoption example

Ship an opt-in example wired to the actual typed consumer and existing
PostgreSQL transaction boundary. Its receipt and demonstrable business mutation
commit in the same caller-owned `Tx`; no receipt may survive a rolled-back
effect, and no confirmed successful effect may lack its receipt. The example
declares the logical-ID scope so two concurrent deliveries of the same event
cannot both apply the effect. Restart and replay use persistent identity,
not process memory or the broker duplicate window.

The example makes these outcomes observable: first successful application,
duplicate after restart, concurrent duplicates, effect failure rolling back
receipt/effect together, and an uncertain COMMIT. With an uncertain COMMIT it
does not report ordinary success or automatically rerun the mutation closure.
It reconciles the same logical identity against authoritative durable state;
confirmed receipt/effect permits duplicate completion, established non-effect
permits a same-identity attempt, and inconclusive state remains unresolved and
unacknowledged for safe recovery. Reconciliation reads cannot infer absence from
a racing or still-unresolved original transaction.

The same logical ID supplied with conflicting event meaning must not silently
count as an equivalent duplicate; the example declares the immutable meaning
it compares and exposes the conflict. The schema and mutation are example-owned
and do not become a default production feature or universal inbox abstraction.
The example's receipt lifetime covers its permitted replay, with no automatic
short TTL disguised as an exactly-once guarantee.

Nearest falsifier: a failed effect leaves a receipt that suppresses recovery,
concurrent duplicates mutate twice, or an unknown commit repeats the effect
without first establishing the durable outcome.

## B4. Controlled recovery of one broker DLQ record

An operator can inspect a selected DLQ record without mutation, identify its
actual stream/sequence/stored timestamp and restorable event meaning, and
request redrive of that exact record. Native inspection is sufficient when it
provides these facts; a new parallel inspection service is not required.
Inspection distinguishes missing, malformed/unrestorable and restorable data.
Payload inspection is an explicit operator action, not routine payload logging.

Redrive preserves logical ID, event type/version/time, original destination and
payload bytes, using the existing deterministic redrive identity from actual
DLQ coordinates. It must not rebuild identity from source coordinates or select
another record by an unstable cursor. A repeated operation after ambiguity,
including process restart, uses the same immutable prepared publication.

The workflow reports publication and retirement separately:

| Publication/result | Permitted retirement and next result |
| --- | --- |
| Invalid record, definite refusal or unavailable precondition | No retirement; report refusal with the selected record intact. |
| Possible dispatch without positive PubAck | No retirement; report ambiguity and retain the same prepared identity for recovery. |
| Positive PubAck for the expected destination, including duplicate ACK | The operator may deliberately retire only the inspected exact record after the workflow establishes it still represents that record. Confirmation does not claim a business effect. |
| Confirmed publish but failed/unknown retirement | Report publication confirmed and retirement incomplete/unknown separately. Inspect the exact record to resolve retirement; do not mint a new publication or claim the DLQ is cleared. |
| Record absent or replaced before a safe action can establish its identity | Report the unavailable/stale selection; perform no publication or deletion based on that stale selection. |

Crash/retry recovery cannot delete a different record after a stream restore or
replacement reuses a sequence. Conflicting concurrent operations must not
silently expand one-record authority. Deliberate retirement may be combined in
one bounded operator invocation, but remains conditional on confirmed publish
and exact-record identity. Native capabilities, proof of identity and command
shape belong to Technical Design.

No automatic sweep, perpetual retry, stream administration, business-effect
reconciliation bypass, or second delivery engine is introduced. The workflow
states that retries beyond the broker window can duplicate delivery and relies
on durable logical-ID effect handling over the permitted replay horizon.

Nearest falsifier: a lost PubAck deletes the DLQ record, restart changes its
redrive identity, or a stale sequence retires a different retained event.

## B5. Measured capacity and failure-domain decisions

Deliver an executable bounded measurement path and retained findings for the
current one-slot publisher and shared jobs/consumer/publisher process. The
representative path uses R3 and TLS, actual producer PostgreSQL/outbox and a
consumer effect boundary; synthetic handler-only or R1/plaintext results are
labeled separately and cannot supply that claim.

Record offered/admitted/published/applied rates, backlog and oldest queue age,
outage duration, time/rate to catch up, errors/ambiguity/retained failures,
PostgreSQL pool/query/load indicators and broker storage/load indicators needed
to attribute the observed bottleneck. Record worker and durable-consumer
concurrency, effective `MaxAckPending`, replica count, limits, host resources
and competing load so the observed envelope can be reproduced and interpreted.
Include the interaction with ordinary jobs, and identify when a messaging
admission or critical-task failure affects the other roles in that process.

Use comparable baseline and candidate evidence to decide publisher concurrency,
effective `MaxAckPending` configuration and role separation. Implement a change
only where the result supports a present constraint; retaining a current limit
is an acceptable evidence-backed decision. The selected behavior, defaults,
overload/drain consequences and proof must be closed by Technical Design before
such a runtime change is implemented. A chosen measurement alone creates no
throughput promise and does not justify unbounded workers, connections or retry.

Completion requires delivered measurement capability, actual bounded results
and a disposition for all three choices. If an authorized environment cannot
establish a required claim, retain that incomplete scope rather than substituting
an unrepresentative measurement or expanding into paid infrastructure.

Nearest falsifier: a claimed capacity improvement is only a concurrency knob
increase, omits growing queue age or database pressure, or attributes unrelated
main/runtime changes to the candidate.

## B6. Trustworthy feedback during development

Investigate the observed region-span test's zero-versus-one failure against
current main and its unchanged passing rerun. Close the cause with a bounded
fix and meaningful regression evidence, or identify the already-landed causal
repair with equivalent evidence. Keep the asserted telemetry contract: one
operation span, AWS region on Amazon, no invented region on other providers.
An unchanged rerun, ignored test or weaker assertion does not close the issue.
Do not assume the test harness, production instrumentation or shared callsite
cache is faulty before discriminating evidence.

When validation waits on the existing shared lock, it identifies waiting as
distinct from a running command and exposes enough non-secret owner information
to locate the owning process, checkout and command, together with elapsed wait
and the configured timeout. Emit an initial wait diagnostic promptly and bounded
progress while waiting, then distinguish acquisition, cancellation and timeout.
Missing or stale owner data must be represented honestly; diagnostics must not
reclaim a live owner's lock, kill its process or start validation concurrently.
Timeout/cancel before acquisition runs no requested validation command.
Successful acquisition preserves the original command's exit result.

Preserve Git-common heavy-validation serialization and current build-speed
ownership. No machine-wide security exemptions, cache changes, unrelated build
system migration or weakening of required checks is part of this repair.

Nearest falsifier: a waiting developer cannot distinguish lock contention from
a hung build, or a diagnostic improvement permits two heavy validations to run.

## Composition, compatibility and proof boundary

A producer commits business state and immutable outbox intent. A possible
publication followed by a lost response retains identity and custody. The
idempotent consumer resolves repeated deliveries by its durable receipt; a
permanent failure transfers to an admitted DLQ before source settlement. An
operator inspects and redrives the retained record with stable identity and
retires it only after confirmation. If independently restored stores disagree,
the rehearsal accounts for those same identities and either recovers from
retained authority or reports the missing data/reconciliation stop. Measurements
must include these actual custody/effect boundaries to characterize catch-up.

All requirements can pass only if executable code/example/tooling and observed
results exist; prose-only recovery steps fail the accepted outcome. New
verification infrastructure is bounded by the specifically approved rehearsals
and measurement capability. Reuse existing canonical validation and CI; do not
multiply full builds across fixture dimensions or invent a test-design phase.

Preserve current main behavior when incorporating the older PR #239 candidate;
its whole-tree replacement would lose #254 custody fixes. Technical Design
chooses integration and coherent delivery boundaries. Profile removal and Go
wire compatibility remain existing gates for whichever surfaces actually change.

Reopen Specification if design discovers that supported-envelope bounds,
record identity across restoration, transaction reconciliation, or a selected
capacity change requires different observable behavior. Reopen supporting
Research for new provider/version evidence. Reopen Intake only for changed
desired behavior, a real service SLO/loss decision or additional external effect
authority. None is required to begin Technical Design for this contract.
