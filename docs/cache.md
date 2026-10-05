# Cache

<!-- template:begin cache:docs-cache -->
`CACHE=redis` retains an optional bytes-only Redis-protocol cache. It is inert
by default (`CACHE=none`). [Decisions](cache-decisions.md) own the library and
lifecycle choices.

## What the profile retains

The pack is one standalone client. Composition or a service adapter calls
`get`, `set`, and `delete` on a `CacheNamespace`, translating feature-defined
requests and results. Features own behavior and do not depend on `infra-cache`.
Rate limits and locks are not part of this profile; an accepted capability
that needs them can add a provider operation beside these commands.

It does not add a generic `Cache<K, V>`, get-or-load, a serializer, a global
TTL, or a lock API. The feature owns keys, serialization, TTL policy, and
invalidation. `set` stores the given bytes with `SET` and `PX`. A TTL below
1 ms panics. That is a programmer error.

## When process-local moka is enough

`moka` 0.12.16 is already the process-local cache. Multiple replicas may each
keep independent copies when their expiration and invalidation semantics meet
the feature's needs. Replica count alone does not require a shared cache.
Moka does not share state across replicas and cannot back a distributed rate
limit or lock. This profile is for bytes that replicas must share.

## Select and configure

`CACHE=redis` retains the pack. `CACHE=none` is the default and removes it.
Selection alone opens no connection. The `[cache]` section is active only when
`dsn` is set.

`cache.dsn` is a `SecretString`. Set it only with `APP__CACHE__DSN`. A missing,
empty, or whitespace-only value is absent, and the section stays inert. A
nonempty file value is refused because `dsn` is secret-like. `Debug` on the
client prints host, port, and whether TLS is on. It never prints the DSN or
password.

Admitted schemes are `redis`, `rediss`, `valkey`, and `valkeys`. The address
must be standalone TCP; a unix socket is refused, and Sentinel or Cluster URLs
are not admitted because their client features are not enabled. A password is
required, in the DSN or through `password_file`, unless
`allow_unauthenticated` is set. TLS uses native roots, or the
PEM file at `root_ca_path` when that path is set. The custom CA bytes are
admitted when the cache client is constructed and retained for reconnects;
replacing that file requires reconstructing the owner or restarting the
process. A CA path on a plaintext scheme is refused. The `#insecure` fragment is refused.

A server that requires a client certificate (mutual TLS) gets one through
`cache.client_cert_path` and `cache.client_key_path`. A self-hosted Valkey or
Redis with TLS requires it unless `tls-auth-clients no` is set; managed
services usually do not. The certificate file is a PEM chain, leaf first; the
key file is the leaf's PEM private key (PKCS #8; a PKCS #1 or SEC1 key also
loads). The two are set together: one without the other, either on a plaintext scheme, a
file that cannot be read, or a key that does not belong to the certificate
fails startup. Both files are read when the cache owner is constructed;
the service does this at startup. Renewing them requires a new owner or process,
not merely a connection retry. Publish the certificate/key as one coherent
generation before construction; two separate atomic file replacements do not
make a pair atomic. Both keys are paths, so a file or the environment may set
them. Existing TLS sessions are not revalidated by trust changes; termination
and resumption handling remain separate from loading new material.

The connection always speaks RESP3: the client opens with `HELLO 3` and
authenticates inside it, whatever `protocol=` the DSN carries. A server or
proxy without `HELLO` (Redis before 6.0) is not supported.

`cache.password_file` names a file that holds the password alone (one
trailing line break is ignored), for a platform that rotates it: a mounted
Kubernetes secret, a secrets manager's agent, or a sidecar that writes
short-lived tokens such as cloud IAM tokens. The DSN then carries no
password; a password in both places, or a file that is missing or empty at
startup, fails startup. A blank `password_file` value is unset. The user is the DSN's, or `default` when it names
none; password-file rotation cannot change that username. Every connection
attempt rereads the file within its 1 s setup budget;
if the file is unavailable, it cannot use a remembered password to connect.
An open connection checks the file every 5 s. Reading and, when needed,
direct `AUTH` share a 1 s budget. Only successful authentication records the
password as accepted and logs `cache_password_reloaded`. Rejected bytes stay
pending and are retried on later ticks even when the file is unchanged. A
plain AUTH rejection may preserve the previously authenticated connection;
a timeout, I/O failure, or unusable protocol retires it for recovery.

Replace the file atomically (write a new file, then rename it, as a Kubernetes
mount does). A later unreadable or empty file logs
`cache_password_file_unreadable` once per outage and preserves a usable
connection and its accepted password. Refresh continues without traffic.
Once the file and server are usable, refresh on a retained connection takes
at most 7 s; recovery requiring reconnection has a conservative 11 s bound,
assuming a reachable server accepts that credential. Rewrite expiring tokens
with enough margin for that recovery plus external publication/projection.
These are conditional local recovery bounds, not an end-to-end delivery or
revocation deadline. A file read alone is not accepted AUTH; verify the existing
sanitized reload/failure signals and fresh authenticated work before removing
the old credential under provider/session policy. The
[common rotation sequence](configuration-source-policy.md#rotation-and-revocation)
also covers emergency controls. The key is a path, so a file or
`APP__CACHE__PASSWORD_FILE` may set it.

`allow_plaintext` and `allow_unauthenticated` are accepted only when `app.env`
is `local` or `development`. `command_timeout` uses a human duration, in a file
or in `APP__CACHE__COMMAND_TIMEOUT`. Its default is `100ms`. The inclusive
range is `1ms` to `1s`. DSN form checks stay in `infra-cache`. Configuration
errors fail startup with a sanitized message.

```toml
[cache]
command_timeout = "100ms"
# dsn is environment-only: APP__CACHE__DSN
# password_file = "/run/secrets/cache-password"
# client_cert_path = "/run/tls/cache-client.crt"
# client_key_path = "/run/tls/cache-client.key"
# allow_plaintext and allow_unauthenticated are local or development only.
```

## Wire feature-owned behavior

Composition or a service adapter holds the `Cache` and `CacheNamespace`.
It translates feature-defined requests and results without introducing a
feature-to-provider dependency or a generic cache interface. `namespace` panics
unless the name matches
`^[a-z][a-z0-9_]{0,63}$`. That is a programmer error. The name is both the
`cache` metric label and the key prefix: a namespace stores `key` as
`{name}:{key}`, so two features sharing one server cannot read each other's
entries. The feature still puts a format version in its key.
When services, environments, or tenants share an endpoint, their identities
must also be part of the feature key wherever they change the result. A
namespace separates key prefixes; it is not an access-control boundary.

```rust
// Adapter/composition code: build the namespace once and retain it here.
// The feature supplies key, TTL, serialization, and fallback policy.
let profiles = cache.namespace("user_profile");
match profiles.get(&key).await {
    Ok(Some(bytes)) => return Ok(bytes),
    Ok(None) | Err(Unavailable) => {}
}
let bytes = load_from_source_of_truth(&key).await?;
let _ = profiles.set(&key, &bytes, ttl).await;
```

The crate documentation of `infra-cache` carries the same example as a
compiled doctest.

`Ok(None)` is a miss. `Err(Unavailable)` is an outage or a timeout. Both take
the source of truth. A best-effort `set` may ignore `Unavailable`. An
operation that cannot run without the cache defines its unavailable behavior
in the feature; the HTTP handler maps that result to HTTP 503. The provider
does not choose the status.

### Concurrent misses and local capacity

The provider does not coalesce concurrent misses. When a load is expensive
and many requests can miss one key at once, use Moka's existing
`try_get_with` or entry insertion API around both the provider read and
source-of-truth load in composition or the adapter. Keeping the Redis GET
outside that scope still sends one GET per caller. Clones of one Moka cache
share the load; separate caches and application replicas do not.

For one missing key and one error type, `try_get_with` evaluates one
initializer while the other callers wait. It shares a completed error with
those waiters without retaining it as a cache entry; a later call can try
again. Dropping the initializer lets a surviving waiter run its own
initializer. Dropping a waiter leaves the original load running. This is
request-owned work, not a task that must finish after all callers leave;
each caller still needs its own deadline. Reuse this mechanism before
adding a flight registry, and do not add a distributed lock to solve a
local stampede.

Moka's `max_capacity` is a best-effort retained-entry target without a
`weigher`, or a retained-weight target with one. An adapter with variable
payloads can account for key and value storage and use a minimum nonzero
entry weight to bound the entry count as well. Neither target is a strict
RSS bound or a bound on loaders and waiters. Bound source admission and
caller concurrency separately: coalescing one hot key does not bound
simultaneous misses for different keys or the request rate of fast failures.

### Freshness and invalidation

The feature defines which source is authoritative, the permitted age of a
result, and the required visibility of writes. TTL bounds retention after a
fill; it does not establish read-your-writes. For either Moka or Redis,
ordinary cache-aside permits this ordering:

1. A reader starts loading the old value.
2. A writer commits the new value and invalidates the key.
3. The reader finishes and stores the old value with a new TTL.

Moka `invalidate` and Redis `DEL` do not fence that late fill. A slow
initializer can also overwrite a replacement inserted while it was loading.
When the service requires stronger visibility, use an authoritative read or
a versioned/conditional publication protocol that rejects the old fill.
The check and publication must be coordinated; a separate version check or
a delayed second delete is not that guarantee. Reliable invalidation events
can support eventual convergence but do not alone prove immediate visibility.

Application replicas using the same Redis endpoint and keys share stored
bytes, not an atomic transaction with the source. An added Moka L1 needs its
own expiry, invalidation and reconnect policy. redis-rs 1.7.1 has experimental
server-assisted client caching, but this profile does not enable
`cache-aio`; Redis tracking observes Redis key changes, not arbitrary
source writes, and does not replace source-load coalescing. Reading server
replicas or failing over adds the provider's replication guarantees;
[Valkey replication](https://valkey.io/topics/replication/) is asynchronous.

An authoritative absence can be a separately retained value only when the
feature accepts its negative TTL and invalidates it after creation. A
provider timeout, transport error or 5xx is unavailability, not absence.
Choose negative retention and failure cooldown separately. Do not serve an
expired authorization result or other freshness-critical value merely to
keep the cache available. Any expiry jitter must stay within the accepted
maximum age and token expiry rather than extending them.

## Failure and budgets

A miss and an outage are degradation, not a failed process. Every `get`,
`set`, and `delete` has one absolute `command_timeout` budget.
That bound covers waiting for a connection and the reply. During an outage each
call costs at most `command_timeout`.

`cache.command_timeout` must satisfy
`2 * cache.command_timeout <= http.request_timeout`, so one degraded cache
call still leaves at least half of the request budget. The rule covers one
call, not a handler: each sequential cache call on the request path can spend
another `command_timeout`, and the example above spends two on a miss (a
`get`, then a `set`). The feature counts its calls: calls × `command_timeout`, plus its
source-of-truth work, plus a reserve for writing the response, must fit in
`http.request_timeout`. With the defaults (100 ms and 8 s) that is not tight.
There is no per-command retry. A timed-out `SET` or `DEL` may already have
taken effect; timeout proves neither success nor absence of the effect. A
stored entry still has its TTL.

Check the source's capacity with a cold, expired or unavailable cache.
Fallback can turn every miss into source work, and local provider limits
multiply across application replicas. The adapter owns admission and its
terminal refusal when source capacity is exhausted; the service owns the
fleet budget and rollout pace. Observe source loads and capacity refusals
beside the cache hit/miss/error series. A healthy cache hit ratio alone does
not prove that degradation will fit those bounds.

Connect, backoff, and TCP are constants, not keys. One owned supervisor opens
canonical redis-rs multiplexed connections, with one current generation and
at most one setup or maintenance operation in progress. Each setup attempt
has a 1 s envelope for file read, client construction, and DNS/TCP/TLS/HELLO.
The existing `backon` schedule starts at 100 ms and doubles with jitter; each
yielded sleep is capped at 2 s. Six retries follow the first attempt, then a
2 s pause starts another chain while the cache has an owner. All setup errors,
including rejected authentication, retry independently of user calls.

TCP nodelay is on. Keepalive is 30 s, then 10 s, with 3 retries where the
platform supports them. Linux `user_timeout` is 10 s. RESP3 disconnects,
command errors (including `READONLY` or `OOM`), and timeouts after dispatch
retire their connection generation immediately. Replacement dial eligibility
may wait until 2 s after that generation's publication. Late failures from an
old generation cannot retire a successor. Waiting for a connection consumes
the caller's command budget but does not cancel setup progress; a command is
dispatched at most once.

The supervisor also sends one PING every 2 s with response budget
`min(command_timeout, 1 s)`. Refresh and PING never overlap or accumulate
missed ticks; a due credential refresh has priority. A PING failure retires
the generation even with no traffic, so unanswered slots from cancelled
callers cannot remain forever. Caller cancellation alone does not retire a
healthy connection. Retirement wakes operations and releases published and
maintenance handles; dropping the last canonical connection clone aborts its
driver, including unanswered slots.

For a public command timeout C, old operation handles last at most
`B = max(C, 1 s)`; successful publications are spaced by 2 s. The conservative
bound is `1 + ceil(B / 2 s)` live generations: two with service-validated
C <= 1 s, or sixteen for a direct caller using C = 30 s. This bounds generation
count and retention time, not bytes or request count under arbitrary fan-in.
These time bounds assume the async executor continues running.

## Readiness and shutdown

The cache does not gate readiness. A gate would turn a cache outage into total
unavailability and contradict degradation. `Cache::connect_lazy` admits
configuration and starts one owned connection supervisor. Construction waits
for no network I/O; the supervisor advances the first setup in the background.
Startup then runs one `probe` check inside a 1 s bound, long enough for
the first DNS, TCP, TLS, and `AUTH` exchange. Success logs
`cache_connected` with `server.address`, `server.port`, and `cache.tls`.
Failure logs `cache_unavailable_at_startup` and startup continues.

A service whose traffic requires the cache opts in by pushing the probe in
`crates/service/src/bootstrap/mod.rs`:

```rust
probes.push(Box::new(cache.probe()));
```

The probe name is `cache`. Connection acquisition uses the caller's startup
or `health.probe_budget` bound; a connected PING has a 1 s internal ceiling
and also stops when its generation retires. Do not add it to liveness.

`Cache`, namespaces, and probes share the application owner. Dropping its last
handle withdraws the connection, requests cancellation, and aborts the
supervisor; it does not wait for a warm-up or retry chain. The supervisor holds
no application-owner cycle. Runtime scheduling completes destruction, and an
already dispatched filesystem OS read may finish without being able to publish
credentials or continue recovery.

Bootstrap records `Option<Cache>` in the startup `Dependencies` and drops it
inside `Dependencies::close`, in the dependency stage after HTTP drain. The
drop is synchronous and adds no wait to `DEPENDENCY_CLOSE`. The same path runs
on startup failure and interrupted startup. A legitimately retained namespace
or probe keeps the cache alive until that handle is released.

## Observability

The histogram is `cache_operation_duration_seconds`. Labels are `cache` (the
namespace name), `operation` (`get`, `set`, or `delete`), and `outcome`
(`hit`, `miss`, `ok`, `error`, `timeout`, or `cancelled`). A failed series
(`error` or `timeout`) also carries `error_type` (`timeout`, `io`, `auth`,
`response`, `parse`, or `other`), so the cause of an outage is visible
without a trace or a debug log. Hit, miss, and error counts are the `_count`
series. A dropped future records `cancelled`.

Hit ratio:

```text
sum(rate(cache_operation_duration_seconds_count{outcome="hit"}[5m])) / sum(rate(cache_operation_duration_seconds_count{operation="get"}[5m]))
```

Failures by cause:

```text
sum by (error_type) (rate(cache_operation_duration_seconds_count{outcome=~"error|timeout"}[5m]))
```

The client span is `cache`, exported under the name `GET`, `SET`, or `DEL`
(`otel.name`), with `otel.kind` `client`, `db.system.name` `redis`,
`db.operation.name` with the same command, plus `cache.name`,
`server.address`, `server.port`, `cache.outcome`, `error.type`, and
`otel.status_code`. On error or timeout one debug event
`cache_operation_failed` carries `cache.name`, `cache.operation`, and
`error.type`; it is not a warning because an outage would log it at the
request rate. Alert on the `error` and `timeout` outcomes of the histogram
instead. `error.type` takes the same values as the `error_type` label; a TLS
handshake failure, including a client certificate the server refuses,
surfaces as `io`, and a `HELLO` refused with `WRONGPASS`
or `NOAUTH`, or a password file that cannot be read when a connection
opens, as `auth`. Setup, direct AUTH, maintenance PING, and command failures
retain only bounded error classification. Metrics, spans, and logs never carry
keys, values, passwords, the DSN, or raw server text, including bridged
dependency logs. `CacheError` Display follows the same rule.

## Operate the server

The tested server is Valkey 9.1.2
(`valkey/valkey:9.1.2-alpine@sha256:48332870af354a799964c0012ae1194a0bf2bf894eb508f945810596dc2d8d11`).
Redis 7.2 and later is compatible for the command subset the client uses:
`GET`, `SET` with `PX`, `DEL`, and `PING`, plus `HELLO 3` (with `AUTH`
inside it), `CLIENT SETINFO`, `SELECT`, and direct `AUTH` for password rotation.
Topology is standalone TCP only.

Set `maxmemory` and an eviction policy, such as `allkeys-lru`. Every entry
carries a TTL, so `volatile-lru` also works on a server this profile does not
share with durable data.

The client does not configure server memory or eviction, and TTL is not a
memory limit. The adapter also owns key/value size and command fan-in bounds;
the connection generation bound above does not supply them.

## Local run and proof

Start the Compose server, then run the proof:

```sh
docker compose -f env/docker-compose.yml up -d valkey
ALLOW_HEAVY=1 make test-integration-cache
```

`make test-integration-cache` is heavy and needs Docker. Without `CACHE_URL`
it starts a throwaway Compose Valkey (`VALKEY_PORT=0`). A shared server can be
passed as a plaintext `redis://` URL in `CACHE_URL`. The proof does not
certify a deployed memory policy.

## Remove the profile

Initialize with `CACHE=none`. The initializer removes the crate, configuration,
Compose service, proof, and this guide together. Changing the selection after
initialization is a refused profile migration. Do not remove the profile by
deleting only a configuration section.
<!-- template:end cache:docs-cache -->
