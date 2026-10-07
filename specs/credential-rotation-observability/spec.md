# Credential rotation observability

Status: ready. Independent [Specification Review](definition-review.md): PASS,
including the scoped R3 correction; unchanged R1/R2 retain their original review.
Authority: [Intent](intent.md). Evidence:
[baseline](research/baseline.md). Base:
`699887b18594088a59bcc23a049d290d089f6da1`.

## Outcome and boundary

Make existing file-credential refresh and usable JWKS acquisition observable
without changing authentication policy. Close the NATS and Valkey real-server
authentication proof gaps in one separate PR. Operators must not mistake file
progress for server acceptance, or acquisition recency for issuer key freshness.

No TLS reload, stale-key cutoff, revocation deadline, new identity mode,
credential issuer, new readiness/default policy, production rotation or broad
import of PR #247 is included. Existing public REST/configuration contracts,
refresh schedules, provider request counts, retry/deadline limits and lifecycle
ownership remain unchanged. No new metric-driven control flow is permitted.

## R1: bounded observations at the actual file-refresh boundary

Expose cumulative refresh outcomes through the existing metrics surface for
PostgreSQL password-file polling, Valkey password-file maintenance and NATS
credential-file challenge preparation. Counters describe completed work at the
owner's boundary, including completed failures; they must not count an attempt
that never completed as success. Existing startup admission errors retain their
behavior; startup admission does not require a second, duplicate counter family.

| Owner | Required distinguishable outcomes and success meaning |
| --- | --- |
| PostgreSQL periodic file owner | Successful unchanged read; installation of material into future connection options; failed read/validation. Installation establishes no database authentication. The first periodic assignment may be installation but is not evidence of rotation from startup material. |
| Valkey maintenance owner | Successful unchanged read; changed material accepted by AUTH on the maintained connection; failed read/validation; failed refresh exchange. Exchange failure retains a bounded classification distinguishing authentication rejection from timeout/transport failure. Neither a read nor sending AUTH is accepted authentication. |
| NATS file challenge owner | Prepared current JWT/signature tuple; failure to read/parse/sign it. Preparation establishes no broker authentication. Each completed challenge is an observation even when bytes repeat; it is not a rotation counter. |

Repeated successful reads/preparations may increase their corresponding outcome
counter, but cannot increase a metric documented as rotations or authentications.
Existing `postgres_password_reloaded`, `cache_password_reloaded` and
`messaging_credentials_reloaded` events retain their exact current boundaries;
guide text must say what each proves. No new NATS or PostgreSQL authentication
success metric is required: the adapter cannot infer it from these file owners.

Keep failures secret-free and finite. Labels may identify only statically
bounded provider/stage/outcome/reason categories chosen in Technical Design;
no paths, URLs, usernames, JWT subjects, key IDs, key/token/password values or
fingerprints, arbitrary errors or per-instance IDs. Existing process/target
scrape labels identify the deployment instance. Use existing metrics/tracing
owners, without new network calls, periodic tasks, configuration keys, secrets
retention or per-credential histories. Disabled profiles/file readers produce
no synthetic successful activity. Counters reset with the process and are not
an audit ledger.

Nearest falsifier: an unchanged read or rejected AUTH is presented as rotation
or acceptance; a completed failure vanishes from the corresponding outcome
signal; or varied secret/input data increases label cardinality or enters output.
Implementation chooses focused cases and reuses current failure coverage.

## R2: when usable JWKS material was last obtained

Expose the time of the latest successful usable JWKS acquisition through the
existing metrics surface. The semantic value is a Unix timestamp in seconds,
named and described as last successful acquisition, not key age, validity or
revocation. A successfully admitted startup key set establishes the initial
value. A completed refresh advances it only after the response has parsed to
a usable key set and that set becomes the owner's current set. A successful
refresh returning the same usable keys also advances it.

Failed fetch, invalid document, no usable keys, cooldown/coalescing without a
fetch, request cancellation or a cancelled worker fetch cannot advance it.
The existing success/failure counter retains its semantics and label set.
On failed refresh, keys and the previous successful timestamp remain usable
under the existing policy. The timestamp is process-local evidence, initialized
again on successful startup; a profile that has never admitted JWKS material
has no successful timestamp. It is not persisted or synthesized from process
start. Wall-clock corrections can affect timestamp-derived elapsed time and
must be documented; they do not change refresh scheduling or token validity.

The operator can use acquisition recency together with existing failure counts
to recognize ongoing fetch failure. No threshold, alert, readiness failure,
maximum staleness or forced key eviction is introduced. Publisher freshness,
key change, old-key removal, token acceptance and immediate revocation remain
distinct facts. The signal has no issuer/key labels or new diagnostic endpoint.

Nearest falsifier: success followed by failed refresh makes the timestamp newer,
or successful reacquisition of the same usable set leaves it stale. A known-key
JWT continuing to validate after failed refresh is preserved policy, not proof
that the provider was contacted successfully.

## R3: real authentication during NATS and Valkey file rotation

Extend existing integration coverage to cross the actual server authentication
boundary with disposable synthetic material. For NATS, the broker must validate
the adapter's JWT-and-seed challenge credentials. Demonstrate that replacing
the file permits the existing client to reconnect and perform an authenticated
operation using replacement material. Real-broker evidence must include refusal
of old credentials after the fixture's provider policy stops accepting them,
refusal of expired user credentials, and recovery after publication of a
currently valid replacement file. A broker-rejected credential must fail new
authentication even if the client successfully prepared it. Evidence must
exclude stale captured credentials or disabled server authentication as an
explanation for success. This concerns new authentication; it does not impose
immediate termination of already authenticated sessions. An atomic rename is
already covered and is not a separate new capability.

For Valkey, use an authenticated user and the adapter's password-file path to
demonstrate acceptance of replacement credentials during maintained-connection
refresh and on a subsequent connection. A rejected replacement must not produce
`cache_password_reloaded` or authentication-success telemetry; a valid pending
replacement must permit recovery under the existing behavior. A successful
cache command on the original authenticated session alone cannot prove reAUTH.
After the fixture's server policy removes an old password, new authentication
with that password must fail. Expiry coverage applies to expiring NATS user
credentials; this task adds no Valkey password-expiry policy or mechanism.
Reuse existing protocol mocks for precise deadline, no-traffic retry and
sanitization semantics; do not reproduce their exhaustive scenarios on a server.

Malformed replacement content must fail at the existing file/parse/sign boundary
and must never be reported as accepted authentication. For NATS this includes an
unusable credential tuple; for Valkey it means input rejected by the current
password-file validation, without imposing a new password syntax. Publishing a
corrected valid file must restore the existing recovery path. Preserve the
current last-good-session/options policy while such a file is unusable.

These are required regression coverage outcomes, not a prescribed test matrix.
The executor chooses the smallest proving scenarios, assertions, fixture
construction and commands, combining outcomes and reusing adequate existing
mocks where they establish the claimed boundary. No separate test per failure
kind is required. Mock coverage may establish malformed-input handling and
precise recovery timing, but cannot replace the NATS real-broker expired/old
credential refusal and authenticated replacement-recovery evidence above.
Use the current integration harnesses and pinned services; keep test identities
and mutations isolated from other tests and tear down test-owned state. No real
credentials or live service configuration are in scope. Technical Design must
resolve any mechanism/isolation question before implementation depends on it.
This requirement does not create a new environment, runner or permanent stack.

Source-only assertions, wire recording with authentication off, a fake AUTH
reply and skipped/zero-selected tests cannot establish a real-server proof.
Execution of optional local container scenarios remains under the repository's
validation policy; CI's applicable integration gates can own the real-server
run. Report an unavailable/unrun scope accurately rather than claiming it
passed or provisioning extra infrastructure. Completion of the requested proof
claim requires a matching actual run somewhere in the accepted local/CI path.

PostgreSQL password rotation and LISTEN reconnect coverage are deliberately
reused. This task neither certifies every provider combination nor proves
production revocation, session eviction or zero downtime.

## Composition and compatibility

When files are replaced while servers reject new material and the identity
provider is unavailable, file-read/preparation counters can progress while
authentication fails and the JWKS timestamp remains old. Existing usable
sessions and keys may continue working. The operator sees these separate
facts; no signal silently promotes them to full rotation success. Once usable
JWKS acquisition or authenticated Valkey refresh succeeds, only that boundary's
success evidence advances. NATS preparation remains preparation even after a
separate authenticated reconnect demonstration.

Documentation updates belong in existing provider/observability guides. Explain
metric meanings, absence/reset/clock behavior and the distinction above; do
not add dashboards or operational policy. Added telemetry is compatible;
retain existing metric and event meaning so consumers do not silently change.
No dependency upgrade, new generic abstraction or configuration migration is
required by this contract.

## Downstream decisions and reopening

Technical Design owns the minimal metric schemas, registration/update placement,
JWKS time-capture boundary and authenticated fixture feasibility/isolation using
existing owners. It must not invent a new policy or claim that a file stage can
observe broker acceptance. Planning and Implementation follow their current
owners; implementation selects tests and matching validation, with the existing
CI gates and final review trigger preserved.

Reopen Intake for changed outcome/authority; Specification for changed signal
meaning or authentication/failure policy; supporting Research for contradictory
source or dependency facts. Recheck affected surfaces when main changes or
PR #247 lands; do not import it automatically. Definition ends at its reviewed
transition, with no build/test/runtime or delivery claim.
