# Alerting

[`env/monitoring/service.rules.yml`](../env/monitoring/service.rules.yml) gives
an on-call operator Prometheus recording and alert rules over the metrics the
service and `jobs-worker` already emit. Pages fire on symptoms a client sees,
an error or latency budget burning or the service gone; tickets cover lost
capacity, crash loops, a starved runtime and work that stopped draining. Each alert carries a
`severity` (`page` or `ticket`) and a `runbook_url` to its section below.
[`env/monitoring/tests/service-rules.yml`](../env/monitoring/tests/service-rules.yml) proves
every alert fires on its fault and stays silent otherwise; `make
monitoring-rules` (`scripts/checks/monitoring_rules.sh`) runs it with the
pinned `promtool`. Rules for a profile ship
only with that profile.

## Install

1. Set the scrape job names in both the rules and their fixtures. For an
   `orders-api` service with an `orders-worker`, run these commands from the
   repository root.

   ```sh
   sd -F 'job=~"service|jobs-worker"' 'job=~"orders-api|orders-worker"' env/monitoring/service.rules.yml
   sd -F 'job="service"' 'job="orders-api"' env/monitoring/service.rules.yml env/monitoring/tests/service-rules.yml
   sd -F 'job="jobs-worker"' 'job="orders-worker"' env/monitoring/tests/service-rules.yml
   ```

   If the worker is required, append `or absent(up{job="orders-worker"})`
   to the `ServiceDown` expression. Give the normal fixtures a reachable
   worker and add a case with only the API present. Use one exact selector
   for each job that must stay in discovery.
2. When installing `env/monitoring/postgres-maintenance.rules.yml`, first
   replace its example `pg:expected` rows with the actual database, scrape job,
   instance and maintenance population for each expected writer. Give the
   scrape targets the matching `database` label. Set each
   `pg:writer_set_changed_timestamp_seconds` to the Unix time in seconds of
   the last writer-set change.
3. Point `runbook_url` at this page in your repository, both in the rules
   and in the fixtures' expected annotations. Check the configured rules
   with `make monitoring-rules`, then load them through `rule_files` or a
   `PrometheusRule`.
4. Route `severity="page"` to the pager and `severity="ticket"` to the queue.
   This Alertmanager example suppresses a ticket while the same SLO is paging
   for the same job. It also suppresses the slower of two active pages or two
   active tickets.

```yaml
inhibit_rules:
  - source_matchers: [severity="page"]
    target_matchers: [severity="ticket"]
    equal: [alertname, job]
  - source_matchers: [severity="page", long_window="1h"]
    target_matchers: [severity="page", long_window="6h"]
    equal: [alertname, job]
  - source_matchers: [severity="ticket", long_window="1d"]
    target_matchers: [severity="ticket", long_window="3d"]
    equal: [alertname, job]
```

## Objectives

[`env/monitoring/service.rules.yml`](../env/monitoring/service.rules.yml) is the
source of truth for exact SLI ratios, thresholds, windows, and `for` durations;
tune objectives directly in those rule expressions rather than introducing a
second configuration surface:

- **HTTP availability & latency:** Non-5xx responses (excluding `/health/live`
  and `/health/ready`) and responses within the latency bucket threshold.
<!-- template:begin grpc:docs-alerting-grpc-objectives -->
- **gRPC availability & latency:** Calls not ending in server-fault statuses
  (`Unknown`, `DeadlineExceeded`, `ResourceExhausted`, `Internal`,
  `Unavailable`, `DataLoss`), excluding health and reflection services, and
  calls handled within the latency bucket threshold.
<!-- template:end grpc:docs-alerting-grpc-objectives -->

Load-shedding replies (`503` over HTTP, `RESOURCE_EXHAUSTED` over gRPC) count
against availability because the client received no answer; client errors do
not. Each SLO pairs fast and slow multi-window burn-rate rules from the
[SRE workbook](https://sre.google/workbook/alerting-on-slos/) so pages fire
within minutes of a fault and reset promptly after recovery.

## Tuning

- **Objective.** The error budget is `1 - objective`; update the budget factor in
  all four burn-rate rules for that SLI together.
- **Latency threshold.** Must match an emitted histogram bucket boundary (`le`)
  defined in `crates/infra-http` (`HISTOGRAMS`) or the Prometheus recorder; any
  other `le` matches no series and silences the alert. Update `le` in both the
  recording rules and alert annotations.
- **Excluded traffic.** Exclude non-interactive routes (exports, long polls) via
  `http_route!~`. The gRPC latency SLI counts only `grpc_type="unary"`, because a
  streaming call stays open for its whole lifetime; exclude a slow-by-design
  unary method via `grpc_method!~`.
- **Low traffic.** To page only above one request per minute, append
  `and (sum by(job) (rate(http_server_request_duration_seconds_count{job=~"service|jobs-worker",http_route!~"/health/live|/health/ready"}[1h])) > 1/60)`
  to the HTTP page expression. Use the same job and route selectors as the
  SLI denominator. Prometheus reports the rate in requests per second.
- **Labels.** Recording rules aggregate `by(job)`; add `http_route` only for
  routes that genuinely require an independent SLO. The template ships no
  Grafana JSON model because series in `service.rules.yml` (`job:slo_*`,
  `http_server_shed_requests_total`, `http_server_active_requests`,
  `runtime_scheduler_lag_seconds`, and pool gauges) plot directly across Grafana
  versions.

## Runbooks

### HTTPErrorBudgetBurn

Clients receive 5xx answers faster than the objective allows. In the
`http_request` log, `termination="load_shed"` identifies a 503 sent after the
request waited `http.admission_wait` for a permit. Check
`http_server_shed_requests_total` and `http_server_admission_wait_seconds`
for the frequency and duration of those waits. For an admitted webhook with
`problem_code="service_unavailable"`, correlate by `request_id` with the
`webhook_receipt_unavailable` warning to find the failed receipt stage.

`problem_code="gateway_timeout"` identifies an exhausted request budget.
Expiry during a tracked request-body read returns `408 request_timeout` and
does not count here. An access log with `termination="server_shutdown"`
records a forced shutdown before the response head as 503 for the SLO.
The client received no HTTP status. Match the metric's `http_route` to
`route` in the access log for the affected job, then follow `request_id`.

```promql
sum by(job, http_route, http_response_status_code) (
  rate(http_server_request_duration_seconds_count{http_route!~"/health/live|/health/ready",http_response_status_code=~"5.."}[5m]))
```

For shedding, check capacity. For `authentication_unavailable`, group
`authn_token_verifications_total{outcome="failure"}` by `mode` and `reason`.
For provider failures, inspect `authn_provider_request_duration_seconds_count`
by `operation` and `outcome`.
For a route using object storage, look for `object_storage_operation_failed`
with its `error.type` and provider request ID. Compare the start of errors
with the deployment history.

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

Unary calls take longer than 500 ms. Locate the method with
`grpc_server_handling_seconds_bucket{grpc_type="unary"}` by `grpc_method`, then follow
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

One instance has not answered a scrape for 15 minutes. Check `up` for the
other instances of that job and look for a concurrent `ServiceDown` page to
determine whether any scrape targets are reachable.
Check whether the instance is wedged (no logs) or the metrics listener alone is
unreachable (network policy, port).

### ServiceRestarting

`process_start_time_seconds` changed at least three times in 30 minutes: a
crash loop. Read the logs just before each restart; a panic, an
out-of-memory kill or a failed liveness probe each look different there.

### ServiceNotReady

The instance has reported unready on every scrape for 15 minutes. Check
whether it is draining. If `readiness_checks_total` has stopped advancing,
the last check can expire even while the diagnostics listener answers.
Inspect runtime progress and look for `readiness_refresh_late` when checks
resume. If checks continue, use `readiness_probe_checks_total` by probe and
outcome and the `readiness_lost` log event to identify the failing dependency.

### ServiceReadinessFlapping

Scrapes observed at least four readiness changes in 30 minutes. Compare the
timing with `readiness_refresh_late` events for gaps between checks. For
repeated probe failures, compare the dependency's latency with the probe
timeout and inspect `readiness_probe_checks_total` by probe and outcome.

### RuntimeSchedulerLagging

More than 1% of the 100 ms runtime progress samples ran over 100 ms late for
15 minutes: tasks wait that long for a worker, and request latency follows.
`tokio_busy_ratio / on(job,instance) tokio_workers_count` gives the mean
fraction of worker time spent busy. A value near 1 during the lag indicates
little idle worker time. Compare with CPU use and container throttling
before changing replica or thread counts. With idle worker time, inspect
blocking calls and scheduling delays.
`runtime_scheduler_lag_max_seconds` gives the longest single stall.

A build with `RUSTFLAGS="--cfg tokio_unstable"` adds per-poll and queue detail
(`tokio_mean_poll_duration`, `tokio_budget_forced_yield_count`,
`tokio_blocking_queue_depth` and 31 more names) for under 1% CPU per request on
a DigitalOcean c-4. The default build leaves it off because Tokio exempts that
API from its compatibility guarantee.

### RuntimeThreadsElevated

A process has held over 64 threads beyond its runtime workers for 15 minutes.
Blocking-pool threads exit after 10 idle seconds, so that many are busy at
once: DNS lookups against a resolver that stopped answering, file reads on a
stalled volume, or blocking work a handler submits faster than it finishes.
Find what the blocked threads wait on before raising any limit; the pool stops
growing at 512 threads and queues without bound after that.

### OpenFilesNearLimit

A process uses over 80% of its open-file limit (`process_open_fds` against
`process_max_fds`); past the limit every accept and outbound connect fails with
`EMFILE`. The service raises its soft limit to the hard limit at startup, so
the limit is the platform's. Compare against `http.max_connections` and the
pool sizes: a count that grows without matching traffic is a descriptor leak.

### MemoryNearLimit

A process holds over 85% of its control-group memory limit
(`process_resident_memory_bytes` against `process_memory_limit_bytes`, read
once at startup and absent when the group has no limit). Past the limit the
kernel kills the process: the exit status is 137 and the last log line is an
ordinary one, so this alert is the only warning. The limits multiply into memory: measured on
the scaffold, an idle connection costs about 32 KiB (`http.max_connections`
4096 is 128 MB), a 0.9 MiB JSON request about 1–1.8 MB while in flight (256
connections peaked at 288 MB). Lower `http.max_body_bytes`,
`http.max_in_flight` or `http.max_connections` to fit the limit, or raise the
limit; steady growth at constant traffic is a leak.

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
`jobs_operation_failed` log for its `sqlstate` or `cause`.

### JobsDeadLettered

Jobs failed permanently or exhausted their attempts, and stay failed until an
operator acts. Read the failure in the `jobs-worker` logs, fix the cause, then
inspect and recover retained jobs with the operator commands `jobs-worker --help`
lists (`failed`, `inspect`, `redrive`, `discard`).
<!-- template:end jobs:docs-alerting-jobs-runbooks -->

<!-- template:begin messaging:docs-alerting-messaging-runbooks -->
### Changing a durable subscription

All replicas sharing a stream and durable name must agree on the handled
subjects, event types and schema versions, the DLQ stream and subject, and
the broker's filter and delivery settings. Change these together.

1. Generate the target metadata offline with
   `registry.durable_metadata(dlq_stream, dlq_subject)` from the target
   binary's registered handlers and resolved DLQ names. Deployments using
   other languages can calculate the same value using the encoding below.
   On an existing durable a refused admission also logs `expected_contract`.
2. Stop and drain **all** workers sharing the durable before changing it,
   including older binaries that ignore the contract metadata. Save the
   consumer configuration, pending deliveries and ACK floor. Decide how to
   handle records entering or leaving the subscription before changing
   publishers.
3. With the broker administration account, update the **existing** durable
   in place, merging the generated metadata entry into its configuration.
   This step also applies on the first rollout to a durable without the
   entry. Preserve unrelated metadata, the delivery cursor and broker limits.
   For a newly named durable the first compatible worker can create it;
   choose its replay history explicitly.
4. Verify the stored settings and ACK floor, start the target workers, then
   verify progress and DLQ disposition before enabling the new publications.
   For rollback, drain all target workers before restoring the saved
   configuration and starting compatible binaries. Account for records
   already acknowledged or transferred and the effects of their handlers.

The metadata key is `service.delivery-contract.v1`. Its value is `sha256-`
followed by the lowercase hexadecimal SHA-256 digest. Supply the following
bytes in order:

- The DLQ stream and DLQ subject, each encoded as an unsigned 64-bit
  big-endian UTF-8 byte length followed by those bytes.
- Each handled `(subject, event_type, schema_version)` in ascending tuple
  order (UTF-8 byte order for strings, numeric order for versions). Encode
  each string as above, followed by the unsigned 16-bit big-endian version.
  Exclude routes without handlers. This contract describes route ownership;
  coordinate changes in handler semantics under the same event version
  through the service's rollout policy.

### MessagingDeadLettered

The adapter attempted to move an event to the dead-letter stream. A
`rejected` publication leaves the event in the source stream for redelivery.
An `ambiguous` publication also leaves it for redelivery, and the DLQ may
already contain a copy. Fix the handler or the producer. Use `inspect` in
the [`dlq_recovery` example](../crates/infra-messaging/examples/dlq_recovery.rs)
to read an exact DLQ record without changing the broker. Redrive and
retirement are available through the recovery controller for its own
isolated broker sessions. Use your deployment's recovery procedure for
other brokers.

When independent durable subscriptions need separate incident histories,
provision a separate DLQ stream for each. A transfer ID identifies the source
record. If two subscriptions reject that record within the same stream's
duplicate window, the second transfer can receive a duplicate acknowledgement
even when the subscriptions publish to different subjects in that stream.

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
