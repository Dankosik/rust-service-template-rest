# Production Contract

Status: **Unresolved — production promotion is blocked until the owning
service accepts every applicable field.** Use `N/A` only with a concrete
reason. This file becomes service-owned with the first production feature
and is not a template-wide source of deployment defaults; the defaults the
template does fix (probes, budgets, exit codes, image, grace period) are
named where they are enforced and linked from each section.

## Service scope

- Business capabilities: Unresolved.
- Authoritative facts and data owners: Unresolved.
- Public and internal interfaces: Unresolved. The template serves
  `GET /health/live`, `GET /health/ready`, and the operations in
  `api/openapi/service.yaml` on the application listener, and `/metrics` and
  `GET /health/live` on the separate diagnostics listener.

## Dependency contract

| Dependency | Criticality | Readiness | Deadline | Concurrency | Retry owner | Identity | Ambiguous outcome | Recovery |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Unresolved | Unresolved | Unresolved | Unresolved | Unresolved | Unresolved | Unresolved | Unresolved | Unresolved |

A dependency whose loss makes the instance unable to serve registers a
readiness probe; every operation's deadline fits inside
`http.request_timeout` ([Integration Boundaries](architecture/integration.md)).

## Capacity envelope

- Expected and burst arrival rate: Unresolved.
- Service time, payload bounds, and expected concurrency: Unresolved. The
  template caps `http.max_in_flight`, `http.max_connections`,
  `http.max_body_bytes`, and `http.max_header_bytes`
  ([runtime budgets](configuration-source-policy.md#runtime-budget-policy));
  a service raises them from measurements, not defaults.
- First capacity ceiling and required headroom: Unresolved.
- Database connections, worker concurrency, and queue-age objective: N/A for
  the health-only scaffold; reopen when the service retains those
  capabilities.
- Surviving capacity after one failure-domain loss: Unresolved.
- Comparable workload evidence: Unresolved (stage 11 adds the benchmark
  harness).
- Source capacity with cold, expired or unavailable caches, including
  concurrent loaders, waiters, payload storage and application replicas:
  N/A for the health-only scaffold; reopen when cached state is adopted.

## Consistency and durability

- Transaction and read guarantees: Unresolved.
- Asynchronous propagation, replay, deduplication, and retention: Unresolved.
- RPO and RTO per authoritative store and for the recovered service: Unresolved.
- Backup custody, retention, access/key custody and restore-compatible versions:
  Unresolved.
- Deduplication horizon across retries, replay, rollback and restored work:
  Unresolved.
- Cached facts, authoritative source, permitted staleness, TTL and negative
  retention, invalidation after writes, rejection of late fills, and
  coherence across application replicas: N/A for the health-only scaffold;
  reopen when cached state is adopted. Authenticated services also accept
  token-revocation and signing-key-removal lag and name an emergency trust
  removal owner. Library expiry and coalescing do not choose these policies.

## Edge and trust

- TLS termination and trusted proxy topology: Unresolved. The service
  speaks plain HTTP; TLS terminates at the platform edge.
- Gateway retry and fleet rate-limit owners: Unresolved. The service sheds
  with `503` + `Retry-After: 1` above `http.max_in_flight` and has no rate
  limiter.
- Metrics listener reachability: Unresolved. The shipped
  `observability.metrics.addr` binds IPv4 all-interfaces (`0.0.0.0`); deployment keeps it
  private.
- Egress, identity, and authorization authorities: Unresolved. The template
  ships no authentication and no outbound client; a retained authentication
  profile supplies a global bearer default and explicit `security: []` marks a
  public operation.

## Operation and recovery

- SLI, SLO, and alert queries: Unresolved. Available signals: the HTTP
  duration histogram by route template and status, `http_server_shed_requests_total`,
  connection refusals, `readiness_checks_total` by outcome, process and Tokio
  runtime metrics, and the JSON log records with trace and span ids.
- Runbook and manual intervention paths: Unresolved.
- Rollback authority and mixed-version window: Unresolved. A published
  image is rolled back by digest ([Railway Deployment Profile](railway-deployment-profile.md#rollback)).
- Shutdown: the template exits `0` after a clean staged teardown, `3` when a
  stage overran, `1` on startup failure, inside the 45 s grace period
  ([Runtime Lifecycle](architecture/runtime-lifecycle.md#exit-codes)); a
  platform grace shorter than that is a contract violation.
- Reconciliation policy, missing-data disposition and authority to resume writes:
  Unresolved.
- Observed restore proof (candidate/configuration, retained store identities,
  recovery point, elapsed time and reconciled effects): Unresolved. A backup plan,
  startup identity or readiness result alone is not this proof.

Independent stores have no coordinated snapshot guarantee. Older PostgreSQL with
newer broker state can erase dedup history and repeat effects; newer PostgreSQL
with older broker state can lose events already recorded as published. Restored
database references plus overwritten object keys can retrieve the wrong bytes.
Reconciliation cannot universally reconstruct missing data.

The service's recovery runbook must make this sequence concrete:

1. Fence producers, worker claims, writes and external effects, including old
   replicas and retention owners.
2. Restore the selected retained application artifact, compatible configuration
   and independently retained stores into an isolated environment. Preserve
   identity/secret custody and migration history, sequences and consumer state.
3. Reconcile business identities, already-applied effects, deduplication and
   publication state, and object references against expected content digests.
   Resolve missing data or explicitly retain the fence.
4. Invalidate saved tokens, commands and receipts prepared before restoration;
   re-inspect current identities before authorizing any recovery action.
5. Record observed recovery evidence. The service owner admits resumed writes
   only after its compatibility and reconciliation criteria pass; any unresolved
   store or custody mismatch keeps them fenced.

The [persistence guide](architecture/persistence.md) owns PostgreSQL scope.
<!-- template:begin messaging:docs-production-messaging-recovery -->
The [messaging guide](durable-messaging.md#dlq-restore-and-bounds) distinguishes
source/DLQ snapshots and consumer state from DLQ redrive and broker identity custody.
<!-- template:end messaging:docs-production-messaging-recovery -->
<!-- template:begin jobs:docs-production-jobs-recovery -->
Preserve the [jobs upgrade and custody gate](background-jobs.md#upgrade-and-custody),
including replacement of every old seven-day retention owner and invalidation of
pre-restore recovery tokens.
<!-- template:end jobs:docs-production-jobs-recovery -->
<!-- template:begin object-storage:docs-production-object-storage-recovery -->
The [object-storage guide](object-storage.md#integrity) owns latest-key/version
limits and expected content digests.
<!-- template:end object-storage:docs-production-object-storage-recovery -->
<!-- template:begin cache:docs-production-cache-recovery -->
The [cache guide](cache.md#operate-the-server) defaults to invalidation unless the
service explicitly adopts authoritative cache custody.
<!-- template:end cache:docs-production-cache-recovery -->

<!-- template:begin source-template:docs-production-consumer-lifecycle -->
The source template provides a finite synthetic [native recovery rehearsal](consumer-lifecycle-rehearsal.md)
with historical actors, native archives and per-identity reconciliation.
<!-- template:end source-template:docs-production-consumer-lifecycle -->
