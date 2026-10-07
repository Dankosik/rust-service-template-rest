# Credential refresh technical design

Status: ready. Bounded [D1/D3 dependency review](design-dependency-review.md): PASS. Prior [review](../design-review.md) remains valid for unchanged scope.

Authority: [ready Specification](../spec.md) and [Definition transition](../definition-transition.md).
Evidence: [baseline](../research/baseline.md), source at base
`5927ffbba351af2f7fb8635316bbfa4ae5b31da6`, and the versioned sources below.
This design closes R1 mechanism and R2 documentation ownership. No production
code, tests, builds, provider operations or credential reads are part of this phase.

## D1: reuse the installed fallible randomness source

Use the installed AWS-LC random source for a fresh two-byte sample at each
eligible scheduling point. Decode as `u16`; byte order has no behavioral
significance, but use one explicit order consistently. OAuth and bearer
authentication retain their existing direct `aws_lc_rs::rand::fill` calls.
Messaging obtains the same fallible source through its existing SDK's public
re-export: `async_nats::rustls::crypto::aws_lc_rs::default_provider().secure_random`.
Capture that `&'static dyn SecureRandom` once when configuring the existing
reconnect callback, and call its `fill` for each non-immediate attempt. The
library trait is Send + Sync and the static reference fits the SDK callback's
existing lifetime. No owned RNG state, new trait, singleton or background owner
is introduced. The temporary CryptoProvider's lists may be dropped after the
static random-source reference is extracted; do not rebuild its allocated lists
on every retry. Do not install or read a process-global default provider:
explicit `aws_lc_rs::default_provider()` does not depend on previous TLS startup,
provider installation or whether other crypto features are also enabled.

No manifest, feature, dependency-version or lockfile change is selected.
Implementation removes only T1's attempted direct `aws-lc-rs` manifest addition,
preserves other work, and resumes the existing locked command path. OAuth and
bearer retain their existing direct dependency declarations. The re-export uses
async-nats 0.50.0's already-enabled `aws-lc-rs` feature, which activates
`tokio-rustls/aws-lc-rs`, then `rustls/aws_lc_rs`; Cargo.lock already resolves
rustls 0.23.45 and AWS-LC 1.18.1. There is no accidental reliance on a different
workspace crate enabling the provider.

This corrects a concrete D1 gap: adding the proposed direct messaging edge made
T1's `cargo metadata --locked --offline --format-version 1` exit 101 because a
lockfile update was required. Repository policy requires every Cargo invocation
locked and forbids hand-editing Cargo.lock. The unsupported regeneration step
is withdrawn rather than weakening policy. No Cargo resolution command or build
was run by this bounded design reopen; T1's ordinary feedback verifies the
repaired manifest/code against the unchanged lockfile.

The source is independent library randomness; do not seed a PRNG from a shared
constant, timestamp, token, key, process identity or credential bytes. Do not
introduce owned RNG state, a configuration key, new diagnostic or readiness effect.
If fill fails, disregard all output bytes and choose zero spread. This reproduces
B for NATS, A for OAuth lead, 30 s for OAuth retry and 15 min for JWKS. No retry
of the RNG, unwrap, panic or provider failure mapping is needed.

Version evidence: Cargo.lock resolves aws-lc-rs 1.18.1, async-nats 0.50.0 and
rustls 0.23.45 and backon 1.6.0. The installed aws-lc-rs `src/rand.rs::fill` directly wraps the
library RNG and exposes failure; its documentation is the resolved-version
API authority. rustls `src/crypto/aws_lc_rs/mod.rs::default_provider` exposes a
static `secure_random`; its `AwsLcRs::fill` calls AWS-LC `SystemRandom::fill`
and maps failure to `GetRandomFailed`, retaining failure information needed by
the zero-spread fallback. async-nats `src/lib.rs` publicly re-exports rustls.
The [NATS re-export](https://docs.rs/async-nats/0.50.0/async_nats/index.html#reexports)
and [rustls CryptoProvider fields](https://docs.rs/rustls/0.23.45/rustls/crypto/struct.CryptoProvider.html)
confirm the supported surface. [AWS-LC crate documentation](https://docs.rs/aws-lc-rs/1.18.1/aws_lc_rs/)
and [NATS callback API](https://docs.rs/async-nats/0.50.0/async_nats/struct.ConnectOptions.html#method.reconnect_delay_callback)
corroborate the existing family. Registry source root inspected:
`/Users/daniil/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f`.

| Candidate | Decisive constraint and disposition |
| --- | --- |
| Existing AWS-LC random-byte API, via native rustls re-export for messaging | Selected: already installed and available through current declared dependencies/features; fallible without owned RNG state. Cost is one static interface capture during messaging setup and one tiny draw per eligible schedule. |
| New direct messaging AWS-LC edge | Withdrawn: observed locked metadata requires a forbidden lockfile update path; the existing public re-export provides the same source without this edge. |
| Existing backon 1.6.0 jitter | Rejected for these schedules: its source adds `[0,current_delay)` after the base/cap, whereas R1 requires subtractive NATS/JWKS/lead spread and a narrow additive OAuth retry window. Retain Redis's current use. |
| New direct rand/getrandom/fastrand family or retry framework | No surviving need: these would require another direct declaration and API/feature selection while the existing fill API already supplies the only missing primitive. No upgrade or new-crate research campaign is justified. |
| Standard-library clock/hash pseudo-randomness | Rejected: no standard random-byte API here establishes independent replica samples; clock/hash seeding would introduce unneeded state and weak synchronization assumptions. |
| Shared jitter crate, generic secret/RNG service, or cross-provider helper export | Rejected by deletion test: three small policy owners can use the library directly; removing a shared layer eliminates dependency/profile edges without losing a lifecycle or semantic boundary. |

This is reuse of an installed mechanism, not adoption of a new crate family.
Reopen D1 only if actual retained-profile resolution cannot supply the existing
backend/features or the resolved API changes. No custom general random mechanism
or new Cargo feature is authorized by this decision.

## D2: integer arithmetic and scheduling boundaries

All schedule arithmetic uses `Duration` and monotonic `Instant`. At these call
sites the largest base is 900 s. For sample s in `[0,65535]`, define the subtractive
spread mathematically as `floor(floor(base_ns / 10) * s / 65535)` nanoseconds;
subtract it from the base. Use wide integer intermediates, no floating point
or narrowing before proving the result fits. At s=0 the original base is chosen;
at s=65535 the maximum rounded 10% spread is chosen. Therefore the result is
in `[ceil(0.9*base_ns),base_ns]`, and a positive base never becomes zero.
A source failure takes the same s=0 result. This notation specifies bounded
policy arithmetic, not a public reusable helper interface.

For OAuth retry use `30 s + floor(3_000_000_000 * s / 65535) ns` with the same
failure-to-zero-spread rule. Both endpoints are representable. Finite sampling
is sufficient to spread replicas; it does not promise uniqueness or a particular
measured fleet-load reduction. Independent samples may coincidentally match.

### NATS

`Messaging::connect` installs the supported synchronous
`ConnectOptions::reconnect_delay_callback` after obtaining existing authenticated
options. A private function in `messaging.rs` owns the policy. Attempts 0 and 1
return zero without drawing randomness. For later attempts retain
`B = min(2^(attempts-1) milliseconds, 4 seconds)` using saturating exponent
conversion/power, or an equivalent pre-cap before shifting. Compute B before
spread. Attempts at or beyond saturation, including `usize::MAX`, therefore
stay in `[3.6 s,4 s]`; no wrapping or narrowing can produce a short retry.
Every later callback invocation draws independently, even for equal attempt counts.

SDK owns attempt counting, retry, sleep, connection recovery and eventual
connection. The callback transfers only a `Duration`, no tasks or authority.
Unlimited reconnect, auth callback rereads, startup admission, connection and
request budgets, topology admission and close remain unchanged. No extra retry
layer or `reconnect_to_server_callback` is added.

### OAuth service tokens

`into_token` remains the admitted-response owner. Preserve checked expiry and
reuse cutoff U. For an expiring reusable service token, compute current
`A = min(300 s, U.saturating_duration_since(started)/4)` and draw the subtractive
lead once. Store `U - lead` in `Token.refresh_after`; `Cached` copies this value
when admitting that exact token. Cache hits read it, never resample the lead or
slide the time. `lead <= A <= reusable_lifetime/4` makes subtraction safe and
keeps the threshold before U. Missing expiry and non-reusable short tokens gain
no refresh work. Shared parsing of exchange tokens must preserve their existing
behavior: do not add background exchange work or a random exchange schedule.

At the existing `reusable_service_token` queue attempt, replace only the 30 s
addition with sampled `[30 s,33 s]`. This assignment occurs while the cache state
is owned and before `try_send`; failure or queue refusal retains that spacing.
It is still one queued/pending request with an absolute five-second deadline
from enqueue. Driver lock wait and provider fetch continue to spend that deadline.

At the existing successful `refresh_ahead` completion, independently sample a
retry spacing and retain `max(new_token.refresh_after, completion + spacing)`.
The failure path keeps the spacing established at enqueue, plus existing
completed-failure suppression; it must not acquire a second retry loop. Early
return, stale-token identity checks, capacity failure and shutdown keep their
existing disposition. A foreground miss after U still uses the existing bounded
acquisition regardless of background eligibility. This mechanism never changes
`reuse_until`, `is_reusable`, exact-token invalidation, semaphore admission,
one-second failure suppression, queue ownership, or resource replay policy.

### JWKS

Replace the fixed-period `Interval` in `run_refresh_worker` with one persistent
monotonic deadline waited on through `sleep_until`. Initial deadline is worker
start plus one sampled `[810 s,900 s]` delay. When that deadline branch is actually
selected, request periodic work through the existing `KeyStore`, then set the
next deadline to the current monotonic time plus a fresh independently sampled
delay. Preserve that deadline across unknown-key wakes and their fetches; they
do not reset or resample periodic eligibility.

The existing biased cancellation-first select remains both while waiting and
while fetching, followed by `store.stop()` on exit. The existing pending-ticket
loop remains the sole fetch worker and `finish` remains key-set publication and
waiter-completion authority. An overdue deadline yields at most one periodic
request and rearms from observed time, never by adding to the old deadline or
replaying a count of missed periods. If a fetch or runtime suspension spans a
period, one overdue request may run on returning to the select, as with the
current Delay policy; there is no backlog of catch-up ticks. The selected range
bounds scheduled waiting, not network completion or runtime suspension.
Unknown-key cooldown stays exactly 30 s and remains owned by `KeyStore`.
Last-good keys, fetch bounds, coalescing, sanitized failures and joined process
lifetime are unchanged. No timer task or separate refresh concurrency is added.

## D3: responsibility and file map

Existing component boundaries force placement; root ownership self-review finds
no new ambiguity requiring a separate Rust ownership panel. Apply the earliest
existing owner, using private policy functions rather than a shared module.

| Responsibility | Affected path / exact action | Evidence and semantic owner | Boundary and cleanup | Proof owner / reopen |
| --- | --- | --- | --- | --- |
| NATS delay | `crates/infra-messaging/src/messaging.rs`: install callback and private delay policy through native rustls re-export; `crates/infra-messaging/Cargo.toml`: remove only T1's attempted direct AWS-LC addition, returning to existing dependencies; `Cargo.lock`: unchanged | `Messaging::connect`, async-nats 0.50.0 callback/default source; provider adapter | No bootstrap or auth-reader change; remove reliance on default deterministic callback only | Messaging local behavior proof and existing CI messaging surface; reopen D1/D2 for SDK/API drift |
| OAuth eligibility | `crates/infra-oauth2-client-credentials/src/lib.rs`: one-shot lead and existing two retry assignment sites | `into_token`, `reusable_service_token`, `refresh_ahead`; credential owner | Private schedule policy; no new state lifetime, exported API or grant behavior; deterministic calculations replaced at those sites | OAuth local behavior proof; reopen D2 for expiry/driver interaction |
| JWKS periodic delay | `crates/infra-bearerauthn/src/refresh.rs`: private delay and persistent deadline in current worker | `run_refresh_worker`, `KeyStore`; bearer verifier | Existing process owner; remove unused Interval/MissedTickBehavior imports; no new task | Bearer local behavior proof; reopen D2 for cancellation/coalescing interaction |
| Operator truth | Existing guides listed under D4 | Current source/guide authority, R2 | Edit canonical text and cross-links, preserve initializer profile markers; no parallel truth document | Static source/guide consistency and docs links; reopen Definition for changed behavioral claim |

| Material Rust file | Responsibility | Present reason / visibility / call path | Lifecycle, errors and dependencies | Forbidden responsibilities |
| --- | --- | --- | --- | --- |
| `crates/infra-messaging/src/messaging.rs` | NATS delay | Existing connection owner; private callback policy consumed by `connect` | SDK retains lifecycle/error owner; std durations and async-nats rustls re-export of the existing AWS-LC random interface | Generic retry framework, secret reader redesign, service composition |
| `crates/infra-oauth2-client-credentials/src/lib.rs` | OAuth eligibility | Existing token/cache policy owner; private schedule calculations consumed at admission/queue/completion | Existing driver and provider errors; current imports plus existing AWS-LC rand API | New token timer, public RNG/token surface, exchange refresh |
| `crates/infra-bearerauthn/src/refresh.rs` | JWKS periodic delay | Existing periodic worker; private delay policy feeds same select | Existing cancellation and `KeyStore` failure handling; Tokio time and existing AWS-LC API | New worker, key expiry policy, readiness or unknown-key jitter |

Non-mechanical source selection for all three is existing provider/library reuse:
AWS-LC 1.18.1 (through async-nats/rustls for messaging) plus native NATS callback / Tokio sleep / current OAuth state. The
strongest reusable rejected mechanism is backon 1.6.0 (D1). Parity means R1
ranges and existing owner invariants, not bit-identical deterministic times.
Upgrade only for a demonstrated missing API or defect. Implementation chooses
focused tests beside these owners, including existing OAuth `src/tests.rs`;
those proof files add no runtime responsibility or exported seam.

## D4: canonical documentation mapping

Use existing guides; do not add a new overview file. A compact cross-provider
rotation section in `docs/configuration-source-policy.md` supplies navigation,
publication/reload/authentication/expiry/revocation distinctions and the common
atomic publication, overlap, sanitized verification and emergency response
sequence from R2. Keep provider details in the following canonical owners.

| Existing owner | Required edit / consistency surface |
| --- | --- |
| `docs/configuration-source-policy.md` | Startup snapshot, fixed username/secret/key/kid/policy; named reader exceptions; cross-provider links; eventual Secret projection and no subPath update; external Vault/sidecar and workload-identity scope; no generic credential issuance. Existing OTLP TLS variables and constructed-client load points stay here. |
| `docs/architecture/persistence.md` | Correct five-second cadence versus cutover SLA and 30-minute retirement versus forced session revocation; password-only options and last-good reads; SQLx handshake root-file read/additive trust and require versus verify-full. Retain decision-owner wording. |
| `docs/background-jobs.md` | LISTEN uses a separate no-max-lifetime session and current options on reconnect; link to persistence for password-only rotation and session limits. |
| `docs/cache.md` | Preserve existing 1 s read+AUTH, retry unchanged rejected bytes, jittered reconnect and conditional 7/11 s bounds; clarify accepted AUTH versus file read and admitted TLS material. |
| `docs/durable-messaging.md` | R1 delay range, immediate attempts, 4 s cap; coherent per-challenge JWT+seed file read, external early renewal, open-session limit and reconnect recovery; NATS root-file reread applies at reconnect. |
| `docs/outbound-machine-authentication.md` and `docs/outbound-machine-authentication-decisions.md` | Replace deterministic lead/retry wording with R1 timing and preserved owner/budget policy; distinguish access-token refresh from fixed assertion signing key/kid rotation, provider overlap and already-issued token validity. |
| `docs/authentication.md` | First/subsequent `[13m30s,15m]` scheduled wait; unchanged 30 s unknown-key cooldown; last-good/no-max-age/known-kid behavior and expiry; fixed introspection/trust policy. |
| `docs/grpc.md` and `docs/outbound-http.md` | Fixed server acceptor/config and constructed tonic client; process-wide outbound HTTP TLS owner means rebuilding Client alone does not reload Linux trust roots; existing connections/resumption separate from full handshakes. |
| `docs/architecture/integration.md` and `docs/architecture/runtime-lifecycle.md` | Update only contradictory schedule or revocation summaries; preserve ownership/no-readiness/no-new-task claims and refer to canonical guides. |

R2 is source-qualified guidance, not measured cutover proof. All provider-specific
TLS wording must be tied to its resolved owner; never convert a root-file read
into a universal reload or hard-revocation promise. Preserve relevant template
markers so a removed profile does not retain links or dependency requirements.
No runtime changes in PostgreSQL, Redis, gRPC, outbound HTTP or configuration.

## Proof, delivery and reopen boundary

Implementation selects concrete cases and reuses current protection tests.
The discriminating proof is bounded arithmetic (including saturation and random
failure), one sampled token lead, unchanged retry floor/expiry cutoff, independent
period samples, unknown-key cooldown and cancellation/coalescing preservation.
Use existing deterministic controls rather than chance-sensitive assertions that
random samples must differ. No new production test interface, RNG trait, test
runtime or secret fixture is required by this design.

Several crates select ordinary matching build and tests;
use the current validation owner for mixed code/docs routing and dependency
advisory checks. Docs require static source consistency and relative-link proof.
CI owns retained heavy gates. No full local profile matrix, database exercise,
real broker/identity provider, load campaign or credential rotation is newly
required. An assembled final independent review remains required for the
changed scheduling/authorization interaction; per-task reviews are not added.

One separate PR may carry the authorized implementation, local proof, commit
and push. Merge, deployment, infrastructure and real credentials remain outside
scope. Planning can order the three disjoint code owners and the documentation
assembly without inventing mechanism. Reopen Specification for changed timing,
revocation or compatibility; D1 for resolution/library changes; D2 for scheduler
semantics; D3/D4 for an actual ownership contradiction. No user technical input
is pending.
