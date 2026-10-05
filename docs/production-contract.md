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

## Resource ownership for retained capabilities

When a service retains or adopts a capability, accept its workload and resource
scope alongside the capacity envelope above. These obligations supply no
service-specific limits, fairness classes or measured capacity; unresolved
business choices stay unresolved. The [configuration policy](configuration-source-policy.md),
[integration boundaries](architecture/integration.md),
[runtime lifecycle](architecture/runtime-lifecycle.md) and
[persistence architecture](architecture/persistence.md) own the applicable
runtime rules and links to retained capability guides.

- HTTP ingress admission and its timer end at the handler's response head.
  A feature that streams a response owns body lifetime, slow-reader limits and
  release through actual completion or cancellation; a head limit is not a body
  or RSS bound. Native gRPC uses independent opening and terminal-call counts
  with the same configured value, shared by router clones. Health bypasses
  those business counts; neither transport's admission is a fleet quota.
- With bearer authentication or outbound machine credentials, distinguish
  retained cache entries/bytes, coalesced followers, active provider attempts
  and all callers. Provider concurrency does not bound every waiter. A caller
  outside bounded ingress or jobs owns its own admission. Token-provider
  completion does not release the consuming resource operation: a workload
  class owns capacity through that resource's actual EOF or terminal outcome.
  Carry the original consuming deadline through waits, authentication,
  attempts and backoff; a new stage does not grant a fresh budget.
- With PostgreSQL, retain SQLx's native finite acquisition and pool bound;
  request-scoped acquisition spends the remaining HTTP deadline with its
  existing 100 ms response reserve. Keep transactions short and avoid holding
  a connection across unrelated provider work. Readiness shares the pool and
  may withdraw a saturated replica. A timeout or cancellation at an effect
  boundary does not prove rollback; handle uncertain commit/effect outcomes
  through the persistence contract. Another pool needs an accepted capacity
  reservation, not an assumption that it eliminates waiters.
- With jobs, outbound webhooks or outbox delivery, active worker/per-kind
  capacity is distinct from durable backlog count, bytes and age. Accept
  admission, expiry, replay and tenant/endpoint fairness from the real workload;
  do not infer them from worker concurrency. Expiry cannot erase an unresolved
  accepted obligation. A count-then-insert check alone is not fleet admission.
- Account for peak replicas, rolling-deployment overlap, API and worker pools,
  dedicated LISTEN sessions, migrations/admin reserve and pooler front/back
  connections. Sum provider attempts across independently constructed clients
  and replicas. With messaging, local active/reserved pulls and durable pending
  ACK capacity do not bound total broker backlog or storage; accept effective
  retention, byte/message limits and recovery capacity with the broker owner.
- A service adding CPU-heavy or blocking business work owns admission before
  submission, bounding queued plus running work, and holds capacity until the
  actual work ends. Name cancellation, panic/completion observation and shutdown
  ownership; dropping an async waiter does not stop a started blocking closure.
  The template adds no CPU workload, executor or reserved diagnostics capacity.

## Consistency and durability

- Transaction and read guarantees: Unresolved.
- Asynchronous propagation, replay, deduplication, and retention: Unresolved.
- RPO, RTO, backup owner, and restore proof: Unresolved.
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
- Reconciliation and recovery proof: Unresolved.
