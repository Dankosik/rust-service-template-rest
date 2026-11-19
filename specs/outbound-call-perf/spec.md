# Specification: reduce work per authenticated outbound call

status: ready  
Basis: [intent.md](intent.md), [full-source review inventory](research/review.md)

## Actors and surfaces

- Outbound HTTP callers and receiving origins use the buffered and headers-only [HTTP client](../../crates/infra-outbound-http/src/lib.rs).
- Outbound gRPC callers use the [gRPC client](../../crates/infra-outbound-grpc/src/client.rs), with direct transport or decoded status observation.
- Service-identity and on-behalf-of callers use the [authenticated HTTP](../../crates/infra-oauth2-client-credentials/src/http_client.rs) and [authenticated gRPC](../../crates/infra-oauth2-client-credentials/src/grpc.rs) clients. The registration, refresh driver, provider, and resource peer can observe acquisition and dispatch.
- Operators consume [HTTP observation](../../crates/infra-outbound-http/src/observe.rs) and [gRPC observation](../../crates/infra-outbound-grpc/src/observe.rs). The benchmark operator consumes the comparison, cost, and teardown evidence.

## Current baseline

- Service-token acquisition checks registration and caller state around token reuse and refresh, with early refresh while a token is still reusable and bounded demand acquisition after it stops being reusable. Exchange has a per-subject cache and separates reusable results from request-only results. [Service acquisition](../../crates/infra-oauth2-client-credentials/src/credentials.rs), [exchange acquisition](../../crates/infra-oauth2-client-credentials/src/credentials/exchange.rs).
- HTTP prepares origin and framing before sending and observes through final headers or buffered completion as appropriate. Request and response limits, timeout behavior, and retry boundaries can be seen in the existing [HTTP tests](../../crates/infra-outbound-http/src/tests.rs) and [transport configuration](../../crates/infra-outbound-http/src/transport.rs).
- gRPC resolves an admitted method before selecting its metric series, and the response path handles terminal headers, EOF, and open streams. [Observation](../../crates/infra-outbound-grpc/src/observe.rs), [response custody](../../crates/infra-outbound-grpc/src/call.rs), [status coverage](../../crates/infra-outbound-grpc/tests/client_status.rs).
- The HTTP admission rules recognize same-origin alternate authority spellings, while the transport determines its pool identity from the request URI. [Admission](../../crates/infra-outbound-http/src/policy.rs), [URI origin rules](../../crates/shared-rules/src/uri.rs), [vendored pool dispatch](../../vendor/hyper-util/src/client/legacy/client.rs).

## Terminology

- A **service-token hit** reuses the service source's current eligible token. A hit **during refresh** occurs while its replacement acquisition is in flight and the current token is still reusable. Waiting for a replacement after the old token ceases to be reusable is a separate scenario.
- An **exchange-token hit** reuses an eligible token for the supplied subject and configured exchange target.
- An **attempt** is the logical observation at the existing client boundary. A transport reconnection or retry before a request starts writing does not constitute another application attempt.
- A **canonical authority** is the configured authority spelling. An alternate spelling can be admitted by the origin comparison even if the transport pools it separately.

## Behavior delta

### R1 — Service-token hits and refresh waiting

Reduce work on ordinary reusable service-token hits while preserving the ability of concurrent callers to reuse the eligible token during an early refresh. A waiter that needs a replacement still obeys its own cancellation and deadline and can observe registration or refresh-driver closure. Reuse eligibility is checked when selecting the cached token. A later rejection does not revoke that caller's selection, while the dispatch expiry check still applies. Contention continues to respect cooperative scheduling.

- **Falsifier.** A pending early refresh holds reusable-token hits until the provider returns, acquisition succeeds despite closure or a stop at its existing checks, or concurrent demand causes extra token grants for the same reusable result.

### R2 — Credential cutoff work

Reduce redundant cutoff work in active checks on service and exchange paths. Preserve checks across scheduling and acquisition boundaries where time, cancellation, or registration state can change. A cutoff already reached at an active check takes precedence there. A representable resource deadline produces the existing timeout, and expiry of the synthetic acquisition cutoff produces unavailable. Keep the existing stop precedence, origin and authorization checks, and dispatch expiry check throughout.

- **Falsifier.** An acquisition reports cancellation or a ready credential where its active check would return a reached cutoff, reports timeout for a synthetic cutoff, or allows an expired token to dispatch.

### R3 — HTTP observation and destination fields

Reduce redundant HTTP observation ownership and host extraction while preserving exactly one terminal observation for each attempt already observable at the existing boundary. The caller's result and the recorded classification continue to distinguish failures before final headers from failures afterward, including a recorded final status when the subsequent body fails. Preserve abandonment, timeout, trace propagation, destination address and port, URL-template rules, and existing error labels.

- **Falsifier.** A cancellation during body consumption emits two observations, loses the already received status, changes the recorded destination, or replaces an already settled timely result.

### R4 — HTTP empty-body framing

Reduce work when framing HTTP requests with an empty body. Preserve the wire contract. An empty POST, PUT, or PATCH sends `Content-Length: 0`. Other empty methods omit the header. A nonempty body sends its byte length. Caller-supplied transfer encoding is stripped and caller-supplied content length is replaced or removed under the same rule.

- **Falsifier.** A receiving origin sees omitted length for an empty PUT, added length for an empty GET, or framing that reflects the caller's supplied length instead of the body.

### R5 — gRPC method admission and tracing

Reduce work for previously admitted methods, new methods after admission reaches capacity, and span attributes when no OpenTelemetry context consumes them. Only a valid request path with a matching declared method may enter a named series. Keep the existing global bound, admitted method names, and fallback series. At capacity the admitted names remain usable and new valid names resolve to fallback. Preserve redaction, the bounded diagnostic attributes of enabled spans, and metric behavior regardless of span enablement. Preserve gRPC trace context on the wire, including propagation from an active parent context when the outbound span is filtered.

- **Falsifier.** A warm path with absent or conflicting method metadata uses a named series, saturation reassigns an admitted series, or enabled and disabled tracing produce different metric counts.

### R6 — Immediately terminal gRPC responses

Reduce work when an arriving gRPC response is already terminal. Preserve the precedence of an already observed lifetime stop, wire status, and the existing missing-status and HTTP fallback cases. At attachment of an already terminal response, preserve immediate release of upload custody, the existing transport observation, and once-only wire status hook behavior when applicable, independent of subsequent body polling. For decoded calls the decoder still owns reporting of the decoded outcome, with the existing local-stop and abandonment behavior. Preserve staged stop trailers when applicable. Open streams still honor deadline or cancellation while idle and finalize on the same terminal conditions as before.

- **Falsifier.** A trailers-only response delays upload release until polling, a decoded metric is attributed to an undecoded wire status, a terminal outcome is double-counted, or an open unpolled stream loses its stop behavior.

### R7 — Conditional alternatives

Give each option a recorded disposition with evidence about the benefit, cost, and applicable guarantees. Retain the simpler current behavior when evidence does not establish a gain with equivalent behavior. A structured comparison may use source evidence to rule out an option with a demonstrable incompatibility or an absent triggering workload. Record measured conclusions separately from source-derived conclusions.

| Alternative | Deciding evidence |
| --- | --- |
| Disable redundant Hyper Host insertion | Preserve configured Host on the wire, rejection of caller Host, URI handling, and retry boundaries. Consider canonical and alternate admitted authorities and the occupied Host lookup. An incidental panic in the redundant lookup is outside the supported contract. |
| Retain a single HTTP data frame | Preserve response bytes, byte limits, trailers and EOF handling, errors, and cancellation. Account for backing allocation retained across response lifetimes, including short slices of larger reads. |
| Shared versus exclusive service-cache access | Account for single and concurrent calls before and during refresh. Preserve the current-token decision when a reader and a replacement or rejection race, including refresh scheduling and provider request counts. |
| Compare exact authority before the normal origin decision | Compare canonical and accepted alternate spellings and account for the extra work on misses. Preserve invalid-origin refusal and existing case, port, userinfo, and address handling. |
| Canonicalize default-port spellings for HTTP pooling | Keep the current disposition for the fixed-URL workload. Reopen only if a relevant workload demonstrably mixes accepted spellings and connection evidence supports a compatible gain. Account for URI, Host, DNS, TLS, retry, and origin behavior if reopened. |

- **Falsifier.** The report omits an alternative, declares a resource-dependent win without corresponding resource evidence, or uses an exact-spelling shortcut that rejects an origin the current comparison admits.

### R8 — Comparable remote proof and closure

Run the performance comparison only on the separately created temporary CPU-optimized DigitalOcean Droplet within the total US$5 authority. Baseline and candidate must be comparable release artifacts, with matched relevant build settings, host conditions, workload, instrumentation, and response consumption. Save enough evidence to identify the artifacts, conditions, repeatability, and limits. Report counts and caller CPU per call, and p99 for both single- and 64-way service-token calls before and during an active refresh. Keep successes, refusals, and provider and resource request counts visible so changes in completed work cannot masquerade as savings. Include memory or connection evidence for conclusions that depend on those resources. Save results and verify deletion of the task Droplet. Existing Droplets and local performance measurements are outside this authority.

- **Falsifier.** A comparison counts unlike work, confounds provider CPU with caller CPU, lacks the in-flight refresh or required concurrency evidence, or reports completed teardown before verified deletion and saved results.

## Preserved guarantees

- **G1 — Operation precedence and custody.** Keep parent and request-extension stop precedence, client-specific opening and lifetime budgets, and distinct cutoff and cancellation outcomes. A stopped operation cannot turn into a successful late dispatch or successful late buffered result. Final response handling still owns cancellation, timeout, and resource release at the current boundary. [Operation context](../../crates/operation-context/src/lib.rs), [HTTP request context](../../crates/infra-outbound-http/src/context.rs), [gRPC opening and lifetime](../../crates/infra-outbound-grpc/src/client.rs).
- **G2 — Credential identity and source lifecycle.** Keep TLS and configured resource-origin binding, conflicting authorization refusal, grant-specific subject requirements, token reuse and expiry windows, age-gated rejection, per-source or per-subject acquisition coordination, failure throttling, and provider call semantics. Request-only tokens remain request-only. [HTTP authentication](../../crates/infra-oauth2-client-credentials/src/http_client.rs), [gRPC authentication](../../crates/infra-oauth2-client-credentials/src/grpc.rs), [acquisition](../../crates/infra-oauth2-client-credentials/src/credentials.rs), [exchange](../../crates/infra-oauth2-client-credentials/src/credentials/exchange.rs).
- **G3 — HTTP wire behavior.** Keep caller Host refusal, configured Host, same-origin and TLS restrictions, default Accept and caller headers, bounded buffering, headers-only acknowledgment, and existing reuse and retry semantics. A request that could already have reached the peer is not replayed by this client. [HTTP policy](../../crates/infra-outbound-http/src/policy.rs), [HTTP tests](../../crates/infra-outbound-http/src/tests.rs), [pool and retry source](../../vendor/hyper-util/src/client/legacy/client.rs).
- **G4 — Observable status.** Preserve existing public APIs, metric names, labels, cardinality, status attribution, and reporting at the existing terminal boundaries, including gRPC local stops and fallback for unverified methods. [HTTP observation](../../crates/infra-outbound-http/src/observe.rs), [gRPC observation](../../crates/infra-outbound-grpc/src/observe.rs), [gRPC status behavior](../../crates/infra-outbound-grpc/src/call.rs).
- **G5 — Accepted decisions.** [Outbound HTTP](../../docs/decisions/outbound-http-decisions.md), [outbound machine authentication](../../docs/decisions/outbound-machine-authentication-decisions.md), and [backend library selection](../../docs/backend-library-selection.md) remain binding.

## Non-goals

- Optimize client construction, service discovery, or execution of provider grants. Their existing behavior remains part of the comparison boundary where it affects caller results or counts.
- Change caller APIs, provider policy, application retries, existing Droplets, release state, remote Git state, or deployment.
- Claim production performance from the temporary benchmark.

## Decisions, assumptions, and reopen triggers

- **Delegated decision — Performance interpretation.** Keep the simpler correct path when the gain is inconclusive under comparable conditions. The comparison should explain observed variance and trade-offs. **Reopen if** matched evidence shows a repeatable improvement in the relevant metric and preservation of the applicable guarantees.
- **Delegated decision — Pool identity.** Preserve the fixed-URL disposition in R7. **Reopen if** evidence of mixed default-port spellings and a compatible connection-level benefit is available.
- **Bounded assumption — Benchmark authority.** The requested budget and teardown authorization cover one new CPU-optimized Droplet and its benchmark. **Reopen with Intake if** the benchmark needs another target, charge-bearing resource, or recovery beyond that envelope.

