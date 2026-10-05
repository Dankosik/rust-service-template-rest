# Outbound transport resilience

Status: ready. Independent [Specification review](definition-review.md): PASS,
including the bounded gRPC cooperative-timeout clarification.

Authority: [Intent](intent.md). Evidence: [reviewed research baseline](research/baseline.md).
Base: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`.

## Outcome and boundaries

An admitted destination must reach the intended host representation and trust
boundary. A connection attempt that stalls in DNS, TCP, TLS or protocol opening
must not strand a reusable client indefinitely after its caller has timed out.
Recovery must preserve the caller's original operation deadline and the actual
effect's finality. Correct concrete gaps with the smallest provider-owned change.

This is one outbound-client correction PR. There is no inbound API change,
application retry policy, live endpoint watcher, universal resolver, provider
migration, new service or deployment. Technical Design selects mechanisms and
placement; it may not quietly omit an accepted behavior below.

## Accepted behavior

### PostgreSQL literal IPv6

Every currently admitted IPv6-literal PostgreSQL URL must supply a bare literal
to the socket address resolver and the matching IP identity to TLS verification.
Brackets remain URL syntax, not hostname bytes. Only parsed IPv6 literals are
normalized: DNS names, IPv4, port, database, password source, sslmode and CA
admission retain their meaning. Malformed/unsupported URLs remain rejected by
the existing sanitized admission path. No new DSN forms are accepted.

The nearest falsifier observes the admitted connection options and resulting
dial/TLS target for a literal URL; an acceptance-only parser test which expects
brackets is insufficient. Real-database execution follows the existing database
validation owner when an observed database claim is made.

### gRPC reusable channel recovery

Keep lazy construction without network I/O. Each newly attempted connection has
one native cooperative 5 s wait limit spanning DNS, TCP and TLS, including the
connection future retained by the shared channel. While the channel is driven
by an active call, a still-pending dial observes expiry and releases that failed
attempt. The limit uses the original attempt's deadline; idleness or another
call never restarts that deadline.

When no live call remains, the lazy channel may stop polling and retain the
attempt beyond five seconds. On a later poll, a still-pending dial observes its
expired deadline; that later call may receive the retained transport failure,
and a subsequent call through the same client must be able to redial. An already
ready connection result may win the native timeout poll even after elapsed wall
time. Eager cleanup while idle and rejection of an already-ready late result
are not promised. Neither can leave future actively driven calls indefinitely
waiting on the original stalled dial.

A caller with a shorter FullRpc, OpeningOnly or supplied deadline still expires
earlier; connection recovery never extends or renews it. No new RPC retry,
replay or hedging is introduced. Existing normal TLS validation, keepalive and
TCP settings remain. This preserves the lazy channel's native ownership instead
of adding a separately driven connection supervisor solely for idle cleanup.

Existing deadline/status/finality behavior in [gRPC](../../docs/grpc.md) is
unchanged: before dispatch expiry prevents submission; after dispatch the
remote effect may be unknown. A terminal peer status already observed remains
final. Prove release under active polling and later recovery through the same
client after an idle interval, allowing the retained failure described above,
not merely that one RPC times out. OpeningOnly still permits a stream after its opening
deadline has been satisfied; connection timeout is not a stream lifetime.

### Authentication-provider dial budget

Inbound-authentication provider requests retain the 3 s total attempt deadline,
including body completion, and gain a strictly smaller finite connection budget
covering DNS, TCP and TLS. Technical Design chooses its fixed value from the
existing budget and supported native behavior; it is not a new operator knob.
A failed/stalled connection releases the provider attempt through its existing
unavailable classification, allowing subsequent attempts without a stuck client.
The connection budget must permit native candidate fallback within that budget.

No ambient proxy, redirect, library/application retry, TLS bypass, extra allowed
origin or larger response body is introduced. Discovery, JWKS and introspection
use this provider client. Outbound OAuth token acquisition remains with
`infra-outbound-http`. Existing bulkhead/cache/failure and request-expiry owners
remain unchanged. The nearest falsifier is a stalled dial followed by an
available address/attempt, under the same 3 s total; adequate existing body and
trust coverage is reused.

### NATS bounded recovery

Both admission and each background reconnect attempt must have a finite 5 s
ceiling covering name resolution, candidate TCP connections, TLS, initial INFO
and authentication. Timing out a stalled reconnect must release its ownership
and permit another attempt; a restored broker must be usable by the existing
messaging owner without a process restart. No finite retry count is imposed on
an otherwise recoverable dependency. Retry pacing remains bounded and avoids a
tight loop. Technical Design owns the supported-library strategy and lifecycle.

Readiness remains false while disconnected, using the existing cached-refresher
owner. Cancellation and shutdown stop recovery within the existing shared
deadline; forced close remains degraded. Consumers retain durable position and
settlement semantics, and file credentials are reread on each connection.
Publish cancellation/timeout after possible dispatch remains ambiguous; no
transparent replay, duplicate success, fabricated ACK, changed DLQ ordering or
cursor deletion is permitted. Nearest falsifier: stall a reconnect before TCP,
observe bounded attempt termination, then restore availability and observe
same-owner recovery; retain existing settlement proofs.

### NATS discovery trust and compatibility

Discovery may extend the configured seed set only when the supplying INFO is
inside an accepted trust boundary. Ordinary TLS with plaintext-first INFO does
not authenticate that INFO, even if a later dial verifies the advertised
hostname's certificate. In that mode, use configured seeds and ignore discovered
servers. Keep ordinary TLS startup compatible with brokers that do not support
TLS-first; do not opportunistically downgrade a selected TLS-first mode.

Provide one explicit provider capability choice for TLS-first handshake. When
selected, the complete connection uses TLS before INFO and authenticated cluster
discovery is retained. Selecting it against an incompatible broker fails through
bounded admission/recovery. Plaintext trusted-network mode retains discovery
because the operator already declares that network the trust boundary. Existing
local/development plaintext escape hatches likewise retain discovery within
their admitted scope. TLS-first with plaintext destinations is invalid; a mixed
TLS/plaintext seed set uses the conservative no-discovery policy unless a later
accepted design proves a per-connection boundary with equivalent semantics.

Document the compatibility change: an existing ordinary-TLS deployment that
relied on discovered hosts must list failover seeds or enable TLS-first on both
broker and client. Credentials remain required where currently required, and
normal hostname/certificate checks apply to every TLS destination. No arbitrary
new hostname allowlist or private-address prohibition is introduced. Negative
proof must distinguish an injected pre-TLS advertised address from permitted
authenticated/trusted-network discovery. This addresses destination authority;
it does not claim that NKey seed material was leaked.

## Conditional address-fallback decisions

The audit found plausible candidate starvation in PostgreSQL, NATS and S3;
bounded total failure alone does not establish successful address fallback.
Technical Design must close a separate disposition for each using the exact
library source and, where it changes the decision, one bounded discriminating
fault probe within existing fixtures. A probe is mechanism research, not a new
mandatory environment or exhaustive acceptance matrix.

When a supported native setting/extension or a small provider-owned repair can
give a healthy later candidate a dial opportunity within the existing parent
deadline, adopt it. Preserve TLS hostname/IP identity and existing cancellation,
operation finality and retry limits. If source/evidence shows existing behavior
already adequate, record that and reuse coverage. If correction would require a
new resolver, provider migration or material custom transport, record the exact
limitation and alternative cost and reopen this bounded decision in Definition
before excluding it. Do not label a confirmed in-scope defect a non-goal or
silently retain it merely because total timeout passes.

Redis already races all TCP/TLS candidates and needs no fallback rewrite.
Outbound HTTP's existing two-address blackhole coverage is reusable evidence,
not a reason to replace the other provider clients with that transport.

## Deliberately unchanged and documentation corrections

| Surface | Disposition and rationale |
| --- | --- |
| DNS/live sockets | Keep system resolution and per-library pooling. Explain that a new dial may observe DNS changes, while busy live sockets are not migrated. Idle eviction is not a maximum age. No endpoint watcher or blanket resolver migration is justified. |
| PostgreSQL pool and LISTEN | Retain pool idle/lifetime settings, server statement timeout, vendored whole-return bound, real-PgBouncer/commit-proxy regressions and dedicated LISTEN polling recovery. Clarify their separate scopes; no duplicate supervisor or warm connection. |
| Redis/Valkey | Preserve bounded command/PING behavior, connection generations and existing write-once recovery coverage. Native ConnectionManager alone does not replace the owned recovery contract. |
| gRPC keepalive | Retain active-stream 60 s PING / 20 s timeout; state the provider's compatible enforcement requirement. Do not add retries, discovery or claim per-RPC balancing. |
| S3 streaming | Preserve operation and stalled-download protections. Document that GET header completion ends the SDK operation timeout and that callers own a whole-stream deadline; no universal stream cap is added. |
| Certificate/credential rotation | Document each affected client's actual reload/snapshot boundary and required restart. Preserve existing supported password/credential-file reload; no global hot-reload capability is invented. |
| Security/error/telemetry | Keep sanitized typed failures and bounded labels. New bounded-attempt failures must be observable through existing failure/lifecycle vocabulary, without endpoint secrets or raw provider text. |

## Composition and acceptance

Representative case: a caller uses a long-lived client while a configured host
becomes unreachable, then the dependency recovers. The caller receives its
bounded existing failure, never false success. A later call can dial again;
configured trust and any effect that might already have occurred keep their
meaning. Connection recovery neither spends a fresh operation deadline for the
expired caller nor authorizes replay. For NATS, an untrusted advertisement cannot
change the set of permitted recovery destinations.

Implementation adds or adapts the smallest regression cases which distinguish
these defects, reuses adequate existing coverage and follows the repository's
ordinary local validation budget plus applicable CI gates. These falsifiers do
not mandate new containers, external infrastructure, provider certificates or a
new standalone runner. Exact cases and commands belong to the executor; final
validation is assembled once under Implementation. Publication needs the
separate PR and its actual CI state; local acceptance is not production proof.

No user-owned decision is open. Technical Design owns exact budget selection,
library extensions, NATS recovery lifetime, conditional fallback dispositions and
file placement. Reopen Definition if a supported mechanism cannot satisfy an
accepted behavior without a material compatibility or scope change. Reopen
Research only for the fact that blocks that decision.
