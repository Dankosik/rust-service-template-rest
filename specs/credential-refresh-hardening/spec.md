# Credential refresh hardening

Status: ready. Independent [Specification Review](definition-review.md): PASS.

Requester meaning: [Intent](intent.md). Supporting source and limitations:
[baseline](research/baseline.md). Base: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.

## Outcome and boundary

Spread the existing provider-facing refresh/reconnect schedules across
independent replicas, without weakening expiry, admission, lifecycle or retry
bounds. Give an operator an accurate per-material rotation and revocation
contract. One separate PR carries the necessary changes and documentation.

No API or schema change, new identity mode, configurable jitter surface,
background credential framework, secret-manager SDK, universal TLS hot reload,
automatic session termination, or deployment is part of this outcome.

## Material behavior

### R1: bounded schedule spread

Randomness must vary independently between process instances/owners and repeated
eligible schedules; a deterministic seed shared by replicas is insufficient.
The policy ranges below are Definition decisions, not measurements. They use
small bounded windows to reduce synchronized provider work while keeping the
existing maximum waits or minimum retry spacing. Technical Design owns the
randomness mechanism, arithmetic, and placement.

| Existing schedule | Required change and bound |
| --- | --- |
| NATS reconnect | Retain immediate attempts 0 and 1. For a later attempt with the existing capped exponential delay B, choose its delay within `[0.9 B, B]`, independently on each attempt. Saturation remains 4 s; large attempt counts cannot overflow or wrap into a short retry. Retain unlimited SDK reconnect recovery, startup admission and attempt/request budgets. |
| OAuth reusable service token | With reuse cutoff U and existing maximum lead A = `min(5 min, reusable lifetime/4)`, choose one lead in `[0.9 A, A]` when a reusable token is admitted. Its eligibility is U minus that lead. Cache hits do not resample or slide the time. This is still caller-triggered background refresh; idle clients create no new autonomous fetch. |
| OAuth subsequent background attempts | Where the existing owner uses its 30 s retry floor, choose spacing in `[30 s, 33 s]` for the next eligibility. Retain that floor for success and failure paths. Eligibility does not permit using a token at/after its reuse cutoff; a foreground miss retains its current immediate bounded acquisition behavior. |
| JWKS periodic refresh | First and subsequent scheduled periods use independent delays in `[13 min 30 s, 15 min]`. This range bounds scheduled waiting, not network completion. Retain one worker, no catch-up bursts, and cancellation priority. Unknown-key cooldown is unchanged at 30 s and is not jittered. |

The ranges permit rounding to the scheduler's duration precision. A nonzero
source delay must stay nonzero. A failure to obtain randomness must preserve
the existing conservative schedule and all existing budgets; it must not create
a panic, rapid loop, new readiness failure, or credential-bearing diagnostic.

OAuth preserves its one pending queue, shared foreground/background lock,
five-second background budget starting at enqueue (including lock wait),
provider semaphore, one-second completed failure suppression, exact-token
invalidation, and joined driver shutdown. No resource operation is replayed.
Missing expiry stays request-only, arithmetic overflow is invalid, and token
exchange retains its existing cache and no background refresh. JWKS failure
retains the last usable keys and unknown-key requests remain coalesced. Reuse
the current implementations and supported driver hooks; changing clients is
not needed to achieve these behaviors.

Nearest feasible falsifier: controlled scheduling shows the chosen times fall
outside these bounds, stay synchronized under independent randomness, slide on
cache hits, or violate the existing cooldown/expiry/owned-work boundary. The
executor chooses concrete cases and reuses adequate existing protection tests.

### R2: accurate rotation and revocation guidance

Update existing canonical guides and decision records where they own the
contract; add a short cross-provider overview only if it improves navigation.
Documentation must distinguish publication, application reread, new-session
authentication, token expiry, and revocation. It must describe the following:

* `Config`, environment and `--secrets-dir` are a startup snapshot. Only named
  readers follow files. Fixed usernames, OAuth key/kid, introspection secret,
  issuer/discovery/audience/algorithm policy and admitted TLS owners require
  reconstruction or process replacement according to their existing owner.
* PostgreSQL rereads a password every 5 s and changes future connect options.
  Missing/empty/non-UTF8 content retains last-good options. Five seconds is a
  polling cadence, not an unconditional cutover SLA: file delivery/read time
  and server acceptance matter. Thirty-minute pool lifetime is retirement at
  pool lifecycle points, not forced revocation of checked-out sessions. The
  jobs LISTEN session has no such lifetime and follows current options when
  reconnecting. Password-only rotation cannot change the database username.
* Redis already rereads and reauthenticates live sessions with a 1 s read+AUTH
  envelope, retries rejected unchanged bytes, and uses jittered recovery.
  Preserve the existing documented conditional recovery bounds and distinguish
  accepted AUTH from merely reading new bytes.
* NATS file authentication reads the entire JWT+seed tuple at each challenge;
  replacing it does not itself reconnect an open session. The external owner
  renews it early enough for reconnect. Inline credentials remain a startup
  snapshot. Expiry/refusal may cause repeated reconnect failures; a new valid
  file can recover. Keep the callback; the SDK load-once file builder does not
  supply that behavior.
* OAuth already refreshes access tokens within the owned lifetime and expiry
  rules; rotating client signing keys is a separate startup-material change.
  Provider overlap must cover transition and independently valid issued tokens.
* JWKS refresh is not immediate revocation. Failed refresh keeps last-good
  keys without a new maximum-age cutoff; known-kid bad signatures do not force
  a refresh. JWT expiry still applies. A provider publishing/removing keys,
  overlap, service restart and revocation policy are different actions.
* TLS custom material and trust stores have owner-specific load moments.
  Identify fixed server config/acceptor, Redis admitted custom material,
  constructed tonic/reqwest/OTLP clients, NATS reconnect CA reread and SQLx
  handshake root-file reading without implying universal hot reload. Outbound
  HTTP uses process-wide TLS configuration: rebuilding its Client alone does
  not reload Linux roots. `sslmode=require` does not verify peer identity as
  `verify-full` does; additional SQLx roots do not imply exclusive trust.
* Changing trust material does not revalidate existing TLS sessions. Removing
  trust needs the relevant session/client lifecycle and resumption handling;
  no global hard-revocation deadline is provided. This PR documents those
  limits without adding reload or forced-drain machinery.

The operational sequence is: publish replacement atomically with overlap when
the provider supports it; allow external projection plus application recovery;
verify with existing sanitized reload/failure signals and a fresh authenticated
operation where relevant; remove old material only under provider and session
policy. For emergency compromise, use the provider's revocation/session controls
and the documented restart boundary rather than waiting on a file poll or cache.
This is guidance, not authority to perform a rotation during this task.

Kubernetes Secret projections are eventual and `subPath` does not update. A
Vault/sidecar file publisher remains external; no assumed username rotation,
lease renewal, or cloud credential issuance is supplied by the template.
No diagnostic may disclose tokens, passwords, private keys, raw `.creds`, or
arbitrary provider errors. Existing bounded signals are sufficient here; no
new metric, label dimension, readiness policy or alerting stack is required.

Nearest feasible falsifier: following a documented sequence would claim a
material or session changed when its actual owner still uses the old snapshot,
or a stated bound omits external delivery, read, admission or session lifetime.
Static source/guide consistency establishes this change; it is not live proof.

## Recommendation disposition

| Research recommendation | Disposition / rationale | Reopen condition |
| --- | --- | --- |
| Distribute NATS reconnect, OAuth refresh and JWKS periodic work | Implement R1; existing deterministic provider-facing schedules can align replicas. | Measured provider constraint or changed SDK scheduling contract. |
| Jitter every file poll, including PostgreSQL and Redis | Preserve current 5 s polls; local reads do not themselves call a shared credential issuer, Redis already jitters reconnect, and altered polling would change its recovery contract without a present need. | A real shared filesystem load problem or provider call added to polling. |
| Bound OAuth refresh concurrency, lock time, background ownership, failure retry and expiry | Preserve; base already provides these controls. | A relevant code/dependency change or failing behavioral evidence. |
| Repair Redis rejected reAUTH recovery and impose read+AUTH deadline | Preserve; base supervisor already does both. Do not import the unrelated streaming-provider branch. | Native SDK alternative demonstrably retains the complete accepted contract and solves a current need. |
| Add stale-password/JWKS cutoff or hard revocation semantics | Defer; would introduce availability/security policy not required by a current consumer. R2 documents last-good behavior and its limits. | Concrete maximum exposure requirement with outage tradeoff and owner. |
| Add bounded generic file reader / size cap | Defer; local operator-owned files, current startup failure policy and existing sanitized failures do not establish a new untrusted-input requirement. No unconditional PG cutover bound is claimed. | Actual file-source stall/resource incident or accepted maximum size/read deadline. |
| Add reload/expiry metrics, new readiness gates or dashboards | Preserve existing sanitized events and metrics; implement their accurate operational interpretation in R2. | A named operational question cannot be answered by current signals. |
| Replace NATS callback with native credentials-file builder | Preserve callback; SDK builder is load-once. | SDK introduces equivalent per-challenge coherent tuple reading. |
| Proactive NATS expiry parsing and reconnect | Defer; broker owns acceptance and current callback already recovers on reconnect. | A consumer requires seamless short-lived credentials with a proven broker behavior gap. |
| TLS certificate/CA hot reload and session eviction | Defer runtime work; implement explicit unsupported-reload and session/resumption guidance. | A consumer requires rotation without restart or a bounded trust-removal SLA. |
| Unified rotation matrix, overlap, expiry and revocation runbooks | Implement R2 in existing owners. | Owner/library behavior changes. |
| Secret manager SDKs / credential framework | Defer; external file publisher plus current readers meet this outcome. Preserve AWS workload identity already delegated to its SDK. | Actual provider cannot fit the existing carrier and identity contract. |
| SPIFFE/SPIRE adoption | Defer as a separate workload-identity/trust-domain decision. | Accepted consumer trust domain and deployment owner. |

## Composition, compatibility and proof boundary

Consider replicas starting together while their identity provider fails and a
file publisher rotates database/broker material. Jitter varies scheduled
provider work; it does not bypass provider admission, token expiry, request
budgets or shutdown. Each current adapter keeps its own file/authentication
failure policy. Old authenticated sessions may remain usable. Recovery guidance
requires verifying the relevant new authentication, so successful file delivery
cannot be mistaken for complete revocation. No rule promises zero downtime.

No dependency upgrade or configuration migration is required by this behavior.
Technical Design must decide the smallest existing randomness mechanism and
source placement, then undergo its required review. Planning follows normally.
Implementation owns tests and the repository's matching local validation,
docs checks and assembled delivery review. Existing CI gates apply to the
authorized PR; no new cloud environment, performance campaign, provider
sandbox, exhaustive profile matrix, or live credential exercise is mandatory.

Reopen Intake for changed desired behavior or authority; Specification for a
changed range, failure/revocation policy or compatibility promise; supporting
Research for contradictory base/library facts. Definition stops after its
reviewed transition; code, Technical Design and publication belong downstream.
