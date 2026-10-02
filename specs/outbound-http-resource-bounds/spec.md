# Outbound HTTP resource bounds

Definition candidate: 2026-10-02, base `67be869`.
Requester meaning: [intent](intent.md). This is the fixed behavioral contract;
the [Definition result](definition-result.md) owns review and movement state.

## Accepted delta

Two existing resource guarantees become enforceable: encoded response payload
storage does not retain a history of transport frames, and one OAuth credential
owner admits only a configured finite number of token acquisitions. Existing
trusted-provider, token reuse, and caller deadline semantics remain authoritative.

### Buffered responses

1. For ceiling `L = Limits::response_body_bytes`, complete successful responses
   contain exactly the encoded data bytes, in order, only after body EOF.
   Equality with `L` succeeds; the first data exceeding it fails with
   `ResponseBodyTooLarge`, without returning a prefix as success. An exact
   advertised length above `L` is still rejected before body reads.
2. Accumulation releases each consumed frame's backing storage before retaining
   another frame. There is no growing collection of frame handles, retained
   consumed read buffers, or per-frame metadata; the returned body must also
   not pin an oversized input backing allocation. Accumulator storage grows
   with bytes actually received rather than reserving the ceiling or declared
   length at header arrival. Technical Design must state its finite capacity
   and transient-growth bound in terms of `L`, independent of chunk count.
   This is a body-accumulation guarantee, not a process RSS or DNS/connection
   memory guarantee; the transport's current frame and fixed read buffers exist.
3. Empty data and trailers do not create an early-success path. Trailers retain
   the existing buffered API behavior (not exposed); EOF and later body errors
   are still observed. Body protocol/transport errors remain typed `Transport`
   with their retained cause. No new error taxonomy or transport API is needed.
4. Target admission, whole-exchange deadline, timeout, drop/cancellation, status,
   response headers, and exactly-once terminal observation retain their current
   meaning. Both receiving and accumulating the body spend the existing earlier
   caller/operation deadline. No background collector or additional retry exists.

### OAuth provider admission

1. `integrations.<name>.oauth.provider_concurrency` is a positive `u32`, default
   **32**. TOML and `APP__INTEGRATIONS__<NAME>__OAUTH__PROVIDER_CONCURRENCY`
   follow normal configuration precedence. Zero, negative, fractional,
   nonnumeric, or out-of-type-range values fail configuration; adapter options
   independently refuse zero. It is a nonsecret immutable deployment input.
   Absent OAuth tuples remain inert. The same value reaches the prepared
   credential owner through composition and documented direct-construction APIs.
2. One prepared `Credentials` owner and all its clones/bindings share that bound.
   Independently prepared owners have independent limits, even with the same
   endpoint; this is not a process-wide or provider-account-wide quota. The
   limit counts actual local acquisition attempts, across client credentials,
   foreground refresh, background refresh, and RFC 8693 exchange together.
   Retained cache entries and callers coalesced behind an attempt do not count.
3. Cache lookup and existing same-key single-flight run before new-attempt
   admission. A valid hit succeeds even at capacity; an existing same-subject
   exchange or service refresh can still be awaited under each caller's own
   deadline. A new attempt is admitted immediately if capacity is free. If
   saturated it fails immediately as `AcquisitionError::AtCapacity`, before
   signing a client assertion or token-network I/O. There is **no admission
   queue, reservation, priority, automatic retry, or capacity-failure cache**.
   Existing single-flight waiting is deliberately unchanged. A later ordinary
   call may try again when capacity is available.
4. Capacity remains occupied throughout the local token request, complete body
   read, and response admission/parsing, and is released on success, every
   error, timeout, and cancellation/drop of the actual attempt. A coalesced
   waiter's cancellation must not release another caller's live attempt slot.
   An initiating caller's cancellation/deadline preserves existing leadership
   transfer: a surviving waiter may initiate a replacement, which must undergo
   admission itself. No detached replacement request is added. A local drop
   does not promise physical cancellation of provider work or system DNS.
5. An already expired deadline takes precedence over capacity rejection and
   causes no token I/O. No admission step resets a deadline. Each caller keeps
   its absolute budget while coalescing; each actual attempt retains the existing
   five-second cap and initiating caller cancellation/deadline bound. Resource
   dispatch after acquisition retains its original remaining deadline.
6. Capacity failure cannot dispatch the resource request, supply a service token
   in place of a subject token, evict a valid token, or invalidate another
   subject's cache entry. Background-refresh refusal retains the usable token
   and existing 30-second refresh retry spacing; foreground calls using that
   token succeed. Both grants use identical capacity admission.
7. HTTP bindings expose the closed acquisition error without inventing an
   inbound HTTP response mapping. gRPC maps `AtCapacity` to `UNAVAILABLE` with
   the existing sanitized availability message and typed local source.
   Acquisition observation uses finite outcome `capacity` once per rejected
   initializer; coalesced waiters do not multiply it. A rejected acquisition
   emits no outbound-HTTP attempt observation. Background refusal uses the
   existing background-refresh warning with the closed label. No tokens,
   subjects, endpoint paths, or arbitrary provider text enter diagnostics.

The default matches the existing introspection provider's concurrency default.
It leaves distinct subjects concurrent while bounding one owner's active token
bodies to at most 32 responses at the existing 1 MiB payload ceiling, plus the
documented transport/parser/accumulator overhead. Operators select the bound
for their deployment; no measured throughput or universal safe-memory claim is
made. The new option changes direct Rust struct construction and the new enum
variant changes exhaustive matches; migrate all repository consumers/examples
in this PR and document the adopter adjustment. Existing configuration files
remain valid through the default.

### Documentation truth

The outbound guide and decisions must distinguish payload ceiling, body storage,
cache capacity, same-key coalescing, and provider-attempt concurrency. Remove the
claim that OAuth single-flight alone bounds all token requests. Correct the
claim of exactly one hyper-util replay: the resolved client may retry an unsent
request returned after a reused pooled connection fails, with no fixed numeric
replay limit in that loop; it does not retry requests that may have been sent,
and the outer operation deadline still bounds the exchange. No transport retry
setting changes. Historical benchmark results keep their date/revision and
must not be presented as measurements of the changed collector.

## Deliberately unchanged

The [outbound contract](../../docs/outbound-http.md) remains authoritative for
fixed HTTPS origin admission, TLS, private providers, header/parser limits,
HTTP/1 framing, no decompression/redirect/proxy, trace propagation and provider
retry ownership. The [machine-authentication contract](../../docs/outbound-machine-authentication.md)
retains assertion format, both grant forms, subject isolation, cache expiry,
single-flight sharing/failure semantics, age-gated exact-token eviction,
authorization conflict and required-subject refusal, resource status passthrough,
and no replay of resource requests. No resolver or inbound-auth change is needed.

## Proof obligations and handoff

Implementation chooses the smallest relevant cases and commands under the
repository validation owner; this is not a prescribed test plan. Decisive
falsifiers are fragmented bodies retaining consumed frame allocations, success
before EOF, an over-limit body accepted, more than the configured token attempts
alive for distinct subjects/mixed grants, capacity consumed by hits/waiters,
leaked/double-released slots after failure/drop, lost caller budgets, and a
capacity refusal causing resource I/O or token/cache mutation. Configuration
default/source/range and gRPC projection need boundary proof, reusing existing
coverage where adequate. Static review must establish the accumulator's
retention invariant; elapsed time or process RSS alone cannot prove it.

Technical Design selects the collector and admission mechanism/placement from
existing code and dependencies and closes the stated capacity-growth bound.
No dependency upgrade, new transport limiter, new benchmark project, or live
provider certification is required. Reopen Definition only if preserving these
observable contracts is infeasible or new evidence changes a material rule;
routine mechanism and proof choices remain with their next owners.

## Evidence used

- Base `67be869`: `infra-outbound-http::Client::exchange` uses
  `Limited::collect`; `Credentials::exchanged_or_fetch` uses per-key Moka
  initialization, while `Inner::post_form` has no shared admission bound.
- Existing `infra-bearerauthn` introspection uses provider-owned immediate
  admission after coalescing; its config default is 32. This is precedent,
  not a request to consolidate its transport.
- Resolved hyper-util 0.1.21 `client/legacy/client.rs::send_request` loops over
  canceled unsent requests and admits that retry only for a reused connection.
- Earlier baseline tests and historical performance evidence are context only;
  they do not establish validation of this candidate.
