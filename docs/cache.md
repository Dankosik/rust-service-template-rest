# Cache

<!-- template:begin cache:docs-cache -->
`CACHE=redis` retains an optional bytes-only Redis-protocol cache. It is inert
by default (`CACHE=none`). [Decisions](cache-decisions.md) own the library and
lifecycle choices.

## What the profile retains

The pack is one standalone client. A feature calls `get`, `set`, and `delete`
on a `CacheNamespace` and receives bytes. `Cache::connection` exposes the
native `redis::aio::ConnectionManager` so a later feature can build rate limits
or locks on the same connection. This profile does not build them.

It does not add a generic `Cache<K, V>`, get-or-load, a serializer, a global
TTL, or a lock API. The feature owns keys, serialization, TTL policy, and
invalidation. `set` stores the given bytes with `SET` and `PX`. A TTL below
1 ms panics. That is a programmer error.

## When process-local moka is enough

`moka` 0.12.16 is already the process-local cache. Use it when the data is
per process, the service runs as one replica, and nothing must be shared or
limited across replicas. It does not share state across replicas and cannot
back a distributed rate limit or lock. This profile is for bytes that more
than one replica must see.

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
required unless `allow_unauthenticated` is set. TLS uses native roots, or the
PEM file at `root_ca_path` when that path is set. A CA path on a plaintext
scheme is refused. The `#insecure` fragment is refused.

`allow_plaintext` and `allow_unauthenticated` are accepted only when `app.env`
is `local` or `development`. `command_timeout` uses a human duration, in a file
or in `APP__CACHE__COMMAND_TIMEOUT`. Its default is `100ms`. The inclusive
range is `1ms` to `1s`. DSN form checks stay in `infra-cache`. Configuration
errors fail startup with a sanitized message.

```toml
[cache]
command_timeout = "100ms"
# dsn is environment-only: APP__CACHE__DSN
# allow_plaintext and allow_unauthenticated are local or development only.
```

## Use it from a feature

Hold a `Cache` from composition. `namespace` panics unless the name matches
`^[a-z][a-z0-9_]{0,63}$`. That is a programmer error. The name is both the
`cache` metric label and the key prefix: a namespace stores `key` as
`{name}:{key}`, so two features sharing one server cannot read each other's
entries. The feature still puts a format version in its key.

```rust
let profiles = cache.namespace("user_profile");
let bytes = match profiles.get(&key).await {
    Ok(Some(bytes)) => bytes,
    Ok(None) | Err(Unavailable) => load_from_source_of_truth(&key).await?,
};
let _ = profiles.set(&key, &bytes, ttl).await;
```

`Ok(None)` is a miss. `Err(Unavailable)` is an outage or a timeout. Both take
the source of truth. A best-effort `set` may ignore `Unavailable`. An
operation that cannot run without the cache maps `Unavailable` to HTTP 503 in
that handler. The adapter does not choose the
status.

## Failure and budgets

A miss and an outage are degradation, not a failed process. Every `get`,
`set`, and `delete` runs inside `tokio::time::timeout(command_timeout)`.
That bound covers waiting for a reconnect and the reply. During an outage each
call costs at most `command_timeout`.

`cache.command_timeout` must satisfy
`2 * cache.command_timeout <= http.request_timeout`, so one degraded cache
call still leaves at least half of the request budget. The rule covers one
call, not a handler: each sequential cache call on the request path can spend
another `command_timeout`, and the example above spends two (a `get`, then a
`set`). The feature counts its calls: calls × `command_timeout`, plus its
source-of-truth work, plus a reserve for writing the response, must fit in
`http.request_timeout`. With the defaults (100 ms and 8 s) that is not tight.
There is no per-command retry. A timed-out `SET` is ambiguous, and the TTL
bounds how long a missed write can stay stale.

Connect, backoff, and TCP are constants, not keys. Connect waits at most 1 s.
Reconnect backoff starts at 100 ms, doubles, and caps at 2 s, with 6 retries.
TCP nodelay is on. Keepalive is 30 s, then 10 s, with 3 retries where the
platform supports them. On Linux, `user_timeout` is 10 s so a half-open
connection is detected and reconnected.

redis 1.7.1 reconnects only after an I/O error. A connection whose setup
fails otherwise, for example `AUTH` refused while a failover saturates the
server, would stay failed until the process restarts. The cache replaces that
connection from the retained client, at most once per 2 s, so it recovers
when the server accepts it again. A `READONLY` reply (a demoted primary after
a failover) replaces the connection the same way, at most once per 2 s. The
first lazy connection, and a manager the cache replaced, advance only while a
call is waiting on it; the manager's own reconnect after an I/O error runs in
the background. With sparse traffic, recovery can take a few calls. A command
timeout does not by itself reconnect.

## Readiness and shutdown

The cache does not gate readiness. A gate would turn a cache outage into total
unavailability and contradict degradation. `Cache::connect` admits
configuration and builds a lazy `ConnectionManager`. It does no network I/O.
Startup then runs one `probe` check inside a 1 s bound, long enough for
the first DNS, TCP, TLS, and `AUTH` exchange. Success logs
`cache_connected` with `server.address`, `server.port`, and `cache.tls`.
Failure logs `cache_unavailable_at_startup` and startup continues.

A service whose traffic requires the cache opts in by pushing the probe in
`crates/service/src/bootstrap/mod.rs`:

```rust
probes.push(Box::new(cache.probe()));
```

The probe name is `cache`. It sends `PING` and has no timeout of its own: the
readiness refresher bounds it with `health.probe_budget`. Do not add it to liveness.

Dropping the last `ConnectionManager` clone closes the socket. Bootstrap
carries `Option<Cache>` into the shutdown plan and drops it inside
`close_dependencies`, in the dependency stage after HTTP drain. The drop is
synchronous, so it does not add to `DEPENDENCY_CLOSE`. The same drop runs on
the startup-failure and stopped-startup paths.

## Observability

The histogram is `cache_operation_duration_seconds`. Labels are `cache` (the
namespace name), `operation` (`get`, `set`, or `delete`), and `outcome`
(`hit`, `miss`, `ok`, `error`, `timeout`, or `cancelled`). Hit, miss, and
error counts are the `_count` series. A dropped future records `cancelled`.

Hit ratio:

```text
sum(rate(cache_operation_duration_seconds_count{outcome="hit"}[5m])) / sum(rate(cache_operation_duration_seconds_count{operation="get"}[5m]))
```

The client span is `cache`, with `otel.kind` `client`, `db.system.name`
`redis`, `db.operation.name` `GET`, `SET`, or `DEL`, plus `cache.name`,
`server.address`, `server.port`, `cache.outcome`, `error.type`, and
`otel.status_code`. On error or timeout one debug event
`cache_operation_failed` carries `cache.name`, `cache.operation`, and
`error.type`; it is not a warning because an outage would log it at the
request rate. Alert on the `error` and `timeout` outcomes of the histogram
instead. `error.type` is `timeout`, `io`, `auth`, `response`, `parse`, or
`other`; a TLS handshake failure surfaces as `io`. Metrics, spans, and logs never carry keys, values, the DSN, or raw server text. `CacheError` Display follows the same
rule.

## Operate the server

The tested server is Valkey 9.1.2
(`valkey/valkey:9.1.2-alpine@sha256:48332870af354a799964c0012ae1194a0bf2bf894eb508f945810596dc2d8d11`).
Redis 7.2 and later is compatible for the command subset the client uses:
`GET`, `SET` with `PX`, `DEL`, and `PING`, plus `HELLO`, `AUTH`, and `SELECT`
by the client. Topology is standalone TCP only.

Set `maxmemory` and an eviction policy, such as `allkeys-lru`. Every entry
carries a TTL, so `volatile-lru` also works on a server this profile does not
share with durable data.

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
