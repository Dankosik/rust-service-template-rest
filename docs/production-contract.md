# Production Contract

Status: **Unresolved — production promotion is blocked until the owning
service accepts every applicable field.** Use `N/A` only with a concrete
reason. This file becomes service-owned with the first production feature;
`crates/config`, `crates/service/src/bootstrap`, `api/openapi/service.yaml`,
and each retained `crates/infra-*` adapter own the template's runtime defaults,
limits, probes, and exit codes.

## Service scope

- Business capabilities: Unresolved.
- Authoritative facts and data owners: Unresolved.
- Public and internal interfaces: Unresolved (`api/openapi/service.yaml` and
  `crates/service/src/bootstrap` define the scaffold's application and private
  diagnostics listeners).

## Dependency contract

| Dependency | Criticality | Readiness | Deadline | Concurrency | Retry owner | Identity | Ambiguous outcome | Recovery |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Unresolved | Unresolved | Unresolved | Unresolved | Unresolved | Unresolved | Unresolved | Unresolved | Unresolved |

Register a readiness probe only for dependencies whose loss makes the replica
unable to serve, and budget every downstream deadline inside
`http.request_timeout` (or the caller's gRPC deadline).

## Capacity and resource envelope

- Expected and burst arrival rate: Unresolved.
- Service time, payload bounds, and expected concurrency: Unresolved (size
  `crates/config` listener, body, header, and in-flight limits from workload
  measurements rather than scaffold defaults).
- First capacity ceiling and required headroom: Unresolved.
- Database connections, worker concurrency, and queue-age objective: N/A for
  the health-only scaffold; when retained, budget across peak API/worker
  replicas, rolling overlap, dedicated `LISTEN` sessions, migrator/admin
  reserves, and pooler front/back limits.
- Surviving capacity after one failure-domain loss: Unresolved.
- Cold/unavailable cache source capacity (loaders, waiters, replicas): N/A for
  the health-only scaffold; reopen when cached state is adopted.
- Streaming, background, and blocking work: bound body lifetimes, durable queue
  backlog/retention, and CPU/blocking task permits through actual execution
  completion rather than request-head admission alone.

## Consistency and durability

- Transaction and read guarantees: Unresolved.
- Asynchronous propagation, replay, deduplication, and retention: Unresolved
  (for each durable job, event, or webhook handler, define logical effect
  identity, idempotency/reconciliation boundary, and retention horizon).
<!-- template:begin outbox:outbox-effect-contract -->
- For each outbox consumer, record its logical subscription and effect key.
  Commit the key and the business mutation together. For an external effect,
  reuse the remote operation's idempotency key or reconcile an uncertain
  result there before retrying. Retain evidence through the last permitted
  replay, including operator redrive. Exercise both a lost consumer ACK after
  commit and a repeated publication beyond the stream's duplicate window.
  The [consumer recovery examples](../test/tests/messaging_recovery.rs) cover
  matching and conflicting identities, rollback and uncertain commit.
- For a route requiring order, assign one contiguous event version per
  namespaced aggregate key, starting at one, in the transaction that updates
  that aggregate. Consumers applying ordered changes must also guard their
  own applied version. Redelivery can reintroduce an earlier version.
<!-- template:end outbox:outbox-effect-contract -->
<!-- template:begin http-idempotency:production-contract-retry-window -->
- For mounted idempotent HTTP operations, publish the client retry window and
  how to look up the original effect in the service's public API documentation.
  Measure the window from the client's first send to the database check on
  retry. Include transit, request processing and pool wait in the client's
  budget. Set `http_idempotency.retention` on every writer replica to at least
  the published window. The database sets expiry when it writes the success,
  before commit and delivery. Publish a longer window only after every writer
  supports it, with an effective time for new requests. Earlier requests keep
  their original window. When shortening retention or rolling back, honor
  requests first sent under the earlier promise until that window expires.
<!-- template:end http-idempotency:production-contract-retry-window -->
- RPO, RTO, backup/key custody, and restore-compatible versions: Unresolved.
- Cache staleness, TTL, invalidation, and token/key revocation lag: N/A for the
  health-only scaffold; reopen when cached state or authentication is enabled.

## Edge and trust

- TLS termination and trusted proxy topology: Unresolved.
- Gateway retry and fleet rate-limit owners: Unresolved (the service sheds
  in-flight overload per replica rather than enforcing fleet rate limits).
- Diagnostics/metrics listener network isolation: Unresolved (keep the
  diagnostics port unreachable from untrusted networks).
- Egress, identity, and authorization authorities (trusted issuers, audiences,
  callers, and destination origins): Unresolved.

<!-- template:begin authn:production-bearer-delegation -->
When migrating a handler that used `VerifiedPrincipal::access_token()`, keep
identity-only handlers on `VerifiedPrincipal`. For each HTTP operation that
exchanges the caller's token, declare `x-bearer-delegation: true` alongside
its bearer security requirement. The handler can then read
`infra_http::DelegationCredential` from request extensions. Record the
authorized downstream audience for that exchange.
<!-- template:end authn:production-bearer-delegation -->
<!-- template:begin grpc-authn:production-rpc-delegation -->
Register an RPC that performs token exchange with
`require_scopes_for_delegation` and its required scopes. Read
`infra_grpc::DelegationCredential` from the Tonic request extensions after
admission.
<!-- template:end grpc-authn:production-rpc-delegation -->

<!-- template:begin oidc-jwt:production-jwt-issuer-contract -->
For each JWT issuer, qualify the access-token profile against the provider's
actual configuration. Verify an access token for this API succeeds and an ID
token from the same issuer is refused on each enabled transport. For a
`resource-server` integration, record how the issuer separates these token
kinds, for example by reserving a distinct audience for the API. Confirm that
each signing key is bound to one algorithm, including during rotation.

Choose `authn.jwks_max_age` from the maximum acceptable delay in observing a
withdrawn key and the provider outage the service must survive. Allow for the
normal refresh interval and acquisition time. Exercise an outage across the
chosen limit, then restore the endpoint and confirm authentication recovers.
<!-- template:end oidc-jwt:production-jwt-issuer-contract -->

<!-- template:begin oidc-introspection:production-introspection-issuer-contract -->
Qualify the introspection endpoint with the credentials assigned to this API.
Confirm a valid access token is admitted and that a token for another resource
and a refresh token are refused, including when sent with the access-token
hint. The hint assists lookup. The issuer must supply enough evidence to
distinguish tokens usable at this resource. If positive caching is enabled,
record the revocation delay the service accepts.
<!-- template:end oidc-introspection:production-introspection-issuer-contract -->

## Operation and recovery

- SLI, SLO, and alert thresholds: Unresolved for service-specific routes and
  objectives ([`env/monitoring/service.rules.yml`](../env/monitoring/service.rules.yml)
  and [Alerting](alerting.md#runbooks) own shipped defaults).
- Rollback authority and mixed-version compatibility window: Unresolved
  ([Railway Deployment Profile](railway-deployment-profile.md#rollback)).
- Platform termination grace: must equal or exceed `http.grace_period` validated
  by `crates/service/src/bootstrap`.
- Reconciliation policy, missing-data disposition, and observed restore proof:
  Unresolved.

Independent stores (PostgreSQL, JetStream, object storage) have no coordinated
snapshot guarantee. Any multi-store recovery runbook must:

1. Fence producers, worker claims, retention owners, and external effects.
2. Restore the verified artifact, compatible configuration, and stores into an
   isolated environment while preserving migration history, sequences, and
   consumer state.
3. Reconcile business identities, applied effects, deduplication records, and
   object digests before lifting the fence.
4. Invalidate pre-restore tokens, operator commands, and receipts, re-inspecting
   live state before resuming writes.
5. Record observed recovery timing and reconciled gaps.

<!-- template:begin outbox:outbox-delivery-rollout -->
### Activating outbox delivery guarantees

Allocate a non-nil UUID once for each independent logical publisher. Persist it with
the publisher's business identity and bind its declared routes with
`PublicationRoute::for_outbox(publisher_id)`. Replicas, restarts and restores
of that publisher reuse the UUID. A separately writable fork or independent
publisher gets its own UUID, including when it shares the database or stream.
Keep each local event ID and occurrence time with its business operation.
The wire ID for new events is `outbox-v1-<32 lowercase UUID hex digits>-<64
lowercase SHA-256 hex digits of the UTF-8 local ID>` in both `Message-Id` and
`Nats-Msg-Id`. Reserve this prefix across all stream publishers, including
direct publishers that supply their own globally unique IDs. Check historical
IDs for overlap before activating the new namespace.

Route resubmissions of pre-upgrade operations by their recorded identity.
Use the original unbound route, event contents, time and order to reconcile
an existing receipt. An unknown unbound event fails with `PublisherRequired`.
New operations use the bound route, including when their local ID also
appears in another publisher's history. Workers retry already committed
intents with their stored headers. DLQ restoration preserves the logical
ID and derives its own publication ID from the retained record.

Set the source stream's retention for the longest promised consumer outage
and recovery interval. Use Limits retention for independent subscriptions.
A WorkQueue stream serves one competing group. Provision capacity and
verify discard and per-subject limits, MaxAge, storage, replication and
sync policy before enabling publishers or changing stream configuration.
Monitor rejection on a full stream and free capacity by the retention policy.
Set a delivery freshness objective for the service and page on the age of
outstanding publications against that objective. Allow for the next retry,
queued work, broker admission and the time needed to drain the backlog after
recovery. The retry delay bounds only the interval until a job is eligible
again.

Fence all producers, workers, cleanup and operator processes before applying
the receipt and retry migrations. A stopped old relay cannot schedule another
multi-day retry after the cutoff. The retry migration shortens long waits
only when the retained attempt and its timestamp identify the previous
automatic schedule. Inspect other long waits before changing their due time.
The receipt backfill uses retained queue rows.
Recover previously pruned identities from authoritative business history or
adopt a disjoint ID namespace and fence historical submissions. Resolve
conflicting historical intents against business records before migration.
Verify failed, pending and completed rows against their receipts, then start
the new cohort. Keep permanent receipts while replay remains possible.
For a stalled ordered aggregate, look up its versions and the queue status
of each original job. Redrive a verified failed predecessor after resolving
its cause. A receipt can outlive a completed job's cleanup.

```sql
SELECT r.aggregate_version, r.job_id, j.state, j.error_summary, j.not_before
FROM outbox_event_receipts AS r
LEFT JOIN background_jobs AS j ON j.id = r.job_id
WHERE r.aggregate_key = $1
ORDER BY r.aggregate_version;
```

An old writer or worker can bypass these guarantees. After new writes, fence
all participants and reconcile identity, aggregate progress and external
effects before permitting a rollback.
<!-- template:end outbox:outbox-delivery-rollout -->
<!-- template:begin jobs:production-contract-jobs-rollout -->
### First HTTP operation that enqueues a job

When a business crate first connects HTTP and jobs, register its operation
and job handler together in the service's composition code. Check the operation
in the generated OpenAPI document, send a command through the shipped HTTP
entry point, and observe the corresponding business effect after the shipped
worker processes that command. With only this handler omitted and another kind
still registered, the missing effect must fail the check even if the worker is
ready.

### Rolling out exact background-job payloads

For a service that uses PostgreSQL jobs, inventory every worker, outbox
publisher, operator and rollback image that reads the jobs table. Before
enabling a producer that writes only `payload_text`, move those readers to a
release that prefers `payload_text` and accepts historical `payload` JSONB.
Include tooling that compares a live duplicate, and exercise a retained failed
job through redrive. If an older JSONB-only reader is still in the rollout or
rollback window, deploy the dual-writing, dual-reading release first.

Apply the forward migration that permits either representation before starting
text-only producers. During overlap, exercise an old producer with a new worker
and a new producer with the oldest supported worker. Roll back the application
to the dual-reading release while keeping the expanded schema. Leave historical
JSONB-only rows in their original format: reconstructing text from JSONB cannot
recover the original encoding and changes how live duplicates are compared.

For an incompatible handler payload, register the new kind under a new name
before switching producers. Keep the old handler available until pending,
running and recoverable failed jobs of the old kind have a disposition.
<!-- template:end jobs:production-contract-jobs-rollout -->
