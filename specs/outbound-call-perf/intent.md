# Intent: reduce work per authenticated outbound call

## Problem

An inbound request waits for an outgoing HTTP or gRPC operation. Even with a warm machine token, the caller pays for deadline handling, request preparation, transport, response consumption and observation. Concurrent requests can also wait while credentials refresh.

## Desired outcome

Implement the readable, behavior-preserving improvements identified by the full-source review and evaluate every conditional alternative against its actual costs. Prefer straightforward ownership and control flow. Retain the simpler current implementation when an alternative fails to show a gain or changes an accepted contract.

## Affected actors and systems

Callers of infra-outbound-http, infra-outbound-grpc and infra-oauth2-client-credentials, including ordinary service-token hits, exchange-token hits and callers sharing a refreshing credential source.

## Scope and non-goals

Cover service-hit waiter setup, duplicate cutoff clock reads, HTTP observation ownership, HTTP host extraction, warm gRPC method parsing, empty HTTP content length, immediately terminal gRPC responses, saturated method admission and lazy gRPC span attributes. Evaluate Hyper Host insertion, single-frame HTTP buffer retention, shared versus exclusive service-cache access, exact authority comparison and HTTP default-port pool fragmentation. The review found no basis to canonicalize pool keys in the existing fixed-URL workload. Preserve that disposition unless new evidence establishes the workload and compatible behavior. Client construction, discovery and provider grant execution are cold. Refresh waiting, dispatch, response lifetime and transport retries are in scope.

## Constraints

The accepted outbound HTTP, machine authentication and library selection decisions remain binding. Compare equivalent artifacts on a separate temporary CPU-optimized DigitalOcean Droplet only. The user authorized a total ceiling of US$5 and deletion of that Droplet after results are saved. Existing Droplets belong to other work. No local measurements. Repository implementation and validation rules apply. No release, push or deployment requested.

## Success signal

Account for each review item with a readable implementation or an evidence-backed reason to retain the current path. Preserve cancellation and deadline precedence, provider call counts, origin admission, terminal status and metric behavior. Capture comparable remote counts and CPU per call, including p99 for single and 64-way service-token calls before and during refresh, and memory or connection evidence for changes that depend on those resources. Save results and verify teardown within the approved budget.
