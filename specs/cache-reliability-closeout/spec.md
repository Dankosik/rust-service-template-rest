# Specification: cache reliability closeout

Status: ready. Authority: [Intent](intent.md). Baseline and failure evidence:
[Research](research.md). This specification defines behavior; Technical Design
owns mechanisms, placement and resource accounting.

## Accepted behavior

### R1. Stalled established connections recover without replay

After a connection has completed setup, a peer that accepts commands but
ceases answering must not leave future calls permanently attached to that
connection. Every get/set/delete still ends within `cache.command_timeout`,
including connection wait, and yields the existing `Unavailable` on timeout.
Recovery never retries the timed-out command: SET or DEL may already have
taken effect, so timeout proves neither success nor absence of the effect.

Following an observed command timeout, subsequent demand must make a fresh
connection eligible within the existing 2 s recovery spacing. Recovery may
also advance without demand. It must not accumulate abandoned managers,
warm-up tasks, or unanswered response slots across repeated timeout cycles;
retained work must have a finite lifetime and a bounded number of live
connection generations under sustained demand. Technical Design records the
derived bound using existing command, connect, recovery and lifecycle budgets.
No RSS or request-rate target is asserted.

Concurrent failures from an older connection must not invalidate a recovered
healthy successor. Caller cancellation and externally bounded probe calls
must not leave permanently retained work either; cancellation alone is not a
new requirement to replace a healthy connection. Startup and sparse-traffic
recovery continue to make progress as the current profile promises.

Falsifier: successfully establish a connection, then withhold its replies
while permitting new connections; callers time out within budget, a later
call succeeds through recovery, and retired work does not pile up when the
scenario repeats. A cancelled waiter or late old failure cannot spoil the
successful successor. Mechanistic counters/socket/task observations suffice;
a memory benchmark is not required.

### R2. Background work has the cache's lifetime

Warm-up, recovery and credential work must have a defined cache/connection
owner and stop when that owner retires. Dropping the final application cache
owner (including namespace and probe handles) must initiate cancellation of
all cache-owned background work and release its retained managers; no task
may wait out a 20 s warm-up merely because its join handle was discarded.
Retiring a connection must similarly end its obsolete owned work. Ordinary
outstanding operations remain subject to their existing caller budgets.

Normal shutdown, failed startup and interrupted startup must release the
cache within the service's existing teardown boundaries. If cleanup requires
an awaited close, it consumes the existing dependency-close deadline and
reports overrun through the established degraded-shutdown result; it adds no
grace period. No useful reconnect/re-auth work starts after final cancellation.
Scheduler completion is not required to be synchronous with a Rust destructor.

Falsifier: hold setup or a reply pending, release all application handles or
exercise teardown, and observe owned work/connection release without waiting
for the warm-up timeout. Retaining one legitimate namespace/probe still keeps
the shared cache available until that owner is released.

### R3. Password-file rotation survives rejection

Preserve password-file admission, secret precedence, newline handling,
username semantics and the current 5 s refresh cadence. A changed valid file
must continue to become effective while the cache is alive, including after
an earlier changed credential was rejected on an established connection.
Recovery must not depend on that connection failing coincidentally, traffic
arriving, or a service restart. Re-authentication or reconnection may implement
this behavior; uninterrupted use of the same socket is not required.

A rejected credential is not reported as accepted. Once the file supplies a
credential the reachable server accepts, bounded refresh/recovery resumes
authenticated operation. A credential written before the server accepts it
must also recover after server acceptance without requiring a different file
value. Technical Design expresses the recovery bound from the existing
refresh and reconnect budgets. An unreadable/empty later file preserves the
last usable authenticated connection when possible and emits the existing
sanitized diagnostic; new connection attempts still cannot authenticate from
an unavailable file. These failures must neither spin nor end future refresh.

Falsifier: start authenticated, reject one rotation while the socket remains
open, then accept the current or a later file credential; observe recovery
without user calls or restart. Include unreadable-file recovery only where
existing coverage no longer proves the retained behavior.

### R4. Dependency diagnostics obey the cache's redaction contract

Cache operations, credential refresh, connection setup and recovery must not
emit DSNs, passwords, keys, values or raw server error text through Display,
Debug, tracing, or the service's bridged dependency logs. Keep useful bounded
error classification and the existing metric outcome vocabulary. A failed
authentication remains observable with a sanitized classification; successful
file reading alone must not imply successful authentication.

Falsifier: an AUTH/server error containing a unique arbitrary marker crosses
the real log bridge; the marker and credential do not reach rendered logs or
public error text, while a bounded failure signal remains available. This is
a cache-path requirement, not authorization for unrelated telemetry redesign.

### R5. Adoption and dependency guidance is internally consistent

Moka is appropriate across multiple replicas when independent process-local
copies and their consistency semantics meet the feature's needs. Shared bytes
are a reason to use this optional profile; replica count alone is not.

Preserve the global rule that a feature does not depend on a provider crate.
Document feature-owned behavior and composition/adapter-owned use of
`infra-cache` consistently in the guide, decisions, architecture, and crate
examples. Do not add a generic cache interface or speculative feature crate
to illustrate an adopter that does not exist. Technical Design owns the
smallest concrete placement explanation. Correct lifecycle and password
guidance to match the implemented guarantees and limits.

Falsifier: an adopter following the examples is instructed to add a forbidden
feature-to-provider edge, assumes one replica is necessary for Moka, or relies
on outdated timeout, rotation, sanitization or shutdown behavior.

## Deliberately unchanged and excluded

- Resolved redis-rs 1.7.1, Valkey, standalone topology, optional `CACHE=redis`
  selection and absent-DSN inertness; no new library selection or upgrade.
- Bytes-only get/set/delete, namespace prefixes, TTL validation, feature-owned
  serialization/invalidation/fallback and source-of-truth authority. No
  get-or-load, locks, rate limits, circuit breaker or automatic command retry.
- Current command-timeout configuration and HTTP budget rule, TLS/auth
  admission and certificate policy, 1 s degrading startup check, optional
  readiness and no cache liveness dependency. No new operator budget keys.
- Existing I/O, idle-disconnect and non-I/O setup-error recovery remain valid;
  removing redundant machinery is allowed only if the behavior is preserved.
- No merge/deployment, production investigation or capacity benchmark.

## Composition and proof boundary

A service starting during an outage still admits HTTP after its bounded cache
check. Once reachable, the cache recovers. If that connection later stalls,
calls degrade within their budgets, old work retires, and fresh calls can
succeed. A rejected password rotation does not strand credential tracking.
Concurrent old failures cannot undo recovery, and teardown cancels remaining
owned work while sanitized diagnostics remain usable throughout.

Implementation chooses the smallest tests for these behaviors and reuses
existing protocol, Valkey, redaction and service-lifecycle coverage. Local
validation follows the repository changed-surface budget; selected CI-owned
Valkey/profile gates remain CI-owned. No phase introduces another runner or a
full-repository local validation requirement.

Assumption: there is no existing feature adopter to migrate; this is supported
by the architecture's current feature inventory. Reopen Specification if a
real adopter changes the compatibility boundary. Reopen Research for changed
driver semantics, Intake for changed requested scope, or Technical Design if
the current budgets cannot provide the specified bounded recovery and cleanup.
