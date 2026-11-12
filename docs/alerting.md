# Alerting

[`env/monitoring/service.rules.yml`](../env/monitoring/service.rules.yml) gives
an on-call operator Prometheus recording and alert rules over the metrics the
service and `jobs-worker` already emit. Pages fire on symptoms a client sees,
an error or latency budget burning or the service gone; tickets cover lost
capacity, crash loops and work that stopped draining. Each alert carries a
`severity` (`page` or `ticket`) and a `runbook_url` to its section below.
[`scripts/tests/service-rules.yml`](../scripts/tests/service-rules.yml) proves
every alert fires on its fault and stays silent otherwise; `make
monitoring-rules` runs it with the pinned `promtool`. Rules for a profile ship
only with that profile.

## Install

1. Load the file through `rule_files` or a `PrometheusRule`, next to
   `env/monitoring/postgres-maintenance.rules.yml` when the PostgreSQL profile is kept.
2. Replace the scrape-job selector `job=~"service|jobs-worker"`, and the
   `absent(up{job="service"})` dead-man check, with your scrape job names:
   `sd 'service\|jobs-worker' 'orders-api|orders-worker' env/monitoring/service.rules.yml`.
3. Point `runbook_url` at this page in your repository.
4. Route `severity="page"` to the pager and `severity="ticket"` to the queue.
   Burn-rate alerts share their name across windows and differ by
   `long_window`, so inhibit the slower window while a faster one fires:

```yaml
inhibit_rules:
  - source_matchers: [severity="page"]
    target_matchers: [severity="ticket"]
    equal: [alertname, job]
```

## Objectives

The template's defaults; change them in the expressions, not in a separate
configuration surface.

| SLI | Good event | Objective (budget) |
| --- | --- | --- |
| HTTP availability | a response below 500; `/health/live` and `/health/ready` are not traffic | 99.9% (0.001) |
| HTTP latency | a response within 500 ms (`le="0.5"`) | 99% (0.01) |
<!-- template:begin grpc:docs-alerting-grpc-objectives -->
| gRPC availability | any status except `Unknown`, `DeadlineExceeded`, `ResourceExhausted`, `Internal`, `Unavailable`, `DataLoss`; health and reflection services excluded | 99.9% (0.001) |
| gRPC latency | a call handled within 500 ms | 99% (0.01) |
<!-- template:end grpc:docs-alerting-grpc-objectives -->

The service sheds load with `503` over HTTP and `RESOURCE_EXHAUSTED` over gRPC;
both count against availability, because the client did not get an answer.
Client errors do not.

Each SLO alerts on the four multi-window, multi-burn-rate pairs of the
[SRE workbook](https://sre.google/workbook/alerting-on-slos/): an alert fires
when both the long and the short window burn faster than the factor, so it
starts within minutes of a fault and stops soon after recovery.

| Severity | Long window | Short window | Burn factor | `for` | Budget spent when it fires |
| --- | --- | --- | --- | --- | --- |
| page | 1h | 5m | 14.4 | 2m | 2% of 30 days |
| page | 6h | 30m | 6 | 15m | 5% |
| ticket | 1d | 2h | 3 | 1h | 10% |
| ticket | 3d | 6h | 1 | 3h | 10% |

## Tuning

- **Objective.** The budget is `1 - objective`. Moving HTTP availability to
  99.5% replaces `0.001` with `0.005` in the four `HTTPErrorBudgetBurn` rules.
- **Latency threshold.** It must be a histogram bucket boundary: no series
  matches any other `le`, so the ratio is empty and the alert never fires. HTTP buckets are
  1, 2.5, 5, 10, 25, 50, 100, 250, 500 ms and 1, 2.5, 5, 10 s; gRPC buckets
  are the Prometheus defaults. Change `le="0.5"` in both the recording rules
  and the summary.
- **Excluded traffic.** Add routes that are not user traffic (an admin
  export, a long poll) to the `http_route!~` matcher. Streaming gRPC methods
  hold a call open for their whole lifetime; exclude them from the latency SLI
  with `grpc_method!~`.
- **Low traffic.** At a few requests an hour one failure is a large ratio.
  Below about one request a minute, prefer the ticket windows only, or add
  `and sum by(job) (rate(...[1h])) > 1` to the page rules.
- **Labels.** Ratios aggregate `by(job)`. Per-route objectives multiply the
  series by the route count; add `http_route` to the `by` clause only for the
  few routes that carry their own objective.

The burn rules record `job:slo_<protocol>_<errors|slow>:ratio_rate<window>`
for 5m, 30m, 1h, 2h, 6h, 1d and 3d. A dashboard can chart those directly
together with `http_server_shed_requests_total`,
`http_server_active_requests`, `runtime_scheduler_lag_seconds` and the pool
gauges. The template ships no dashboard: a JSON model is tied to one Grafana
version and datasource, while these series answer the same questions.

## Runbooks

### HTTPErrorBudgetBurn

Clients receive 5xx answers faster than the objective allows. In the
`http_request` log, `termination="load_shed"` identifies a 503 sent after the
request waited `http.request_timeout` for a permit. Check
`http_server_shed_requests_total` and `http_server_admission_wait_seconds`
for the frequency and duration of those waits. For an admitted webhook with
`problem_code="service_unavailable"`, correlate by `request_id` with the
`webhook_receipt_unavailable` warning to find the failed receipt stage.

`problem_code="gateway_timeout"` identifies an exhausted request budget.
A caller that stopped sending its body gets `408 request_timeout` and
does not count here. An access log with `termination="server_shutdown"`
records a forced shutdown before the response head as 503 for the SLO.
The client received no HTTP status. Correlate by `http_route`.

```promql
sum by(http_route, http_response_status_code) (
  rate(http_server_request_duration_seconds_count{http_response_status_code=~"5.."}[5m]))
```

Roll back a fresh deployment first; add replicas for shedding; for a failed
dependency, follow its own alert.

### HTTPLatencyBudgetBurn

Too many requests take longer than 500 ms. Find the route with
`histogram_quantile(0.99, sum by(http_route, le) (rate(http_server_request_duration_seconds_bucket[5m])))`,
then check what that route waits on: pool waits (`PostgresPoolSaturated`),
outbound calls (`http_client_request_duration_seconds`) and
`runtime_scheduler_lag_seconds`, which rises when the runtime itself is
starved of CPU.

<!-- template:begin grpc:docs-alerting-grpc-runbooks -->
### GRPCErrorBudgetBurn

Calls end with a server-fault status. Break them down with
`sum by(grpc_service, grpc_method, grpc_code) (rate(grpc_server_handled_total[5m]))`
and by catalog code with `grpc_server_failures_total`. `ResourceExhausted`
together with `grpc_server_shed_requests_total` is load shedding;
`DeadlineExceeded` is the call deadline. Act as for `HTTPErrorBudgetBurn`.

### GRPCLatencyBudgetBurn

Calls take longer than 500 ms. Locate the method with
`grpc_server_handling_seconds_bucket` by `grpc_method`, then follow
`HTTPLatencyBudgetBurn` for what it waits on. A call over `grpc.max_in_flight`
waits for a permit, at most `grpc.admission_wait`, before it is served or
refused, and that wait is part of its handling time;
`grpc_server_admission_wait_seconds` shows how much and
`grpc_server_admission_waiting` how many calls wait now.
<!-- template:end grpc:docs-alerting-grpc-runbooks -->

### ServiceDown

No target of the job answers a scrape, or service discovery lost the service.
Every other alert for it is now blind. Check the platform's deployment and
restart history, then the process logs from its last start for a startup
refusal: invalid configuration and an unreachable required dependency both
stop the process before it serves.

### ServiceTargetDown

One instance has not answered a scrape for 15 minutes while the job still runs
elsewhere. Capacity is reduced; clients are fine unless the SLO alerts fire.
Check whether the instance is wedged (no logs) or the metrics listener alone is
unreachable (network policy, port).

### ServiceRestarting

`process_start_time_seconds` changed at least three times in 30 minutes: a
crash loop. Read the logs just before each restart; a panic, an
out-of-memory kill or a failed liveness probe each look different there.

### ServiceNotReady

A running instance has withheld readiness for 15 minutes, so the platform
routes no traffic to it. `readiness_probe_checks_total{outcome!="ok"}` names
the failing probe, and the `readiness_lost` log event carries the first cause.
Fix that dependency; restarting the instance does not.

### ServiceReadinessFlapping

Readiness changed at least four times in 30 minutes. A dependency fails
intermittently around the readiness threshold. Look at
`readiness_probe_checks_total` by `probe` and `outcome`, and at the dependency's
own latency against the probe timeout.

<!-- template:begin postgres:docs-alerting-postgres-runbooks -->
### PostgresPoolSaturated

More than 5% of transactions waited over 100 ms for a pooled connection for 10
minutes. Requests queue behind the pool, and latency alerts usually follow.
Compare `db_client_connection_count{db_client_connection_state="used"}` with
`db_client_connection_max`, and `postgres_transaction_duration_seconds` with
its usual level: long transactions hold connections, more traffic needs more
of them. Raise the pool only within the database's connection budget.
<!-- template:end postgres:docs-alerting-postgres-runbooks -->

<!-- template:begin jobs:docs-alerting-jobs-runbooks -->
### JobsQueueBacklog

The oldest available job of a kind has waited over five minutes for a worker.
Work for that kind is late. Check that workers run and claim
(`jobs_attempts_total` rate, `jobs_worker_operation_failures_total`), then
whether attempts of that kind got slower (`jobs_attempt_duration_seconds`).
Add workers when claims keep up with no failures but the age still grows.

### JobsObservationStale

A worker has published no successful queue sample for over two minutes, or
none since it started. Its queue gauges keep their last values, so
`JobsQueueBacklog` cannot see a backlog that grows there. Check
`jobs_worker_operation_failures_total{operation="sample"}` and the
`jobs_operation_failed` log for its `sqlstate` or `cause`; the sample has a
two-second statement timeout.

### JobsDeadLettered

Jobs failed permanently or exhausted their attempts, and stay failed until an
operator acts. Read the failure in the `jobs-worker` logs, fix the cause, then
inspect and recover retained jobs with the operator commands `jobs-worker --help`
lists (`failed`, `inspect`, `redrive`, `discard`).
<!-- template:end jobs:docs-alerting-jobs-runbooks -->

<!-- template:begin messaging:docs-alerting-messaging-runbooks -->
### MessagingDeadLettered

The adapter attempted to move an event to the dead-letter stream. A
`rejected` publication leaves the event in the source stream for redelivery.
An `ambiguous` publication also leaves it for redelivery, and the DLQ may
already contain a copy. Fix the handler or the producer, then recover the
exact DLQ records with the [`dlq_recovery` example](../crates/infra-messaging/examples/dlq_recovery.rs).
After lowering a source limit, older records can still need the previous DLQ
capacity. If the consumer stops with a dead-letter size error, raise the DLQ
or broker limit to fit the indicated source record and restart the consumer.
If the record exceeds the retained-message pull limit, resolve it in the
source stream before restarting.
For malformed source messages, the transfer keeps the original payload and
omits any retained identity or trace field that would exceed the source header
budget. A DLQ record can therefore be inspectable even when it cannot be redriven.

For wildcard consumers, keep source subjects within 4 KiB across publishers,
subject mappings and stream imports. The adapter sizes pulls and DLQ
transfers for the broker's default `max_control_line` and any longer
registered subjects selected by the filter. Use a concrete filter when
consuming longer subjects outside that registry.
<!-- template:end messaging:docs-alerting-messaging-runbooks -->
