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
  `api/openapi/service.yaml` on the application listener, and `/metrics` on
  the separate diagnostics listener.

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

## Consistency and durability

- Transaction and read guarantees: Unresolved.
- Asynchronous propagation, replay, deduplication, and retention: Unresolved.
- RPO, RTO, backup owner, and restore proof: Unresolved.

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
  connection refusals, process and Tokio runtime metrics, and the JSON log
  records with trace and span ids.
- Runbook and manual intervention paths: Unresolved.
- Rollback authority and mixed-version window: Unresolved. A published
  image is rolled back by digest ([Railway Deployment Profile](railway-deployment-profile.md#rollback)).
- Shutdown: the template exits `0` after a clean staged teardown, `3` when a
  stage overran, `1` on startup failure, inside the 45 s grace period
  ([Runtime Lifecycle](architecture/runtime-lifecycle.md#exit-codes)); a
  platform grace shorter than that is a contract violation.
- Reconciliation and recovery proof: Unresolved.
