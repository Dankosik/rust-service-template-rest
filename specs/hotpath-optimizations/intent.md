# Intent: reduce webhook payload overhead

Status: ready. Definition owner: `/root/optimization_definition`.

## Problem

The accepted [profiling report](../hotpath-profiling/report.md), independently
[reviewed PASS](../hotpath-profiling/review.md), identifies avoidable allocation
and preparation cost in inbound webhook admission. It measures the existing
cost; proposed code speedups are forecasts.

## Desired outcome

The user requested: «Давай внедрим оптимизации, которые считаешь нужными».
Implement the smallest useful behavior-preserving optimization supported by
that evidence and show a fresh comparison with behavioral proof. The selected
first scope is payload construction and preparation for durable inbound jobs,
especially large bodies. A plan alone does not complete the request.

## Affected actors and systems

Webhook senders, the HTTP receiver, the webhook and jobs adapters, downstream
job consumers, and operators reading existing logs, traces, and metrics.

## Scope and non-goals

Reduce avoidable payload memory turnover and preparation work. Preserve existing
public behavior, persistence outcomes, and safety bounds. Pool sizing remains
per-deployment operator work; this task does not change the global default.
SQL round-trip changes, storage durability tuning, HTTP observability changes,
new queue mechanisms, and unrelated working-tree changes are outside this scope.

## Constraints

Use current crate boundaries and existing dependencies first. No production
deployment, push, or PR is authorized. Preserve the dirty profiling
instrumentation and unrelated changes on `codex/hotpath-optimizations-20261004`.

Local work is limited to source/document reads and edits and lightweight
coordination. No local cargo, make, Docker, tests, services, load generation,
hotpath, Python, or Node processing. All resource work runs only on the newly
approved DigitalOcean droplet: 8 AMD vCPU, 16 GiB, `fra1`, $0.16667/hour,
at most 8 hours (about $1.34). The root owns provisioning and deletion.
Use synthetic data/providers; copy no credentials or production data.
Railway access is read-only MCP only.

## Success signal

A retained code improvement measurably reduces the selected payload cost,
passes relevant behavioral proof, and has a new comparable before/after report
that distinguishes allocation, CPU, latency, throughput, and memory results.
The report states observed variance and limits; an unconfirmed forecast is not
a delivered speedup. No unresolved user-owned decision remains.
