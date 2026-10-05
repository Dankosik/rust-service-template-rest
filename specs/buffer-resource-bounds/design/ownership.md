# Buffer and resource bounds: Ownership Map V1

This map and [selected design](selected-design.md) are one Technical Design
candidate under the [specification](../spec.md). Current source is baseline
`5927ffbba351af2f7fb8635316bbfa4ae5b31da6`. No application crate or public business
interface is added. Symbol-level implementation and test cases belong to executors.

## Responsibilities

| Responsibility | Affected path and current evidence | Semantic owner and exact action | Dependency/composition/generated boundary | Cleanup and proof owner | Reopen condition |
| --- | --- | --- | --- | --- | --- |
| R1 unread-tail collection | Download::bytes, download.rs:145; remaining/last at :103–124 | infra-object-storage; replace original-size reservation with unread state | Existing public Download/Body and metadata unchanged; no dependency edge | Existing terminal/drop state machine; adjacent download and HTTP-fixture proof | Unread state no longer describes the collection or EOF/checksum ordering changes |
| R2 collected SDK envelope | Runtime collect-before-parse, four control operations plus GET errors; selected design S2 | infra-object-storage; private response_limit.rs composes supported Intercept, MapFrame and Limited; lib.rs wires client non-2xx and four control-success hooks | Existing S3 reexports and declared http-body/body-util; no SDK direct dependency, parser or retry override replacement | SDK drops failed body; existing Reply::Lost and operation guards own outcome/observation; current in-process storage fixtures | Hook ordering, operation inventory or checksum wrapper differs from pinned source |
| R3 bounded job preparation | enqueue.rs::prepare shared by enqueue and compare_live_payload | infra-jobs; private counting/discarding std writer in enqueue.rs | Existing serde_json std feature; SQL, Tx ownership and stored JSON unchanged | Writer local to preparation; existing jobs preparation proof | Required sharing owner emerges or observable serialization/error order changes |
| R4 bounded event preparation | PreparedEvent::prepare at prepared.rs:30–72; outbox consumes prepared bytes | infra-messaging; private counting/discarding writer in prepared.rs | No jobs edge for messaging-only; domain-events retains no wire policy; base64 format unchanged | Writer local; prepared Bytes are caller-owned; current event/outbox parity proof | Cross-profile common owner becomes justified or prepared wire contract changes |
| R5 cache admission and abandoned exchange | connection.rs Link/Shared/Generation and current supervisor | infra-cache; Link application/probe gates and private retirement guard in connection.rs | Existing Tokio dependency and supervisor; no cache-key/value policy or readiness gate | Synchronous generation retirement before permit release, current supervisor recovery/last-owner drop; adjacent cache/silent-server proof | Native controls provide immediate admission plus response-owned permits, or lifecycle/budgets change |
| R6 native request/ACK lifetime | async-nats0.50.0 lib.rs and jetstream/context.rs; published source and upstream1617/1618/1629 | Dependency-source owner; vendor/async-nats same-version repair described in selected design | Excluded dependency selected by root patch; no public API, framing, parser, dependency-vector or feature change | Retained ACK receiver and adaptive handler reclamation/acker; native regression plus infra-messaging public parity evidence | Maintained equivalent release or proof invalidates ACK-drop/pruning behavior |
| R7 publication receiving limits and native window | messaging.rs ContextBuilder; producer.rs and DLQ common helper; outbox calls Producer::publish | infra-messaging; configure P/immediate native admission, validate source final headers and resource M, retain DLQ exception | Shared Context clones; expected-stream remains native header/ACK policy; no transport or business graph change | Repaired native ownership, original deadlines/cancel and source settlement; current messaging/outbox proof | M/H, broker-size or native ACK/admission semantics change |
| R8 vendor custody and removable profile | Existing sqlx-core patch/exclusion/Docker/classifier/profile pattern | Root dependency/delivery owners; add messaging-scoped patch, exclusion, vendor path, Docker and classification/removal entries; deliberate lock source update | Messaging absence removes its patch and copied source; no new application package; generated initialized Cargo graph remains derived | PATCHES.md archive/diff/hash/removal custody; existing graph, dependency, classifier, initializer and image owners | Archive/API/dependency identity changes or absent profile retains an edge |
| R9 adopter guidance | Existing guides and audit dispositions S6 | Each guide's current profile owner; bounded corrections and feature recipes only | Profile markers retained; no generated protocol edit or universal runtime policy | Static consistency and existing docs/examples proof; no new benchmark/infrastructure | Wording requires a new runtime policy or broader example |

R2's reuse rung is supported SDK/library extension, with pinned source evidence
in the design. A custom collector and one new direct Metadata dependency are
rejected as unnecessary; parity is unchanged SDK parsing/IDs/retries/checksums.
R3/R4's rung is existing serde_json plus minimal private std writer policy because
no declared writer provides bounded retention with exact count and late-error
precedence. R5's rung is existing supervisor/Tokio because native Redis alone
waits and releases on caller drop. R6's rung is a narrow published-source repair:
upstream1629 supplies the smaller handler-local pruning source, paired with
ACK retention. Upstream1618's spawned cleanup and a custom dirty-flag/receiver
wrapper are rejected as unnecessary; removal requires equivalent released behavior. R7 uses native admission/protocol after R6. Each retained component has
one present invariant; removing it recreates its named failure.

## Files

The rows below are the exact materially changed Rust owners. Existing tests stay
beside their owner; an executor may add cases in its existing test module or fixture
file, without a new production seam merely for inspection.

| Path | Responsibilities | Present reason / declarations and visibility | Call-path role, lifecycle and error ownership | Allowed dependencies | Forbidden responsibilities |
| --- | --- | --- | --- | --- | --- |
| crates/infra-object-storage/src/download.rs | R1 | Existing public Download; private reservation logic | Existing bytes -> next_chunk; terminal/stable-error and permit owner unchanged | Current body/bytes/Tokio types | Metadata rewrite, forced early EOF, blanket shrink/copy |
| crates/infra-object-storage/src/response_limit.rs (new) | R2 | Private response-envelope Intercept type with nonoverlapping error/control-success modes; one actual protocol-admission responsibility | Before SDK collection, using response replacement; body errors remain SDK response errors | S3 reexports, std, bytes, http-body, http-body-util | XML parsing, retry policy, new public error, successful GET manipulation |
| crates/infra-object-storage/src/lib.rs | R2 | Private module declaration and hook registration in existing client/control builders | New/new-call wiring including HeadBucket probe; preserve once mutation config | Existing dependencies only | New client/provider, key policy, operation-budget change |
| crates/infra-jobs/src/enqueue.rs | R3 | Existing prepare plus a private bounded writer | Both enqueue/compare prepare before SQL; Serialize > size > NUL order | std and existing serde_json | SQL/schema change, shared utility API, outbox policy |
| crates/infra-messaging/src/prepared.rs | R4 | Existing prepare plus a private bounded writer | Preparation precedes identity/time checks as today; caller owns returned Bytes | std, existing serde_json/bytes | Jobs dependency, new envelope/base64 format |
| crates/infra-cache/src/connection.rs | R5 | Link owns two private immediate gates; private generation-retirement guard | Application/probe acquisition, response/drop, existing supervise/recovery; raw maintenance behavior retained | Existing std/redis/Tokio/tokio-util | New command queue, write replay, cache-byte policy, new spawned task |
| vendor/async-nats/src/lib.rs (new copied source; narrow patch) | R6 | Native Multiplexer gains the selected upstream adaptive pruning methods/threshold | Existing single handler owns map/wire commands and bounded stale metadata; closed queued senders are pruned by the same threshold | Upstream dependencies | New receiver/client plumbing, task/channel, skipped wire publication merely because receiver closed |
| vendor/async-nats/src/jetstream/context.rs (new copied source; narrow patch) | R6 | Retained existing receiver in PublishAckFuture and native acker | Poll cancellation transfers receiver+permit; completion drops receiver before release; native ACK parser intact | Upstream dependencies | Alternative ACK schema, altered broker errors, extra cancellation timeout |
| crates/infra-messaging/src/messaging.rs | R7 | Existing Context construction adds max_ack_inflight/backpressure controls and checked P admission | One resource/Context; no added lifecycle owner | Existing async-nats and std | New manager/connection generation, permanent cancellation failure, config key |
| crates/infra-messaging/src/producer.rs | R7 | Source boundary checks resource M/final H; common send accepts already finalized native message | Source/outbox prepare expected-stream and tracing before check; unchanged native classify_publish/deadline | Current async-nats/wire/bytes | Ordinary M/H applied to malformed-source DLQ; new queue/semaphore |
| crates/infra-messaging/src/consumer.rs | R7 | Existing DLQ builder finalizes expected-stream header before shared send | Same copied/restorable envelope, native broker size check, confirmation before ACK and failure redelivery | Current messaging/native types | New topology, truncation, source ACK on refused transfer, changed consumer slots |

Non-Rust custody paths for R8 are Cargo.toml, Cargo.lock,
vendor/async-nats/PATCHES.md and the pristine published package files,
.dockerignore, build/docker/Dockerfile, scripts/lib/template_profiles.json and
scripts/ci/changed-surfaces.sh. Add only the current messaging-profile markers and
source routing those paths need; existing SQLx/hotpath carriers stay unchanged.
An initializer implementation file changes only if existing declarative removal
cannot represent the new vendor edge, which would reopen R8 rather than silently
invent a second removal mechanism.

R9's exact guide owners are docs/object-storage.md, docs/cache.md,
docs/durable-messaging.md, docs/background-jobs.md,
docs/postgres-transactional-outbox.md, docs/grpc.md, docs/architecture/http.md
(request/response lifetime) and docs/backend-utility-recipes.md (buffer/feature
recipe). docs/authentication.md
and docs/outbound-machine-authentication.md change only if the audit's count/weight
wording needs correction. Existing crate doctests/examples may be updated only
where they mirror those corrected contracts; no new example application is added.

The generated OpenAPI/protobuf authorities, domain-events, service composition,
database statements/migrations, configuration values and provider topology are
unchanged by this map. Planning can order R6 before R7's assembled proof and keep
R1/R2, R3/R4, R5 and R9 independently owned, with one final validation boundary.
