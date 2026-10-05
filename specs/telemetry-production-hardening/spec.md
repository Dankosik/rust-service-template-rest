# Production telemetry hardening

Status: ready

Authority: [Intent](intent.md). Evidence: [accepted TELEM-R2 research](research/accepted-evidence.md).
Baseline: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.

## Outcome and scope

Telemetry remains best effort and subordinate to serving and bounded process
termination. A stopped output sink cannot hold application workers or teardown
indefinitely. Diagnostic data leaving the changed HTTP/gRPC, panic, and SDK paths
is deliberately admitted and bounded. Signals distinguish local completion,
known failure, and unavailable delivery evidence.

The change covers the shared logger and trace provider and their existing
entrypoint consumers. It includes JSON and text logging, exporter diagnostics,
and normal, failed-startup, and interrupted-startup cleanup where those resources
were installed. It does not alter business results, HTTP Problem shapes, request
admission, readiness policy, or the success/failure of a migration or job.

## S1. Truthful export and shutdown reporting

Exporter initialization means configuration/client construction succeeded, not
that a receiver was contacted. A disabled exporter remains disabled without
network attempts; a construction failure remains observable degradation under
the existing admission policy.

An SDK batch success means only that the SDK exporter returned success. It does
not establish that every span was accepted, parsed, persisted, or queried at a
Collector/backend. Existing exported-span counters must have accurate help and
operating documentation; failure labels remain finite and never contain raw
error text. Do not manufacture accepted/rejected counts unavailable at the
chosen boundary, and do not treat absent evidence as zero rejection.

The final local trace shutdown operation must retain any exporter failure it
observes in the work being drained, including a failure followed by a successful
batch. A failure from an export already in flight when shutdown begins belongs
to that drain. An SDK outer success cannot erase it. The shutdown result and
records must name only what was established; remove the claim of a confirmed
clean flush from a merely completed SDK shutdown. Partial rejection or malformed
success that the stock SDK does not expose stays explicitly outside that
completion claim; this does not require a custom protocol/exporter processor.

| Situation | Required meaning |
| --- | --- |
| Local shutdown completes within its deadline, with no observed final-drain failure | Local shutdown completed; delivery is unconfirmed. Exit 0 remains permitted when all other stages succeed. |
| Observed final-drain exporter failure, provider/join failure, or final local logging drain failure/expiry | Incomplete/degraded telemetry shutdown; never clean completion. Service and ordinary worker use their existing degraded exit classification (3) unless an existing primary failure already requires 1. |
| Earlier runtime log loss or export failure followed by successful bounded teardown | Runtime degradation remains observable; historical loss alone does not turn ordinary termination into a crash or replay application work. |
| Final delivery cannot be inspected because diagnostics already closed or the process was killed | No final-scrape or delivery claim; absence of a last record is not evidence of success. |

For a migration or other finite command, telemetry must not turn a failed
business operation into success or invite replay of a committed effect.
Preserve its primary result; expose incomplete telemetry separately under its
existing command lifecycle. Technical Design must name the actual consumers
and mapping instead of silently leaving one on obsolete flush semantics.

## S2. Diagnostic privacy at the source

For built-in public HTTP observation, retain the route template (or the existing
unmatched sentinel), normalized method, response status/error classification,
protocol, timing, and validated request/trace correlation. Do not emit raw URI
path or query, User-Agent, or caller-supplied authority/host to logs, span
attributes, span names, or span events. Static service resource identity remains
available. This deliberately replaces the four-key query denylist: encoded
keys, arbitrary parameter names, and path-carried values must not escape it.
The existing bounded validated request id remains correlation, never identity;
this change does not infer whether an otherwise admitted id is personal data.

Apply the same source rule to the retained gRPC server observer: withhold
caller-supplied authority/host and User-Agent from server spans, events and logs.
Keep its admitted service/method identifiers (or existing unknown sentinel),
finite status/failure categories and correlation. This does not change gRPC
client destination identity, RPC behavior, status mapping or method-cardinality
policy. A shared implementation must distinguish those server and client roles.

All shared panic-hook consumers and HTTP panic recovery withhold the payload,
including string, formatted, and non-string payloads. Keep a stable event,
source location and safe execution context useful for diagnosis. Free-form
runtime context such as thread names must also be admitted/bounded; a recovered
handler panic still produces the existing sanitized 500 Problem.

SDK/export transport diagnostics must not copy arbitrary receiver error
messages, response bodies, credential-bearing URLs, or credentials into either
local logs or exported traces. The rule applies under debug/trace logging as
well as normal levels and to failures at initialization, export, and shutdown.
Keep safe finite event/error categories and numeric facts when available;
withhold unstructured diagnostic detail when it cannot be safely admitted.
There is no raw-diagnostic escape switch in this PR. Collector redaction is
defence in depth and cannot be the enforcement point for these rules.

Enforcement is scoped to these concrete paths, not a claim that arbitrary
future application log strings can be automatically classified for secrets.
Existing `SecretString`, TOML secret guards, typed/ambient credential exclusion,
TLS trust policy and output redaction remain authoritative.

## S3. Bounded logging that fails open

The shared logger must not perform potentially blocking sink I/O on the caller's
request, SDK export, or shutdown-control thread. A stopped or failing sink must
not cause unbounded caller waits, unbounded queued bytes, recursive logging,
or a fallback synchronous write to that same sink. Logging failure alone does
not reject requests or withdraw readiness.

Admission has a finite record-byte limit and finite retained queue capacity.
Formatting and reusable scratch storage must also remain bounded for oversized
input; serializing an unlimited record and discarding it afterward does not
satisfy the requirement. Caller-owned source data and arbitrary user-defined
`Display` implementations are outside this memory guarantee. Technical Design
selects numerical values from current records, existing budgets and dependency
behavior, and records their units and accounting boundary.

On saturation, new records are dropped without waiting for sink space; already
admitted records retain FIFO order. On an oversized record, discard the whole
record and count the reason; do not emit malformed or ambiguously truncated
JSON. Admitted JSON retains unique/reserved key semantics, correlation, and
valid one-record framing. Sink write failures are counted distinctly from
admission drops. Recovery allows subsequent records when the sink resumes;
dropped records are not replayed or persisted by the application. A partially
failed OS write can leave partial output and must not be called delivered.

Loss/error accounting must not depend on successfully writing another log.
Use the existing observability surface with finite reasons and no caller-data
labels; no OTLP log/metric pipeline is introduced for this purpose. Accounting
is local observation, not guaranteed scrape or backend receipt. When the metrics
endpoint is disabled or already closed, documentation must name the resulting
observability limit rather than promising a final visible count.

The logger's resource owner outlives request handling, dependency cleanup,
provider shutdown and their final records. Explicit logger cleanup spends the
remaining existing telemetry/process grace budget, including any worker/guard
join; ordinary destructor behavior cannot add an unbounded wait afterward.
When the bound expires, pending records may be lost and cleanup is incomplete.
An uninterruptible sink operation may outlive a bounded wait, but must not
prevent process exit. SIGKILL/crash offers no drain guarantee. No new longer
platform grace period is implied by this change.

## S4. Operating contract and compatibility

Update the existing configuration, runtime-lifecycle and telemetry/performance
documentation at their current owners. Document the selected limits, failure
and recovery semantics, logger ownership, trace queue/retry boundary, and what
each metric/event can establish. Name removed raw fields and renamed shutdown
signals so log consumers can migrate; retaining a misleading alias is not a
compatibility requirement. Preserve unrelated JSON keys and normal correlation.

Record these unchanged operational responsibilities: metric emitters own label
cardinality (there is no total registry cap/TTL); histogram upkeep requires
worker progress despite independence from scraping; deployment owns Collector
queue/retention, privacy policy, private diagnostics exposure and backend
durability; sampling applies to traces and parent sampling can override the root
ratio. Existing HTTP/gRPC finite labels, buckets and sampling ratio stay intact.
Old benchmark results keep their original workloads and limitations. Describe
how existing proof/performance owners can reproduce cost or degradation when
needed, without making a new benchmark campaign a completion gate.

Keep the custom JSON semantics, INFO-span filter behavior, standard propagation,
compression support, SDK batching/retries and metrics registry. Do not add a
fork, custom batch processor, second telemetry pipeline, universal cardinality
framework, dashboard project, or unrelated feature/configuration changes.

## Proof boundary and phase handoff

Acceptance needs focused local negative evidence for privacy at both output
boundaries, fail-open behavior and bounded retention with a stopped/failing
sink, loss accounting and recovery, and truthful final-drain results under
receiver failure and a later successful batch. Preserve existing successful
HTTP/TLS export, logging/correlation and process-lifecycle coverage. Proof must
exercise real formatter/export/lifecycle boundaries using existing fixture and
process owners; isolated type assertions cannot establish those effects.
Implementation chooses the concrete cases, coordination and commands. No live
Collector/backend or paid environment is needed to establish this contract.

Technical Design owns mechanism, crate comparison/admission, fixed bounds,
diagnostic enforcement placement, lifecycle ownership and the complete affected
consumer set. Reopen Specification if a proposed mechanism needs different
loss, privacy, termination or exit semantics; reopen Research for changed SDK
behavior, and Intake only for changed user meaning or external authority.

The necessary research recommendations are accepted as S1-S4. Additional
sampling/bucket/cardinality tuning and backend infrastructure changes are
deployment policy; existing finite labels, upkeep, credentials and trace
correlation already address their stated responsibilities and need no rewrite.
No production incident, durability guarantee, or current speedup is claimed.
