# Bounded outbound HTTP: system design

Status: ready; Technical Design Review PASS. Authority: [Specification](../spec.md),
[Intent](../intent.md), and [research](../research/synthesis.md). This phase fixes
mechanism and ownership; it does not implement or plan tasks.

## Decisions and alternatives

Use the already resolved reqwest 0.13.5, Hickory 0.26.3, url 2.5,
Tokio 1.53.1 and tokio-util 0.7.19. Their supported extension points own HTTP,
TLS, URL canonicalization, DNS protocols, clocks, permits and task tracking.
No new registry dependency, feature family, runtime, or toolchain is required.
The Definition research records current release/maintenance and API evidence.
The installed reqwest `dns/resolve.rs` confirms `Resolve` returns the exact
address iterator consumed by the connector, wraps errors with their source,
and lets the URL port override returned port zero. `error.rs` retains that
source chain. These are version-specific implementation evidence, to be
refreshed on an upgrade; they do not substitute for the eventual negative proof.

Add `infra-outbound-http` for the fixed-authority buffered request contract.
Extract `infra-egress-dns` from auth's existing DNS module for the shared
public-address admission and cancellation-aware Hickory runtime. Both auth
and outbound depend on that leaf. The leaf does not import either consumer,
know credentials, choose HTTP limits, map provider failures, or register
readiness. Two real consumers justify its independent crate. Housing it in
auth would force auth into outbound-only services; housing it in outbound
would retain the outbound pack in auth-only services. Copying the tracked
runtime duplicates security and shutdown corrections. A pure predicate-only
extraction leaves that material duplication. Preserve the existing mechanism
and its tests, changing only neutral visibility and error adaptation.

Raw hyper would make the template own another HTTP/TLS assembly and conflicts
with the accepted reqwest choice. Reqwest's built-in resolver does not enforce
answer admission or the existing task custody. Hickory's ordinary Tokio
runtime does not supply the accepted per-lookup cancel/join policy. Thus the
small existing runtime adapter remains necessary. Do not invent a general
transport trait or configurable resolver/dialer in the production API.

Choose HTTP/1 and no idle connection reuse (`pool_max_idle_per_host(0)`).
This retains a straightforward per-connection admission and cancellation
boundary. Cost: TLS/DNS work per exchange; no throughput claim is made.
Reopen Design with measured need before adding pooling or HTTP/2. The selected
profile has no runtime configuration section, startup call, readiness probe,
or automatic wiring. A real provider later supplies those through bootstrap
and its own validated config, under the existing integration owner.

## Public transport API and data ownership

`infra_outbound_http::Client` is cloneable with shared immutable policy and a
shared `Arc<Semaphore>`; it never exports reqwest's client or builder. Public
value types are `Limits`, `Request`, `Operation`, `Response`, and `Error`.
Use reqwest re-exported `Method`, `HeaderMap`, and `StatusCode`, `Vec<u8>` for
buffered bodies, Tokio `Instant` for deadlines, and `CancellationToken` for
explicit parent cancellation. There is no stream or response upgrade handle.

- `Client::new(base: &str, limits: Limits, tracker: TaskTracker,
  shutdown: CancellationToken) -> Result<Client, Error>` validates and snapshots
  the base and policy, reads resolver configuration, and builds the client
  without DNS/network I/O. The passed tracker/token belong to the runtime
  that owns this adapter, normally bootstrap's existing tracker and a child
  of its root token. The client does not close that shared tracker.
- `Limits` carries `max_active`, `operation_timeout`, `request_header_count`,
  `request_header_bytes`, `response_header_count`, `response_header_bytes`,
  `request_body_bytes`, and `response_body_bytes`. Every count/byte value must
  be positive, representable in the downstream API, and fit its checked
  arithmetic; `max_active <= Semaphore::MAX_PERMITS`. Duration must be
  nonzero and checked-addable to the construction/current Tokio instant.
  No unlimited sentinel/default is introduced.
- `Request` carries an explicit method, target string, headers and body.
  Target is a provider-owned relative reference or absolute URL. Resolve
  relative references using `Url::join` against the configured base, including
  its standard trailing-slash semantics, then admit the resulting authority.
  Reject raw controls, surrounding whitespace, any userinfo (including empty
  `@` syntax), and fragment before a request can run. A same-authority query
  is allowed; a different scheme/host/effective port is denied. Adapter code
  must never source the target authority from inbound data. No raw target,
  header or body is exposed by derived Debug on request/client/response.
- `Operation` carries a mandatory `parent_deadline: Instant`, a mandatory
  cancellation token, optional narrower timeout and optional narrower
  response-body byte ceiling. A supplied zero or value above the client
  ceiling is `InvalidConfiguration`, not a silent clamp. Effective deadline
  is `min(parent_deadline, start + selected_timeout)` with checked arithmetic.
  A background caller supplies its own finite enclosing deadline/token.
- `Client::execute(&self, request: Request, operation: Operation)` returns
  `Result<Response, Error>`. `Response` contains status, bounded headers and
  body for every status; it has no parser/provider semantics.
- `Error` is a closed, content-free enum: `InvalidConfiguration`,
  `InvalidTarget`, `Denied`, `AtCapacity`, `Timeout`, `Cancelled`,
  `RequestHeadersTooLarge`, `RequestBodyTooLarge`,
  `ResponseHeadersTooLarge`, `ResponseBodyTooLarge`, `Transport`.
  Display/Debug are static labels and source() exposes no reqwest, URL, DNS,
  header or body data. Errors do not create inbound Problems. HTTP parser
  refusal before headers exist remains `Transport`; application-observed
  excess is the corresponding size variant. This preserves size distinctions
  without parsing dependency error strings.

Construction validates raw and parsed base URL: absolute HTTPS, present host,
no userinfo, query, fragment, controls or whitespace. Canonical authority is
`Url::host()` plus `port_or_known_default()`; explicit 443 equals default 443,
IDNA/case normalization comes from url, while a trailing-dot spelling remains
a distinct configured host. Literal IPs go through the same public predicate
before reqwest can bypass DNS. Operation admission rejects every `Host` field
before network, including one nominally equal to the configured authority.
No `resolve`, `resolve_to_addrs`, proxy, custom roots, or insecure TLS builder
option is exported.

## Material request flow and custody

1. Provider owns desired method/path/query/credentials and buffered body. Client
   validates target and operation and strips every value of `traceparent`,
   `tracestate`, `baggage`, `x-request-id`, and `accept-encoding` case-insensitively.
   HeaderMap canonical names implement this removal. Count each remaining
   field value separately, using checked `name.len + value.len + 4` and checked
   aggregate addition. Overflow refuses. Check body length before send.
2. Before polling send, reject cancelled root/parent or expired deadline,
   then `try_acquire_owned` with no queue. Recheck deadline/cancellation after
   synchronous admission. The permit stays in the caller-owned async scope
   through complete body consumption; every return, timeout and drop releases
   it. Capacity refusal cannot start a lookup or a socket.
3. The private reqwest builder sets rustls, HTTPS-only, HTTP/1-only,
   `redirect(Policy::none())`, `retry(reqwest::retry::never())`, `no_proxy()`,
   `referer(false)`, all four `no_*` decompression flags, zero idle pool,
   `http1_max_headers(response_header_count)` and client timeout as a backstop.
   Do not enable a cookie store. A single outer `timeout_at` encloses send,
   header admission and the body loop; biased cancellation arms observe the
   root and operation tokens before network progress. The same absolute
   deadline is never restarted between stages.
4. For DNS names the installed shared `PublicResolver` supplies reqwest the
   post-lookup admitted answer vector at port zero; URL port and TLS hostname
   remain unchanged. It reads system DNS config once, disables hosts-file
   resolution, and constructs a fresh Hickory resolver per lookup as auth
   does today. An empty set or lookup failure refuses; any disallowed member
   rejects the entire set before any HTTP connection. DNS server addresses
   are resolver infrastructure, not HTTP destinations, and do not go through
   the public target filter. No fallback resolver or second resolution exists.
5. Once headers arrive, compute count and aggregate from all parsed values
   before exposing headers or reading a successful body. Count is also capped
   by reqwest during HTTP/1 parsing. Aggregate is explicitly post-parse; it
   does not bound parser allocation. Check advertised Content-Length early.
   Append response chunks only after checking `chunk.len <= cap - body.len`.
   On first overflow discard the response and return size failure. Exact cap
   succeeds only after framed EOF; truncated frames are Transport. Reqwest
   may allocate an internal chunk larger than the remaining allowance, but
   the returned/accumulated body never exceeds the cap. Disable decompression;
   compressed bytes stay encoded and separate decoding belongs to the adapter.
6. Return status/headers/body after framing completion, or a static error.
   A 3xx neither resolves nor follows Location. An error after sending might
   follow a provider-side effect; the client never replays, reconciles, or
   declares that effect absent. Provider policy owns any next attempt.

The leaf exports `PublicResolver::new(tracker, cancel)`, `admit_address(IpAddr)`
and a content-free `ResolveError` distinguishing configuration/lookup/denied/
cancelled. Its `Resolve` errors retain concrete source identity. Outbound maps
`Denied` by typed source-chain downcast; lookup/config runtime failure maps
Transport, and top-level timeout/cancel selection maps their own variants.
Never classify DNS denial by matching error strings or mutable client-global
last-error state. Auth maps every leaf error to its existing `Failure::Unavailable`.

The shared address owner retains the existing IPv4 predicate and explicit
IPv6 registry exceptions, then closes concrete gaps exposed against the ready
Specification. On 2026-09-23, IANA's [special-purpose registry](https://www.iana.org/assignments/iana-ipv6-special-registry)
marks 6to4 globally reachable as N/A; its [address-space registry](https://www.iana.org/assignments/ipv6-address-space)
reserves IPv6 space outside 2000::/3 except specifically assigned subranges.
The existing auth predicate admits 2002::/16, reserved examples such as ::2,
fec0::1 and 4000::1, and well-known translation addresses embedding private
IPv4. That contradicts this stage's reserved/ambiguous/embedded-address rule.

Select this explicit predicate delta: first normalize IPv4-mapped IPv6 and
apply the existing IPv4 rule. For 64:ff9b::/96, apply that same rule to the
embedded final 32 bits. For other IPv6, require 2000::/3, then apply the
existing IPv6 special-range exclusions and explicit 2001::/23 exceptions,
and additionally deny all 2002::/16. Existing denials and explicit public
exceptions remain unchanged. This admits ordinary global IPv6 and public
well-known NAT64 while denying private embedded destinations and reserved
space; it promises address policy, not actual routability. Future IANA
allocations reopen this owner. Both transports use the one predicate with
focused new negative proof and retained exception/parity proof. Auth's
observable delta is stricter destination refusal mapped to the same
Failure::Unavailable; HTTP, identity, limits, deadlines and cancellation do
not change. No accepted auth promise requires these formerly admitted
non-public/ambiguous destinations. Do not copy the old predicate verbatim
or leave the choice to Implementation.

## Lookup cancellation and request deadline reuse

Preserve auth DNS's exact ordering: acquire tracker scope before cancellation
check; create per-lookup child token; retain scope in RuntimeProvider/Spawn
clones; install cancel-on-drop after resolver construction so cancellation
signals before resolver handle teardown. Every Hickory background future runs
inside `tracker.spawn` selecting root/lookup cancellation, with no untracked
spawn. TCP connect and UDP bind remain cancellation-aware. Dropping the
reqwest resolving future cancels its lookup; global shutdown cancels all.
Admission permit release is synchronous on operation drop; DNS wrappers may
need a runtime scheduling turn to finish, but remain visible to the runtime
tracker until done. Bootstrap's existing cancel/close/wait shutdown budget is
the join authority; no new teardown stage is added. The client is reusable,
not a new standalone background service.

`infra-http::harden::RequestDeadline` already contains the absolute inbound
instant immediately before the tower timer. Retain its existing stamping
order and auth consumption; make the type and `at()` public through
`infra_http` while keeping its field and constructor private. Its existing
three marker regions move to `request-budget`, selected for auth OR outbound.
Export only under that marker. An inbound adapter obtains the stamped value,
subtracts its provider-owned response reserve, and supplies the resulting
instant to Operation. Missing stamp denies a request-owned integration rather
than inventing a fresh budget. No blanket numeric reserve is chosen; the
100 ms auth reserve and auth three-second cap remain unchanged.

## Auth preservation and proof boundaries

Auth's provider.rs retains builder, HTTP limits, JSON/200 checks, exact
provider URL semantics, failure mapping, no replay, no idle pooling, 1 MiB
body cap and its existing fixture API. JWT refresh and introspection's
32-exchange admission stay untouched. Only DNS construction/admission imports
and leaf error mapping move; the existing auth raw-answer fixture stays
beside auth and calls the shared predicate. Move DNS predicate/tracked-runtime
unit proof with the implementation, and retain auth TLS/cancellation/provider
proof with its current owner. The auth header claim remains count-only.

Implementation selects cases and commands at the proving owners in the
[ownership map](ownership.md). Required behaviors include DNS-to-connector
mixed-set denial, literal/mapped denial, unchanged TLS name/root checking,
proxy/redirect/no-replay and correlation removal, duplicate header accounting,
large one-field aggregate rejection, exact/over-limit framed body,
non-2xx bounded return, admission release and cancelled/expired pre-send
refusal, tracked DNS completion after drop, and auth parity. Fixtures remain
local; private loopback mapping and test roots exist only in cfg(test) owners,
never in a production constructor or public feature. No live provider is
required. Reuse existing auth fixture material rather than create credentials.

Reopen Research if a pinned API or error-chain assumption fails. Reopen Design
for changed pooling/protocol/ownership/cancellation mechanism. Reopen
Specification for changed destinations, byte semantics or propagation.
