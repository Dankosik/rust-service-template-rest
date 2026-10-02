# Intent: close outbound HTTP resource-bound gaps

## Problem

The architecture review found that the buffered HTTP response ceiling counts
payload bytes but retains frame overhead and backing storage until EOF, and
OAuth token-exchange misses for different subjects have no provider-owned
concurrency bound. The outbound documentation overstates the resulting bounds.

## Desired outcome

Close the justified findings completely in one focused separate pull request,
including affected configuration, composition, documentation, and meaningful
regression proof, so the retained outbound architecture has accurate and
enforced resource guarantees.

## Affected actors and systems

The fixed-origin outbound HTTP client, its OAuth credential owner and HTTP/gRPC
bindings, configured integrations, and operators consuming their documentation.
Webhook delivery shares the body collector but retains its worker admission.

## Scope and non-goals

Scope is buffered response retention, OAuth token-attempt admission across both
grant types, their cancellation/deadline behavior, and inaccurate bound/replay
documentation. Preserve the operator-selected trusted HTTPS architecture.
Exclude a transport migration, HTTP/2, streaming, a caller-URL SSRF capability,
resolver replacement, inbound-auth transport consolidation, dependency/toolchain
upgrades, and unrelated review improvements.

## Constraints

Retain normal TLS, private operator-configured providers, HTTP/1 buffered APIs,
adapter retry ownership, and the absence of redirects, proxies, and
decompression. Preserve unrelated work. Authority covers local changes and
required validation plus one separate PR; no merge or deployment.

## Success signal

The assembled candidate enforces the response and token-attempt bounds, retains
the existing authorization/cache/deadline contracts, has passing relevant
validation and required independent review, and is published in the separate
PR with exact evidence and any pending CI clearly identified.
