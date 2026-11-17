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
