# Outbound machine authentication decisions

<!-- template:begin outbound-auth:docs-outbound-machine-authentication-decisions -->
Stage 10.8 library and lifecycle decisions, recorded 2026-09-26.
[Guide](outbound-machine-authentication.md) owns adoption and observable
behavior. This record retains the accepted choices and their reopen conditions.

## Selection and cost

| Decision | Alternative and decisive evidence | Accepted cost and reopen condition |
| --- | --- | --- |
| `oauth2` 5.0.0, defaults disabled, no transport feature | Handwritten protocol duplicates Basic encoding, form construction, and standard response handling. `openidconnect` 4.0.1 adds discovery/JWT beyond this grant; `yup-oauth2` 12.1.2 has no generic Basic client-credentials authenticator. | General protocol dependencies remain even for this small grant. Reopen if resolved admission fails or an actual required provider cannot use the supported hook. |
| Existing `infra-outbound-http` through oauth2's async HTTP hook | oauth2's default reqwest pulls 0.12 alongside workspace 0.13.5. A separate direct reqwest client repeats fixed-origin/deadline/body/observation policy. | A narrow request/response conversion preserves the current transport proof. Reopen only if its supported API prevents a required invariant. |
| Existing `moka` 0.12.16 `future`, per-owner one-key cache | A Tokio mutex alone serializes failures into repeated fetches; a handwritten flight/notification state machine duplicates Moka's shared initializer and cancellation recovery. | Tiny owner policy around supported `try_get_with` and `Expiry`; no general cache wrapper. Reopen if library behavior contradicts accepted cancellation or failure fan-out proof. |
| One new provider crate, independent of inbound authentication | Extending inbound auth joins separate trust and credential lifetimes; placing OAuth in outbound HTTP makes an optional protocol a dependency of every bare HTTP consumer. | Explicit crate/profile pruning keeps independent adoption; remove speculative traits and unused registry/generator paths. |

Registry/maintenance evidence: oauth2 5.0.0 released 2025-01-21 (MIT OR
Apache-2.0, repository unarchived, pushed 2026-02-22); openidconnect 4.0.1 released
2025-07-06 (MIT, unarchived, pushed 2025-11-08); yup-oauth2 12.1.2 released
2026-01-07 (MIT OR Apache-2.0, unarchived, pushed 2026-02-06); Moka 0.12.16
released 2026-08-09 (MIT OR Apache-2.0 plus Apache-2.0, unarchived, pushed
2026-08-09). This reuses Definition's registry/repository inspection, not a new
maintenance certification.

[oauth2's manifest](https://github.com/ramosbugs/oauth2-rs/blob/5.0.0/Cargo.toml)
requires rand 0.8, sha2 0.10, thiserror 1, base64 below 0.23, chrono, and
serde_path_to_error. Baseline lock already contains rand 0.8.8, sha2 0.10.9,
compatible base64, chrono, and serde_path_to_error, plus patched crossbeam-epoch
0.9.21 for Moka. The baseline has only thiserror/thiserror-impl 2.0.20: oauth2
adds the 1.x family alongside it, an accepted transitive duplicate required by
the chosen protocol library. Thus source evidence predicts no new transport
or cryptographic family; it does not claim a resolved candidate graph. Admission
must inspect the actual locked tree/features, keep rand at least 0.8.6 and
crossbeam-epoch at least 0.9.20, and pass existing advisory/license gates without
waivers. OAuth MSRV 1.65 fits workspace Rust 1.98. No unrelated version bump is
needed. Disabled profiles prune actual dependency reachability, including OAuth's
older sha2/thiserror families only when no other retained consumer needs them;
never delete a shared workspace utility declaration merely because OAuth is off.

## Resolved dependency graph

The implementation lock admits `oauth2` 5.0.0 with no OAuth transport feature.
Its only new registry packages relative to the stage base are `oauth2`,
`thiserror` 1.0.69 and `thiserror-impl` 1.0.69; the existing 2.0.20 error family
remains. The protocol uses the already locked rand 0.8.8, sha2 0.10.9 and base64
0.22.1; Moka retains crossbeam-epoch 0.9.21. No second reqwest family is added.
OAuth enables URL's serde feature; this is an explicit feature cost of the
protocol library. Locked offline feature-tree inspection confirms that the
adapter is OAuth's sole workspace consumer and no OAuth default/transport
feature is enabled. Advisory/license gates and executable profile proof remain
CI-owned; these graph observations do not claim their success.

## Supported extension points and named custom gaps

Use BasicClient with token endpoint set, explicit BasicAuth, nonempty secret,
`exchange_client_credentials`, scopes, and `add_extra_param("audience", ...)`.
The [protocol source](https://github.com/ramosbugs/oauth2-rs/blob/5.0.0/src/endpoint.rs)
constructs encoded Basic credentials and a form POST. The hook receives an
absolute `Request<Vec<u8>>`: verify it is the configured endpoint, project only
its path/query into the fixed-origin transport, mark Authorization sensitive,
and convert bounded response bytes back for oauth2. Do not expose raw library
errors or attach them as sources.

The standard parser owns mandatory fields, optional expiry, media type, and
unknown members. Compare the library token-type representation case-insensitively
with `bearer`; do not depend solely on a case-sensitive enum match. Header
projection checks the RFC 6750 bearer grammar and `HeaderValue` construction;
this is the only token-text rule beyond the library, required for a valid wire
Authorization value. Retain only that sensitive header and expiry metadata,
not the parsed token object, extras, refresh token, or provider error text.

The template-owned gaps are: fixed-transport conversion; immutable-owner binding;
monotonic reuse/expiry policy through Moka `Expiry`; conditional 401 eviction;
Authorization injection; config and initializer integration; sanitized outcomes.
There is no custom protocol serializer/parser, flight state machine, retry loop,
resolver, background refresher, or general token-source abstraction.

## Cache, budgets, and finality

Use `moka::future::Cache<(), Arc<CachedCredential>>`, capacity one, with the
closed acquisition failure as the initializer error. Every successful
initializer returns `Ok`; `Expiry` alone decides retention. A credential without
a future reuse cutoff receives a zero lifetime: Moka hands the initializer value
to already coalesced waiters, then treats the entry as expired
(`expiration <= now`), so later calls acquire again. No success travels through
the error channel.

Moka's [initializer source](https://github.com/moka-rs/moka/blob/v0.12.16/src/future/value_initializer.rs)
shares the same error by Arc and removes the waiter on result. Dropping the
initializer marks it abandoned; a surviving waiter can evaluate its own
initializer. Outer `timeout_at(caller_deadline, cache_wait)` protects each
caller; each elected initializer calculates `min(caller_deadline, start+5s)`.
No attempt is retried after a completed failure. Abandonment replacement is
library behavior, not a detached application retry. Bound all active callers
by the existing inbound/job admission and their deadlines; the only cache key
is unit, so cache cardinality cannot grow with caller input.

`CachedCredential` has private sensitive header and optional Tokio monotonic
hard expiry. Representable positive expiry is `acquisition_start + expires_in`.
Its cache cutoff is `hard_expiry - 10s`; if the cutoff is already reached,
return non-retained success while hard expiry is still future. Missing/overflow
expiry follows that same non-retained route, while zero/already-expired lifetime
is invalid. Before resource dispatch, check the hard boundary again and refuse
an expired value as a timeout without resource I/O; the provider response was
valid, so it is not reported as invalid. There is no fallback to a prior credential.

Moka `Expiry` returns the remaining duration to the fixed reuse cutoff on
create/update and preserves the remaining duration on read. It does not slide
expiry. Moka's internal clock is not Tokio's paused clock; test code must not
claim cache eviction from advancing Tokio time alone. Use the final hard-expiry
check as the safety authority, with actual cache expiry or supported isolated
policy proof for reuse. Avoid a new clock trait or runner solely for this test.

A resource 401 evicts the credential that request used through Moka's
`entry().and_compute_with`, removing it only while it is still the cached value,
so a concurrently acquired replacement survives. The response is returned
without replay; the next operation acquires anew. This follows Spring Security's
authorization-failure handler rather than Go's keep-until-expiry, so a revoked
or rotated token does not fail every call until its provider lifetime ends. A
403 is a permission result and keeps the credential.
The ten-second rule is a refresh preference, never a minimum accepted token TTL.

The existing resource `Operation` is forwarded unchanged after acquisition.
The token client uses constants: one active attempt, five seconds, 64 response
headers, 1 MiB encoded body. One MiB matches the existing provider envelope and
allows provider extras without a token-size policy; 64 counts metadata rather
than pretending reqwest exposes a header-byte limit. No claims about measured
latency, memory, or provider capacity are made by these bounds.

## Ownership and proving surfaces

Typed configuration owns key presence, RFC scope representation, safe diagnostic
context, and fixed-endpoint syntax; adapter construction independently admits
direct options. Neither owns the other's dependencies: config uses `url`, while
the adapter receives primitives/SecretString through composition and does not
depend on service-config. Normal config validation runs in every existing binary;
there is no eager token call or extra service lifecycle field.

Runtime errors separate caller Authorization conflict, acquisition failure,
and existing resource transport failure. Acquisition reasons and all public
Debug/Display are closed. Record `oauth2_token_acquisitions_total{outcome}` once
per elected attempt, with finite success/timeout/transport/limit/rejected/invalid/
cancelled outcomes. No scope/audience/URL/integration label or response content
is emitted. Existing resource transport error policy remains unchanged.

The production adapter's local token/resource-server proof covers encoding,
audience and scope omission, permissive RFC success parsing, coalesced success/
failure, expiry and non-retained responses, cancellation replacement, per-waiter
budget, owner isolation, Bearer injection, 401 eviction, and 401/403 without
replay. Reuse existing TLS/transport tests unless that implementation changes.
Negative proof
covers Authorization conflict, secret files, safe diagnostics, token redirect,
limit/timeout, and absence of resource dispatch. Test constructors remain
`cfg(test)` or the existing dev-only `test-support`; no production HTTP bypass.

Initializer wiring adds `outbound_auth` to argument, environment, state, inventory,
sync/migration and profile-owner paths, preserving old locks with default `none`.
Effective HTTP prerequisite selection is saved, not inferred differently during
sync. Extend dependency reachability pruning for the new crate and feature edges.
Profile proof uses representative OAuth-only/no-DB, OAuth+JWT, OAuth+introspection,
and the existing maximal compatible service graph, reusing unchanged no-OAuth
coverage. Inspect retained and removed graphs for OAuth and shared-Moka/HTTP
reachability. Do not multiply by every harness/database/profile permutation or
repeat identical full builds; harness projections remain separate static proof.
Heavy validation remains CI-owned.

The second landing stage merges current main and adds concrete gRPC composition,
with the same private-token ownership, deadline accounting, and unauthenticated/
permission-denied pass-through. No gRPC dependency or unused interface is added
in this stage before that boundary exists. Exact-head CI and whole-result review
belong to delivery; no deployment or image publication is authorized.
<!-- template:end outbound-auth:docs-outbound-machine-authentication-decisions -->
