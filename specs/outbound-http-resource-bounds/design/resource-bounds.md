# Outbound resource bounds: Technical Design

Fixed behavior: [Specification](../spec.md), admitted by the
[Definition result](../definition-result.md). Base: `67be869`.
This design selects mechanisms and existing owners; it does not authorize
implementation, new profiles, dependency changes, or measured-performance claims.

## Buffered body ownership

Retain `http_body_util::Limited` and the existing exact-size-hint precheck in
`infra-outbound-http::Client::exchange`. Replace `collect().to_bytes()` with
incremental `BodyExt::frame()` consumption into one initially empty `Vec<u8>`.
Only data bytes are copied. Each frame/data handle leaves scope before polling
the next frame; empty data and trailers are dropped and polling continues to
EOF. Never transfer an input `Bytes` into the output, including the one-frame
case. At EOF, `Bytes::from(vec)` owns the accumulator allocation, with no input
frame backing retained. Response parts pass through unchanged.

The existing `Limited` error mapping remains the sole byte-limit/transport
classification: equality succeeds, the first over-limit data frame fails, and
later transport errors retain their cause. All polling and copying remain in
the existing exchange future and timeout; no background collection, new timer,
or retry appears. The existing observation guard owns terminal reporting.

Let `L` be the payload ceiling, `n` the accumulated length, and `R` the length
after an admitted nonempty frame. `Limited` establishes `R <= L`; avoid overflow
when deriving `R`. Grow only if `R` exceeds the current Vec capacity `C`, using
`reserve_exact` with a target `max(R, min(L, 2*C))` and additional amount
`target - n`; doubling uses saturating arithmetic. The first target is the
first nonempty frame length. Appending then uses the reserved storage.

Every requested payload allocation is at most `L`. During a relocating growth,
the old and new requested allocations total at most `2L`; between growths and
in the returned body there is one such allocation. Vector/Bytes metadata is
constant, not proportional to frame count. No reservation uses Content-Length
or the ceiling before actual data arrives. Geometric targets avoid per-frame
exact growth and its quadratic copying cost.

These are **requested payload-storage** bounds. `reserve_exact` permits an
allocator to provide extra capacity; allocator rounding, retained freed arenas,
and process RSS are not bounded by `L`. Add the transport's current frame and
fixed read buffers to the storage above; the current frame may itself refer to
a larger backing allocation, but the accumulator never retains it after that
iteration. This distinction is part of the guide, not an RSS claim. Resolved
bytes 1.12.1 converts a Vec by transferring its allocation (with constant
shared metadata when spare capacity exists), so finalization adds no second
payload copy.

`Limited::collect` is unsuitable because it retains frames. Ordinary automatic
Vec/BytesMut growth does not establish the selected capped request bound;
allocating `L` up front trusts a bound rather than received data. Standard Vec
plus the already declared body/Bytes libraries meets the current requirement
without a custom buffer type, new dependency, or a production abstraction added
solely to inject test bodies. Reopen only if the resolved library ownership
semantics change or a caller needs a streaming API.

## OAuth attempt ownership

Add one `tokio::sync::Semaphore` to the existing `Inner` owned by
`Arc<Inner>` in `Credentials`. Preparation initializes it from admitted
`Options::provider_concurrency`; clones and HTTP/gRPC bindings share it.
Independent preparation creates independent capacity. Do not put this bound
in the transport, cache, binding, or a global endpoint registry.

Both actual fetch initializers (`fetch_service_token`, `fetch_exchange`) create
their existing `AttemptMetric`, then use one small private `Inner` admission
operation: compare the actual attempt's absolute deadline to `Instant::now()`,
return `Timeout` if expired, otherwise `try_acquire` and map refusal to
`AcquisitionError::AtCapacity`. Keep the borrowed permit in the actual fetch
future through assertion signing, form creation, `post_form`, complete body
read, JSON parsing, and `into_token` admission. Only then release it, also by
RAII on error or future drop. A helper that returns the borrowed permit closes
the common policy without wrapping the two request mechanisms or adding tasks.

Admission must be inside the result path recorded by `AttemptMetric::finish`,
so a refusal records `capacity`, not its drop fallback. Each initializer emits
one outcome; waiters emit none. Refusal occurs before signing and before
`token_http.execute`, so it emits no outbound HTTP attempt. Keep the existing
background warning, using the same closed `capacity` label and sanitized fields.

Material flows:

- A reusable service/exchanged token returns before admission. An existing
  service refresh lock or Moka same-subject initialization is awaited with the
  caller's current absolute deadline. Those waiters hold no permit.
- A service miss takes the existing refresh lock, rechecks the cache, and then
  starts the admitted fetch. Successful parsing permits storing the new token;
  refusal/error stores nothing. Releasing the refresh lock permits the existing
  next caller behavior; there is no new capacity retry or capacity queue.
- An exchange miss enters the existing `or_try_insert_with`; only its elected
  initializer admits. Its deadline must include both its own caller deadline
  and the existing five-second acquisition cap. Pass that earlier deadline
  into `fetch_exchange`, rather than allowing the outer waiter timeout to be
  the only expired-deadline check. Failed initialization is not cached.
- A dropped waiter changes no live permit. Dropping the initiating future
  releases its permit through RAII; Moka/refresh-lock leadership transfer stays
  as today, and a replacement initializer admits independently. No detached
  replacement is created. Existing background refresh remains its current task.
- Refresh-ahead retains the current cache token and the existing 30-second
  `refresh_after` retry spacing when admission fails; a foreground hit still
  succeeds. Both grant types consume the same pool. Nothing reserves a slot for
  service credentials or background work.
- HTTP acquisition error stops before resource dispatch; gRPC
  `acquisition_status` adds `AtCapacity` to `UNAVAILABLE`, preserving
  `client credentials unavailable` and the typed status source. Cache eviction,
  subject isolation, Authorization refusal, and remaining resource budget keep
  their existing owners.

The semaphore is never closed or replenished manually. `try_acquire` introduces
no admission waiters; a Tower service limit would put ownership around network
dispatch and miss pre-signing work or parsing lifetime. The declared Tokio
primitive supplies cancellation-safe release, so no custom atomic counter or
permit implementation is justified. The accepted cost is immediate local
refusal under contention, without priority. Reopen if accepted behavior later
requires a queue, process-wide quota, or refresh reservations.

## Configuration, ownership, and compatibility

`OAuthConfig` in `crates/config/src/integrations.rs` owns the nonsecret
`provider_concurrency: u32`, default constant/function 32, and nonzero validation
under `integrations.<name>.oauth.provider_concurrency`. Use a field-local strict
scalar decoder accepting an integer or an integer string. The existing
`authn::deserialize_scalar` untagged typed/text pattern is the precedent; keep
the new decoder within this OAuth profile's markers rather than coupling it to
the optional introspection profile. Plain config-rs u32 decoding is insufficient:
resolved config 0.15.27 rounds float values through `into_uint`. Buffered
untagged typed/text decoding preserves the original scalar kind, rejects
fractions/bools and out-of-u32 values, and must retain the loader's value-free
diagnostic convention. Zero is refused by `OAuthConfig::validate`.

Normal file/overlay/secrets-directory/environment precedence and unknown-key
rules remain unchanged. Missing OAuth stays inert. `Options` independently
rejects zero before client construction. Before `Semaphore::new`, convert the
value with checked `usize` conversion and enforce `Semaphore::MAX_PERMITS`,
returning the existing sanitized `ConfigurationError` for an unsupported value
on that target. Resolved Tokio 1.53.1 uses `usize::MAX >> 3`: every positive u32
fits on the current 64-bit CI/delivery targets; a narrower adopter target must
receive an option error rather than a construction panic. This is a target
capability guard, not another deployment maximum or config-library dependency.

Placement is forced by existing boundaries; no new crate, module, generic
provider interface, or separate ownership-design fork is needed:

| Existing owner/file | Present change and proving surface |
| --- | --- |
| `crates/infra-outbound-http/src/lib.rs` | `Client::exchange` accumulator and retention invariant; existing `src/tests.rs` transport/body cases plus static ownership/growth inspection. |
| `crates/infra-oauth2-client-credentials/src/lib.rs` | Options admission, prepared semaphore, both initializer lifetimes, closed error and metric label; existing `src/tests.rs` acquisition/cache/cancellation fixtures. |
| `crates/infra-oauth2-client-credentials/src/grpc.rs` | Closed overload projection; existing `src/tests/grpc.rs` projection and no-dispatch proof. |
| `crates/config/src/integrations.rs`, `crates/config/src/load.rs` | Typed field/default/strict decode/nonzero rule, config literals, and existing loader source/range proof. |
| OAuth `src/tests.rs` and `src/tests/keycloak.rs` | Migrate all direct Options literals to explicit provider capacity; existing fixture owners remain. |
| `docs/configuration-source-policy.md`, `docs/outbound-machine-authentication.md`, `docs/outbound-machine-authentication-decisions.md` | Field/default, direct-construction adjustment, an explicit `oauth.provider_concurrency -> Options::provider_concurrency` composition example, shared-attempt lifetime and overload behavior, finite metric outcome. |
| `docs/outbound-http.md`, `docs/outbound-http-decisions.md` | Payload versus requested body storage, current-frame caveat, remove OAuth single-flight as a bound on distinct subjects, and correct hyper-util unsent reused-connection replay wording. |

The scaffold deliberately wires no unused OAuth integration registry or example
provider into `crates/service`. Its documented composition root is the adopter's
real integration; show the new field's transfer there and preserve that boundary.
Existing config files work through the default. Rust adopters add the explicit
Options field and handle `AtCapacity` in exhaustive matches. No REST/OpenAPI
change, dependency edge, transport limiter, or unrelated authn refactor follows.

## Proof and handoff boundary

Implementation owns focused test selection under the Specification's falsifiers
and repository validation budget. Existing HTTP fixtures can establish bytes,
EOF/error/deadline behavior; retention and requested-capacity bounds additionally
need static review of the actual loop, not RSS measurements. Existing OAuth
fixtures support occupied attempts and controlled completion across both grants,
hits/waiters, cancellation/replacement, and refresh refusal; do not add a
test-only production mechanism. Config proof must exercise the real loader's
TOML and environment paths, because direct Serde-only proof misses coercion.
gRPC proof observes the sanitized status, typed source, and lack of resource I/O.

At the default, at most 32 actual token attempts hold response bodies of payload
ceiling 1 MiB each. Requested accumulator storage is at most `32L` outside
growth and `64L` allowing all to grow concurrently, plus current transport
frames/read buffers and parser/token storage. Neither figure bounds total
owner memory, cached tokens, or process RSS. No throughput claim follows.
Historical benchmark evidence remains dated/revision-qualified and does not
measure this collector.

Only mechanical implementation choices remain: local variable/helper naming,
focused fixture cases, and the ordinary validation route. Reopen Technical
Design if a lifetime/growth or strict-decoding mechanism cannot satisfy this
map; reopen Specification only if an observable rule cannot be preserved.

Evidence checked: current CodeGraph source at the named owners; locked
http-body-util 0.1.5 `Limited::poll_frame`, bytes 1.12.1 `From<Vec<u8>>`, Tokio
1.53.1 `try_acquire`/`MAX_PERMITS`, and config 0.15.27 scalar deserialization.
The standard-library [Vec reserve_exact contract](https://doc.rust-lang.org/std/vec/struct.Vec.html#method.reserve_exact)
distinguishes requested capacity from allocator-provided space. No build, test,
benchmark, or live-provider validation was run in Technical Design.
