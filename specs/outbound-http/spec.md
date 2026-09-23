# Specification: optional bounded outbound HTTP

Status: ready. Definition owner: stage-10.2 Definition. Baseline:
`098b4ab18dd5b2d158a94e126798d8cc429ad735`.
Requester meaning: [Intent](intent.md). Decision-changing evidence:
[research synthesis](research/synthesis.md). This contract owns observable
behavior; Technical Design owns crate placement, exact Rust API, and mechanism.

## Outcome and selection

`OUTBOUND_HTTP=none|bounded` is an independent initializer selection; `none`
is the default. `none` leaves the derived service's current behavior and
dependency graph without a bounded outbound runtime pack. `bounded` retains a
complete, usable public-HTTPS client pack and its guide and tests. The selected
pack alone makes no network call at startup, adds no provider destination,
credential, route, readiness dependency, or automatic request. It does not
turn on inbound authentication. The initializer records the selection in its
complete lock and refuses unknown selections before changing the target.

The selected pack gives a provider adapter one reusable client per fixed
dependency. Provider adapters retain endpoint configuration, credentials,
operation meaning, response parsing, retry eligibility, provider error mapping,
telemetry, and any readiness consequence when a real dependency is later
introduced. A generated provider client may use this transport only through a
seam that cannot bypass its policy. A service needing private networking or an
egress proxy must make a separate integration decision; this profile does not
silently weaken its public-address rule.

## Destination and connection admission

Constructing a client requires one valid, absolute HTTPS base URL. It refuses
userinfo, query, fragment, missing host, control characters, and non-public IP
literals before any network effect. A base path may be retained for provider
URL composition but does not authorize another authority. Each operation may
vary method, path, query, and explicitly supplied provider headers/body; it
must target the same canonical HTTPS host and effective port. A different
scheme, host, port, userinfo, fragment, or HTTP `Host` override is refused
before DNS or connection. No caller-controlled destination or unrestricted raw
`reqwest::Client` escape is exposed.

For every new connection to a DNS hostname, the actual answers supplied to
the connector are checked after resolution. An empty answer set, lookup
failure, or any non-public address in a mixed set refuses the attempt. Only
admitted addresses may be passed to the connector; a prior check followed by
unconstrained re-resolution is not sufficient. Literal IPv4, literal IPv6,
IPv4-mapped IPv6, private, loopback, link-local, multicast, unspecified,
documentation, reserved, and ambiguous special-purpose destinations follow
the public-global predicate. No fallback resolver or silent private exception
is allowed. The original configured hostname remains the TLS/SNI and
certificate-verification name; normal chain and hostname verification remain
enabled. Reused connections, if supported, are connections already admitted
at their establishment and cannot change authority.

Ambient/system proxy settings do not affect this client, and it never tunnels
through a proxy. It does not follow redirects or automatically retry any
request, including protocol-level retries. A 3xx response is one bounded
response for the caller to interpret, not permission to contact `Location`.
The default does not carry cookies or referer state across operations.

## Finite work and returned data

The provider supplies positive finite client ceilings for maximum active
operations, absolute elapsed operation time, caller-supplied request header
count and aggregate bytes, response header count and aggregate bytes,
buffered request-body bytes, and response-body bytes. Zero or overflow does
not disable a limit; invalid limits refuse construction. An operation may
narrow its time or response-body ceiling but cannot exceed the client ceiling.
No transport policy value is taken from a caller's inbound HTTP request or
from ambient environment variables. The shared client has no streaming
request or response escape in this stage.

An excess active operation is refused immediately without a queue and without
starting DNS or network work. An admitted operation holds its slot through
the complete bounded response-body read or until failure, timeout, or
cancellation, then releases it exactly once. An already expired parent or
operation deadline is refused before sending. One effective deadline bounds
resolution, connection, TLS, response headers, and body read, and never
exceeds the enclosing request's remaining budget. If an inbound request owns
the operation, its adapter reserves enough time for the enclosing response;
the shared client never extends the parent deadline. Dropping/cancelling the
operation stops its caller-owned work, releases admission, and cannot leave a
detached lookup continuing past the owning runtime's shutdown budget.

After reserved propagation headers are removed, caller-supplied request
headers are checked before send: each field value counts separately and the
aggregate counts `name.len + value.len + 4` bytes per field (`: ` and CRLF).
Generated protocol fields such as `Host` and `Content-Length` are not claimed
as part of this application-supplied byte count. HTTP/1 response header count
is capped during parsing. Before returning any response body, the parsed
response headers are checked with the same per-field aggregate formula and
duplicate-field counting. A response above either limit is rejected and no
body or headers are delivered as a successful result. This application-visible
aggregate limit is a post-parse check; it makes no claim of a separately
configurable HTTP/1 parser allocation ceiling. Header wait is bounded by the
effective operation deadline.

Buffered request bodies above their configured byte cap are rejected before
send. Response bodies are consumed only up to their cap; an advertised
`Content-Length` above it rejects early, while unknown or chunked lengths
fail if the framed response yields one byte above the cap. A truncated frame
is a transport failure. Exact-limit framed bodies succeed, and no oversized
framed body is reported as truncated success. Bytes sent by a peer after a
complete HTTP response frame are outside that response and are not promised
to be detected. The shared client disables transparent decompression and
counts the bytes it returns. A provider that accepts compressed
representations must own separate bounded decoding; the shared client does
not claim a decoded-size guarantee. HTTP status and bounded headers/body are
returned for any status, including non-2xx, so provider error interpretation
stays with the adapter.

Failures retain distinctions that an adapter needs for recovery: invalid
configuration/target, denied address/authority, capacity refusal, timeout or
parent cancellation, oversized request/response, and transport failure.
Errors, logs, metrics, and diagnostic output do not echo credentials, URL
userinfo, query/body content, response content, or DNS answer data. The
client does not fabricate an inbound HTTP Problem/status; the invoking feature
or provider adapter maps failures through its existing boundary.

## Propagation and auth coexistence

The outbound request removes case-insensitive `traceparent`, `tracestate`,
`baggage`, and `X-Request-ID` from caller-provided headers before limit
checking or transmission. It also removes `Accept-Encoding` so a caller
cannot opt into implicit compression handling; automatic referer generation
and ambient cookie/proxy state are disabled. Explicit provider credentials
such as `Authorization` remain the provider adapter's responsibility and are
never derived from inbound bearer state. This contract promises no automatic
outbound trace propagation.

The current `infra-bearerauthn` transport remains auth-private. JWT discovery,
JWKS refresh, introspection body/JSON rules, 200-only response requirement,
three-second provider cap, 100 ms request reserve, 32-exchange admission,
error taxonomy, per-lookup cancellation/join, and no-replay behavior retain
their accepted semantics. Its documented header-count limit must not be
relabelled as an aggregate header-byte limit. A common public-address rule may
be shared only if the existing auth denials and cancellation evidence remain
valid; auth-specific transport policy is not generalized merely because both
clients use reqwest and Hickory.

## Profile, proof, and completion boundary

The source template carries profile markers for every selected-only Cargo,
Rust, initializer, validation, and local-documentation owner. Default
initialization physically removes the pack; selected initialization retains
every owner, valid links, and a buildable/testable graph. A selected pack also
works with both database choices, all three current auth choices, and every
supported harness choice. Profile selection must preserve unrelated target
work and the initializer's full locked metadata, formatting, and OpenAPI
preflight before any target mutation. Sync follows the selected target lock
and cannot restore a pruned pack as portable tooling.

Extend the accepted [validation boundary](../../docs/template-sync.md#validation-boundary):
the current binary outbound choice makes 96 canonical DATABASE × AUTHN ×
OUTBOUND_HTTP × harness projections and 12 unique runtime graphs. Fast
projection checks establish transformation validity and equality of
non-harness runtime/contract inputs. Public initializer and Rust build/test
proof run once per unique runtime graph, not once per harness. New negative
fixtures address the changed target, DNS-to-connect, limits, propagation,
proxy, cancellation, and selection/refusal surfaces; existing auth and
initializer receipts are reused only where exact inputs and claims remain
equivalent. Local checks do not claim remote CI or deployment.

Outcome falsifier: a client that checks DNS in advance but lets reqwest dial a
new answer, a count-only header check described as a byte ceiling, or an
unselected output retaining the new crate would fail this stage. In a
representative operation, a selected service constructs one public HTTPS
client, rejects a mixed public/private DNS answer before connection, sends a
same-authority request after removing inbound correlation fields, accepts an
exact-limit body, rejects an over-limit chunked body, and releases its
operation slot; existing auth verification remains unchanged.

Reopen Intake only for a changed desired outcome or authority. Reopen Research
for a changed reqwest/Hickory contract, IANA policy, or Go/source baseline.
Reopen Specification for a new target class, streaming/decoding semantics,
different propagation policy, or a different supported selection. Technical
Design next chooses the crate/API and resolver, header-check and cancellation
mechanisms, shared-policy placement, profile marker inventory, and proof route
without changing these observable rules.
