# Alerting

[`env/monitoring/service.rules.yml`](../env/monitoring/service.rules.yml) gives
an on-call operator Prometheus recording and alert rules over the metrics the
service and `jobs-worker` already emit. Pages fire on symptoms a client sees,
an error or latency budget burning or the service gone; tickets cover lost
capacity, crash loops, a starved runtime and work that stopped draining. Each alert carries a
`severity` (`page` or `ticket`) and a `runbook_url` to its section below.
[`env/monitoring/tests/service-rules.yml`](../env/monitoring/tests/service-rules.yml) proves
every alert fires on its fault and stays silent otherwise; `just
monitoring-rules` runs it with the pinned `promtool` from `mise.toml`. Rules for a profile ship
only with that profile.

## Install

<!-- template:begin postgres:docs-alerting-maintenance-inventory -->
Before loading `env/monitoring/postgres-maintenance.rules.yml`, replace
its example `pg:expected` rows with the actual database, scrape job,
instance and population for each expected writer. Add the matching
`database` label to the scrape targets. Set each
`pg:writer_set_changed_timestamp_seconds` to the Unix time in seconds of
the last change to that database's writers for the population.
<!-- template:end postgres:docs-alerting-maintenance-inventory -->

1. Update the shared scrape-job selector and the exact API job selector in
   `ServiceDown` and `ServiceNoReadyTargets`. For example, if your scrape jobs are named `orders-api`
   and `orders-worker`, run these from the repository root.

   ```sh
   sd -F 'job=~"service|jobs-worker"' 'job=~"orders-api|orders-worker"' env/monitoring/service.rules.yml
   sd -F 'job="service"' 'job="orders-api"' env/monitoring/service.rules.yml env/monitoring/tests/service-rules.yml
   sd -F 'job="jobs-worker"' 'job="orders-worker"' env/monitoring/tests/service-rules.yml
   ```
   <!-- template:begin temporal:docs-alerting-temporal-install -->

   When the Temporal profile is retained, also replace the Temporal worker job
   selector (`temporal-worker`) and the combined client selector
   (`service|temporal-worker`), for example with `orders-temporal-worker`:

   ```sh
   sd -F 'job="temporal-worker"' 'job="orders-temporal-worker"' env/monitoring/service.rules.yml env/monitoring/tests/service-rules.yml
   sd -F 'job=~"service|temporal-worker"' 'job=~"orders-api|orders-temporal-worker"' env/monitoring/service.rules.yml
   ```
   <!-- template:end temporal:docs-alerting-temporal-install -->

   For an API-only installation, use `job=~"orders-api"` as the first
   replacement. If the worker must always be in service discovery, add
   `or absent(up{job="orders-worker"})` to the `ServiceDown` expression.
   In that case include a reachable worker in normal fixtures and cover its
   disappearance with only the API present.
2. Point `runbook_url` at this page in your repository.
3. Check the configured files with `just monitoring-rules`, then load them
   through `rule_files` or a `PrometheusRule`.
4. Route `severity="page"` to the pager and `severity="ticket"` to the queue.
   The example inhibits burn-rate tickets while a page for the same alert
   and job fires. Within each severity, it inhibits the 6-hour page while the
   1-hour page fires and the 3-day ticket while the 1-day ticket fires.
   If jobs share a name across environments in one Alertmanager, include
   the environment labels in `equal`.

   ```yaml
   inhibit_rules:
     - source_matchers: ['severity="page"', 'long_window=~"1h|6h"']
       target_matchers: ['severity="ticket"', 'long_window=~"1d|3d"']
       equal: [alertname, job]
     - source_matchers: ['severity="page"', 'long_window="1h"']
       target_matchers: ['severity="page"', 'long_window="6h"']
       equal: [alertname, job]
     - source_matchers: ['severity="ticket"', 'long_window="1d"']
       target_matchers: ['severity="ticket"', 'long_window="3d"']
       equal: [alertname, job]
   ```

## Objectives

[`env/monitoring/service.rules.yml`](../env/monitoring/service.rules.yml) is the
source of truth for exact SLI ratios, thresholds, windows, and `for` durations;
tune objectives directly in those rule expressions rather than introducing a
second configuration surface:

- **HTTP availability and latency.** Availability counts server errors, failed
  response bodies and forced shutdowns. The latency objective counts bodies
  completed locally within the bucket threshold. An interrupted body misses
  this objective even if its response headers arrived promptly. Both measures
  exclude probes. A client disconnect alone does not burn the availability budget.
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
- **Low traffic.** For jobs averaging fewer than one request per minute, use
  ticket windows or require a minimum request rate for pages. Append this
  gate to each HTTP page expression to require more than one request per
  minute on average over the last hour. Keep the metric and selectors
  aligned with the SLI denominator, including any scrape job or route changes.

  ```promql
  and on(job) (
    sum by(job) (
      rate(instance:slo_http_requests:sum[1h])
    ) > (1 / 60)
  )
  ```

- **Rule rollout.** Load the per-instance recording rules alongside the existing
  alerts before switching the burn-rate expressions. Allow up to three days
  for the longest window to fill. Scrape `process_start_time_seconds` with the
  request metrics from each target. The rules use it to distinguish replacements
  at the same address before calculating rates. The intermediate counters
  provide a zero baseline once eligible traffic has
  been observed, so a new error series contributes its first failure. Work
  completed before the first observation of a process has no measured baseline.
- **Labels.** Burn-rate rules aggregate `by(job)`; add `http_route` only for
  routes that genuinely require an independent SLO. The template ships no
  Grafana JSON model because series in `service.rules.yml` (`job:slo_*`,
  `http_server_shed_requests_total`, `http_server_active_requests`,
  `runtime_scheduler_lag_seconds`, and pool gauges) plot directly across Grafana
  versions.

## Runbooks

<!-- template:begin worker:docs-alerting-dead-letter-recency -->
When upgrading, deploy the worker version that emits the event timestamps
to all replicas before replacing the dead-letter rules. Keep the previous
rules active during the worker rollout.

The dead-letter alerts use the last event's Unix timestamp to determine
recency. The first scrape after a recent event can open a ticket even when
Prometheus has no earlier sample for that series. When a counter for an old
event reappears after a scrape gap, its event timestamp keeps it outside
the alert window.

<!-- template:end worker:docs-alerting-dead-letter-recency -->
### HTTPErrorBudgetBurn

The service is returning server errors or failing to finish response bodies.
Group the failures by route, status and cause for the alerting job. Replace `service` with the
alert's `job` label.

```promql
sum by(http_route, http_response_status_code, error_type) (
  rate(http_server_request_duration_seconds_count{job="service",http_route!~"/health/live|/health/ready",http_response_status_code=~"5.."}[5m])
  or rate(http_server_request_duration_seconds_count{job="service",http_route!~"/health/live|/health/ready",error_type=~"server_shutdown|response_body_error"}[5m])
)
```

Match the metric's `http_route` to the `route` field in the `http_request`
access log. Use `request_id` to find related log events for that request.
`termination="load_shed"` identifies a 503 sent after the request waited
`http.admission_wait` for a permit. Check `http_server_shed_requests_total`
and `http_server_admission_wait_seconds` for the frequency and duration
of those waits.

`problem_code="gateway_timeout"` identifies an exhausted request budget.
Expiry during a tracked request-body read returns `408 request_timeout` and
does not count here. A forced shutdown before headers records
`error_type="server_shutdown"` without an HTTP status label. After headers,
body failures retain the response status. Look for `response_body_error` even
when that status is 200.

Roll back a fresh deployment first. Add replicas for load shedding. If the
request failed on a dependency, compare the adapter's signals for the
affected instances during the same period.

<!-- template:begin postgres:docs-alerting-http-postgres -->
- For PostgreSQL, compare pool waits with `PostgresPoolSaturated` and inspect
  `readiness_probe_checks_total{probe="postgres"}` by `outcome` for probe
  failures or timeouts.
<!-- template:end postgres:docs-alerting-http-postgres -->
<!-- template:begin inbound-webhooks:docs-alerting-http-inbound-webhooks -->
- For an admitted webhook with `problem_code="service_unavailable"`, find
  the `webhook_receipt_unavailable` warning by `request_id` to identify the
  failed receipt stage.
<!-- template:end inbound-webhooks:docs-alerting-http-inbound-webhooks -->
<!-- template:begin authn:docs-alerting-http-authentication -->
- For `problem_code="authentication_unavailable"`, break down
  `authn_token_verifications_total{outcome="failure"}` by `mode` and `reason`.
  If verification failed on a provider call, check
  `authn_provider_request_duration_seconds_count` by `operation` and `outcome`.
<!-- template:end authn:docs-alerting-http-authentication -->
<!-- template:begin outbound-auth:docs-alerting-http-outbound-auth -->
- For an outbound OAuth token acquisition, check
  `oauth2_token_acquisitions_total` by `grant` and `outcome` to distinguish
  successful acquisitions from failures and cancellations.
<!-- template:end outbound-auth:docs-alerting-http-outbound-auth -->
<!-- template:begin object-storage:docs-alerting-http-object-storage -->
- For S3, check `object_storage_operation_duration_seconds_count` by
  `operation` and `outcome`. Look for `object_storage_operation_failed`
  and its `error.type` in the related logs.
<!-- template:end object-storage:docs-alerting-http-object-storage -->
<!-- template:begin temporal:docs-alerting-http-temporal -->
- For Temporal, break down `temporal_request_failure` and
  `temporal_long_request_failure` by `namespace`, `operation`, and
  `status_code`, and inspect `readiness_probe_checks_total{probe="temporal"}`
  when `temporal.readiness_critical` is enabled.
<!-- template:end temporal:docs-alerting-http-temporal -->

### HTTPLatencyBudgetBurn

Too many responses failed to complete within 500 ms. Check body errors and
disconnects alongside the latency histogram. Find slow routes with
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
Probe a representative API operation through the public ingress independently.
If it succeeds, investigate the scrape network, discovery and diagnostics listener. Check the platform's deployment and
restart history, then the process logs from its last start for a startup
refusal: invalid configuration and an unreachable required dependency both
stop the process before it serves.

### ServiceNoReadyTargets

Every reachable API replica has reported unready for five minutes. The ingress
may have removed all backends even while Prometheus continues scraping them.
Check the deployment endpoints and a client-facing request, then inspect the
failing readiness probes and the last rollout. Restore at least one ready
replica. A missing readiness sample alone cannot confirm this condition.

### ServiceTargetDown

One instance has not answered a scrape for 15 minutes. Check
`sum by(job) (up)` for the alert's job and whether `ServiceDown` is active
for that same job. If no target for the job answers, follow the
[`ServiceDown` runbook](#servicedown). If another target answers, check its
readiness and the SLO alerts to assess client impact. Check the affected
instance's process logs and whether its metrics listener is reachable on
the configured port.

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
15 minutes. Check `runtime_scheduler_lag_max_seconds` for the longest
single stall and compare it with worker occupancy for the affected instance.

```promql
tokio_busy_ratio / on(job, instance) tokio_workers_count
```

`tokio_busy_ratio` expresses total busy time in worker equivalents. A value
of 1 with four workers means 25% average occupancy. A normalized value near
1 means the workers spent most of the interval busy. Busy time includes
time spent blocked inside a task. Compare process CPU usage and container
CPU throttling with the lag before deciding whether to add CPU capacity or
investigate blocked or long-running tasks. Low average occupancy can hide a
stall on one worker.

A build with `RUSTFLAGS="--cfg tokio_unstable"` adds per-poll and queue detail
(`tokio_mean_poll_duration`, `tokio_budget_forced_yield_count`,
`tokio_blocking_queue_depth` and 31 more names) for under 1% CPU per request on
a DigitalOcean c-4. The default build leaves it off because Tokio exempts that
API from its compatibility guarantee.

### RuntimeSchedulerSampleStale

Prometheus can still scrape the process, but its scheduler heartbeat is stale.
Inspect runtime CPU and blocked tasks. A flat latency histogram can persist
after the timer stops producing observations.

### TelemetryRecordsLost

Check the affected instance for `telemetry_log_records_dropped_total` by
`reason` and `telemetry_log_sink_errors_total` by `operation`. Relieve stdout
backpressure or restore the log sink. For tracing, inspect
`telemetry_trace_exporter_sdk_result_spans_total{outcome="error"}` and
`telemetry_sdk_diagnostics_total` by `event`. Check collector reachability,
trust and capacity. The SDK result counts all spans in a batch by the final
call result. The receiver separately reports explicit rejections through
`telemetry_sdk_reported_rejected_spans_total`. Verify recovery at the
destination with a sampled trace.

### TelemetrySpanQueueOverflow

The SDK dropped at least one span before it could reach the exporter because
the local batch queue was full. The SDK emits `queue_dropping_started` once per
batch processor, on the first drop, and may report the total drop count during
shutdown. With an active exporter, any increase in the diagnostic counter
over five minutes opens the ticket. A positive counter observed during the
first five minutes after process start also qualifies, covering drops before
the first scrape. The ticket clears when the observation window closes or
Prometheus sees a healthy replacement's zero counter.

Ticket expiry marks the end of the notification window. Further drops in the
same batch processor produce no new onset signal. Check the affected
instance's trace load and the collector's latency and capacity. Verify
current trace delivery at the destination with a sampled trace.

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
For a pending job, the wait starts when the job becomes due. For an abandoned
running job, it starts when the lease expires. Inspect the state and the last
worker that claimed the job to distinguish new work from stalled recovery.
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

Jobs of the reported kind failed permanently or exhausted their attempts
recently, as recorded by `jobs_last_terminal_attempt_timestamp_seconds`.
Read the failure in the `jobs-worker` logs, fix the cause, then inspect and
recover retained jobs with the operator commands `jobs-worker --help` lists
(`failed`, `inspect`, `redrive`, `discard`).
<!-- template:end jobs:docs-alerting-jobs-runbooks -->

<!-- template:begin outbox:docs-alerting-outbox-runbook -->
### Outbox delivery age

`jobs_outbox_oldest_unconfirmed_age_seconds` measures time since creation of
the oldest retained publication job without recorded completion. It includes
future retries, active attempts, blocked successors, and retained failures.
After an upgrade, an unresolved historical copy still counts even if another
job delivered the same logical event. The operator retains every failed outbox
job for recovery. Reconcile the consumer's recorded effect and resolve the
cause before redriving the original job. Set the alert threshold and duration
from the service's delivery SLO.

`jobs_outbox_oldest_blocked_age_seconds` reports the age of the oldest pending
ordered event with an unfinished predecessor. The worker checks up to 1,001
ordered candidates in creation order. Zero means the scan found no blockers
and covered every candidate. `NaN` means the sample found no blocker within
its limit or the worker has yet to observe the outbox. When blockers are
present, check for broker refusals, exhausted retries, or permanent failures
on predecessor events before recovering the ordered chain.

Evaluate the age gauges only on live worker targets with a fresh, nonzero
`jobs_outbox_observation_timestamp_seconds` and
`jobs_outbox_last_attempt_success == 1`. The worker retains the last successful
ages and timestamp after a failed observation. Check
`jobs_worker_operation_failures_total{operation="sample_outbox"}` and the
`jobs_operation_failed` log if sampling fails. An old timestamp with a
successful last attempt indicates that sampling has stopped or fallen behind.
Aggregate valid worker observations with `max` for each shared jobs database.
<!-- template:end outbox:docs-alerting-outbox-runbook -->

<!-- template:begin webhooks:docs-alerting-webhook-delivery -->
For `webhooks.deliver`, start with the job ID and error in the worker's
`job_failed` log. A payload decoding failure can exhaust the job before the
dispatcher runs. When the dispatcher reports `webhook_delivery_finished` for
that attempt, its endpoint and reason can help identify a retired receiver.
Contact the partner outside the webhook channel on `endpoint_gone`, repeated
5xx responses or `retry_after_exceeds_max_delay`. Reconcile the delivery ID
with the receiver before redrive.
See [Outbound webhook delivery](decisions/outbound-http-decisions.md#outbound-webhook-delivery).
<!-- template:end webhooks:docs-alerting-webhook-delivery -->

<!-- template:begin messaging:docs-alerting-messaging-runbooks -->
### Changing a durable subscription

All replicas sharing a stream and durable name must agree on the handled
subjects, event types and schema versions, the DLQ stream and subject, and
the broker's filter and delivery settings. Change these together.

Before upgrading an existing installation, inspect both stream configurations
and the effective consumer configuration. Provision writable source and DLQ
streams using Limits or WorkQueue retention. Set the durable's inactivity
threshold to zero, including any inherited stream consumer limit, so the broker
keeps the cursor through a worker outage. A memory-backed consumer needs a
planned migration to file-backed state: capture its pending work and ACK floor
before replacing it, and reconcile replayed effects before restarting workers.

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

The adapter recently attempted to move an event to the dead-letter stream,
as recorded by `messaging_last_dead_letter_timestamp_seconds`. A `rejected`
publication leaves the event in the source stream for redelivery. An
`ambiguous` publication also leaves it for redelivery, and the DLQ may
already contain a copy. Fix the handler or the producer. Use `inspect` in
the [`dlq_recovery` example](../crates/infra-messaging/examples/dlq_recovery.rs)
to read an exact DLQ record without changing the broker. Redrive and
retirement are available through the recovery controller for its own
isolated broker sessions. Use your deployment's recovery procedure for
other brokers.

Before a redrive, identify every durable subscription matching the original
subject. Redrive publishes a new stream record with the original logical
`Message-Id`. A subscriber that already acknowledged the event can receive
it again. For each subscriber with a side effect, verify that repeating the
operation is safe or that its idempotency record still covers this event.
Set the maximum redrive age within those protection windows. If any window
has expired or cannot be verified, reconcile the effects already applied or
repair the failed subscriber directly before considering a shared redrive.

If the DLQ was sealed during delivery, the worker stops with the source
sequence still unacknowledged. Provision a writable DLQ and follow the
durable subscription change procedure above to bind it before resuming.

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

<!-- template:begin temporal:docs-alerting-temporal-runbooks -->
### TemporalWorkerUnavailable

No reachable and ready `/temporal-worker` target answered Prometheus for five
minutes (targets are absent from service discovery, unreachable on scrape, or
every reachable replica reports `readiness_ready == 0`). Check deployment
status, recent rollout logs for startup refusal (invalid config, unmatched
`[temporal_worker.queues.<name>]`, or `temporal.enabled = false`), and
`readiness_probe_checks_total{probe="temporal"}` for Temporal Server
connectivity or authentication failures. Restore at least one ready worker
replica before task-queue backlog breaches workflow or activity timeouts.

### TemporalTaskQueueScheduleToStartHigh

More than 5% of workflow tasks on the reported `namespace` and `task_queue`
waited over 1 s before a worker started them for ten minutes. Check worker
replica count, CPU throttling, `RuntimeSchedulerLagging`, and
`temporal_worker.tuner` utilization (`target_memory`, `target_cpu`, or fixed
slot ceilings) alongside `temporal_workflow_task_execution_latency` to
distinguish insufficient worker capacity from slow replays or blocked threads.
Scale worker replicas or tune task-queue concurrency.

### TemporalWorkflowTaskFailures

Workflow tasks on the reported `namespace` and `task_queue` are failing
execution or replay for five minutes. Inspect `/temporal-worker` logs for
nondeterminism or panic errors, check recent worker deployments (`build_id`)
for unpatched workflow code changes (`ctx.patched(...)`), and reproduce failing
histories offline with `infra_temporal::test_support::ReplaySuite`. Roll back
the offending worker build or deploy a patched release while keeping prior
history compatibility intact.

### TemporalDrainForced

A `/temporal-worker` replica exhausted `http.drain_timeout` or received a
second `SIGTERM` during shutdown and preempted active activities via
`WillCompleteAsync` (`exit 3`) rather than canceling them on Temporal Server.
Check whether the platform sent a rapid double signal, whether
`drainingSeconds` is shorter than `http.grace_period + 5s`, or whether an
activity ignored cooperative `ActivityContext` cancellation and heartbeats
longer than `graceful_shutdown_period + activity_cancel_cleanup`. Verify that
healthy replicas retried and completed the preempted activities.

### TemporalClientRpcErrors

`/service` or `/temporal-worker` encountered sustained Temporal gRPC failures
(`temporal_request_failure` or `temporal_long_request_failure`, excluding
`CANCELLED` and normal domain outcomes such as `ALREADY_EXISTS` / `NOT_FOUND`)
for five minutes. Break down by `job`, `namespace`, `operation`, and
`status_code`: `UNAUTHENTICATED`/`PERMISSION_DENIED` points to expired or
revoked API keys or mTLS certificates (check `TemporalCredentialReloadFailed`);
`RESOURCE_EXHAUSTED` indicates Temporal Cloud/Server rate limits;
`DEADLINE_EXCEEDED`/`UNAVAILABLE`/`TRANSPORT_ERROR` indicates network, proxy,
or Temporal cluster degradation.

### TemporalCredentialReloadFailed

Live rotation of `temporal.api_key_file` or `mtls_rotation = "dynamic"` PEM
files has failed for five minutes while keeping the last valid credential
active. Inspect secret-manager sync or file permissions/atomic swap
mechanics on the affected replica (the watcher requires regular files
`<= 1 MiB` with valid ASCII keys or verified `aws-lc-rs` key/cert pairs) and
fix the file before the previously loaded credential expires.
<!-- template:end temporal:docs-alerting-temporal-runbooks -->
