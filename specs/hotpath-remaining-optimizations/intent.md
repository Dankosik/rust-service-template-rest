# Intent: optimize all remaining measured hot paths

Status: ready.

## Problem

The [profiling report](../hotpath-profiling/report.md) identified database
pool contention, sequential SQL exchanges, payload preparation, and HTTP
observability overhead. The accepted [body-transfer optimization](../hotpath-optimizations/report.md)
removed one body copy but left those four areas unresolved. The user's
continuation, «давай все это оптимизируем», covers all four; another selection
of only the easiest change would not satisfy it.

## Desired outcome

Reduce the remaining avoidable cost with measured changes that preserve the
service's behavior. Evaluate and disposition every named area. Adopt a change
only when its benefit is supported and its correctness and resource costs are
acceptable; report an unsuccessful or inconclusive candidate explicitly,
retaining the simpler correct baseline rather than calling it an optimization.

## Affected actors and systems

Inbound webhook senders, webhook consumers and other durable-job callers;
service operators using pool configuration, logs, traces and metrics;
the HTTP, PostgreSQL, jobs, webhook and telemetry adapters; and the approved
DigitalOcean comparison environment.

## Scope and non-goals

Include deployment-specific pool sizing, fewer SQL round trips with unchanged
durable outcomes, payload serialization, and span/metric allocation overhead.
The accepted body-transfer candidate is the starting behavior, not a new win
to count in this task. A deployment recommendation must account for its actual
connection budget; the synthetic deployment is the authorized runtime target.

Do not weaken durability, authentication, deduplication, resource bounds or
observability to obtain a faster result. Do not tune Railway, raise the template
pool default globally, claim production capacity, alter unrelated dirty work,
push, open a PR, or call paid external providers.

## Constraints

All builds, tests, databases, services, load generation, hotpath, and executable
measurement reduction run only on DigitalOcean. Local work is limited to
source/document reads and edits, Git, and root-owned remote orchestration.
Root owns host authorization, provisioning, SSH, execution and cleanup; a
previous host's approval does not authorize a new paid machine. Its current
authority and expenditure limits live in [operations](operations.md).

Use synthetic data and test credentials; do not transfer production data or
user credentials. Retain hotpath CLI/library 0.28.4, opt-in instrumentation,
jemalloc, and MCP access through SSH. Preserve unrelated working-tree changes
and freeze a complete reproducible baseline before implementation edits.

## Success signal

A new comparable report identifies the exact baseline and final candidate,
the measured benefit or rejection for each of the four areas, behavior proof,
resource tradeoffs and limits. Every area receives a concrete disposition;
no claim of speedup relies solely on older measurements or a forecast.
The overall result includes the surviving measured improvements and explicitly
states any areas where no safe useful optimization was established.
