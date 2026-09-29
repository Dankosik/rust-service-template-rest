# Configuration Source Policy

This template uses a strict split between non-secret and secret
configuration. The loader is the `config` crate with `serde`; the policy below
is what the template adds on top. The `service-config` crate owns it.

## Source Of Truth

- TOML files (`env/config/*.toml`) hold baseline non-secret defaults. TOML is
  the Rust ecosystem convention; a service that must consume YAML enables
  config-rs's `yaml` feature rather than adding a YAML crate.
- Environment variables (`APP__SECTION__KEY`) hold per-environment overrides
  and every application-owned secret. `APP__HTTP__ADDR` sets `http.addr`;
  `APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_HEADERS` sets the collector
  credential. Profile-specific inputs are defined only when their section
  exists in the local typed snapshot.
- CLI flags are loader controls: `--config PATH` selects the base file and
  `--config-overlay PATH` (repeatable, ordered) adds overlays. They never set
  individual keys, and a positional argument is refused.

Runtime value precedence, last wins:

1. code defaults (`impl Default` beside each section type)
2. `--config` base file
3. `--config-overlay` files in order
4. `APP__` environment variables

An empty `APP__` value is still an explicit final override; it flows into
validation and fails when the key cannot be empty. Unknown keys from files or
the environment fail startup (`#[serde(deny_unknown_fields)]` on every
section), and so does a malformed variable name such as `APP____ADDR` or
`APP__HTTP__ADDR__`. Because every `APP__*` variable is read, an unrelated
`APP__FOO` in the process environment also fails startup: name the namespace
for this service only.

Values keep their human forms in both files and the environment: durations
as `"8s"`, `"250ms"`, `"1m 30s"`; byte sizes as `"1 MiB"`, `"16 KiB"`, or a
plain integer; booleans as `true`/`false`; enums by their documented spelling.

## Secret Rules

- Do not place secrets in TOML. A secret-like key (any segment `password`,
  `secret`, `secrets`, `authorization`, `dsn`, `token` unless followed by
  `profile` or `url`, `key` after `api` or `private`, `headers` after `otlp`)
  with a non-empty value in any file fails startup. Empty placeholders are
  allowed so a file can document the key.
- Secret fields are `secrecy::SecretString`: `Debug` output and the startup
  summary print `[REDACTED]`, and the value is zeroed on drop.
- A secret map given a scalar (`APP__WEBHOOKS__SECRETS=value`, the reference
  segment omitted) fails with its key and a static reason; config-rs's default
  type diagnostic would echo the value.
- Files are read as the process user; relative paths and symlinks are
  accepted because Kubernetes projected volumes depend on symlinks for atomic
  updates.
<!-- template:begin postgres:docs-config-postgres-source -->
- `postgres.dsn` is the only PostgreSQL connection source. It must be a
  `postgres://` URL with explicit host, port, user, password, database, and
  `sslmode` (`disable`, `require`, `verify-ca`, `verify-full`), plus an
  optional absolute `sslrootcert` CA file under `verify-ca`/`verify-full`,
  and nothing else; libpq variables that would still merge into it
  (`PGSSLROOTCERT`, `PGSSLCERT`, `PGSSLKEY`, `PGOPTIONS`), `.pgpass`,
  service files, socket paths, and client key or certificate files are
  refused at startup, and the diagnostic never carries the value
  ([Persistence](architecture/persistence.md#connection-admission)).
<!-- template:end postgres:docs-config-postgres-source -->
<!-- template:begin messaging:docs-config-messaging-source -->
- `messaging` is an optional typed section. Its non-secret endpoint, stream,
  consumer, DLQ, TLS, timeout, concurrency, and delivery-size inputs use normal
  file/environment precedence; credentials are `SecretString`, environment-only,
  and redacted. Active consumption requires complete named topology and distinct
  source/DLQ subjects. Local plaintext or unauthenticated use is an explicit
  development/test escape hatch, never a production default. Configuration
  validates shape and resource bounds before any provider I/O; the adapter maps
  the admitted snapshot to its client options.
  `messaging.urls` uses a TOML array or one comma-separated
  `APP__MESSAGING__URLS` value, for example
  `tls://nats-a.example:4222,tls://nats-b.example:4222`. A single URL is written
  directly; JSON array syntax is not an environment format. List parsing is
  confined to this key, so other environment strings, including credentials,
  keep their exact bytes.
<!-- template:end messaging:docs-config-messaging-source -->
<!-- template:begin cache:docs-config-cache-source -->
- `cache` is an optional typed section. It is active when `dsn` is set.
  `cache.dsn` is `SecretString`, environment-only (`APP__CACHE__DSN`), and
  redacted. A missing, empty, or whitespace-only value is absent. A nonempty
  file value is refused because `dsn` is secret-like. `root_ca_path` and
  `command_timeout` use normal file/environment precedence. `allow_plaintext`
  and `allow_unauthenticated` are accepted only when `app.env` is `local` or
  `development`. Admitted schemes are `redis`, `rediss`, `valkey`, and
  `valkeys`. `#insecure` and a unix socket are refused; Sentinel and Cluster
  URLs are not admitted. A CA path requires TLS. DSN form checks stay in `infra-cache`. The
  [guide](cache.md) owns admission.
<!-- template:end cache:docs-config-cache-source -->
<!-- template:begin object-storage:docs-config-object-storage-source -->
- `object_storage` is an optional typed section, inert while `provider` is
  `none`. `object_storage.secret_access_key` is `SecretString`,
  environment-only (`APP__OBJECT_STORAGE__SECRET_ACCESS_KEY`), and redacted;
  a nonempty file value is refused because `secret` is secret-like.
  `provider`, `bucket`, `region`, `endpoint`, `expected_bucket_owner`, and
  `access_key_id` use normal file/environment precedence. Each provider
  accepts only its own keys: `amazon_s3` takes `region` and
  `expected_bucket_owner` and no endpoint, `cloudflare_r2` and `railway` take
  `endpoint`, and a key another provider owns fails startup instead of being
  ignored. `local` (an emulator, plaintext allowed) is accepted only when
  `app.env` is `local` or `development`. The client is built from these keys
  alone: the global `AWS_*` variables, AWS profile files, and instance
  metadata are never read. On Railway, map the bucket's `${{Bucket.X}}`
  variables onto `APP__OBJECT_STORAGE__*`. Value shapes (endpoint origin,
  region, bucket name, owner account) are admitted by `infra-object-storage`.
  The [guide](object-storage.md) owns admission.
<!-- template:end object-storage:docs-config-object-storage-source -->
<!-- template:begin outbox:docs-config-outbox-source -->
- `OUTBOX=postgres` is initializer/profile selection, not a configuration
  section or runtime switch. It requires the retained PostgreSQL, jobs, and
  JetStream messaging capabilities. Its retry, snooze, dedupe, capacity, and
  shutdown rules are code-owned; it adds no operator tuning key.
<!-- template:end outbox:docs-config-outbox-source -->
<!-- template:begin authn:docs-config-authn-source -->
- `authn.mode` defaults to `none`. A retained initialized profile admits only `none` plus its selected engine. The `none` variant accepts no provider fields; an active engine needs exact, nonblank `authn.issuer` and `authn.audience`. Issuer, audience, and identity values are not trimmed or case-folded. `AuthnConfig` Debug exposes only mode; trust inputs, endpoints, queries and credentials remain redacted.
<!-- template:end authn:docs-config-authn-source -->
<!-- template:begin oidc-jwt:docs-config-jwt-source -->
- JWT mode accepts `authn.token_profile = "resource-server"` or `"rfc9068"`, with `resource-server` as the omitted-value default; it accepts a nonempty `authn.algorithms` list of `RS256`, `ES256`, `PS256`, or `EdDSA`. `authn.audience` accepts one string or a nonempty exact-string list. JWT provider configuration does not accept introspection fields.
<!-- template:end oidc-jwt:docs-config-jwt-source -->
<!-- template:begin oidc-introspection:docs-config-introspection-source -->
- Introspection mode requires `authn.introspection_endpoint`, `authn.introspection_client_id`, a nonzero `authn.provider_concurrency` (default 32), and a nonempty `APP__AUTHN__INTROSPECTION_CLIENT_SECRET`. Its client secret is `SecretString`, environment-only, and must never appear in TOML; the mode rejects JWT-only inputs.
- Introspection alone accepts `authn.cache_enabled` (default `false`), `authn.cache_capacity` (default 256, inclusive 1–1024), and `authn.cache_ttl` (human duration, default `"30s"`, inclusive `"1s"`–`"5m"`). These non-secret values use normal file/environment precedence (`APP__AUTHN__CACHE_ENABLED`, `APP__AUTHN__CACHE_CAPACITY`, `APP__AUTHN__CACHE_TTL`). Config owns typed input/defaults; the adapter cache-options constructor invoked by bootstrap owns numeric range validation. Invalid bounds fail startup before provider I/O or listeners even when caching is disabled. Enabled positive reuse ends at the earlier of the fixed TTL and token expiry, without expiry leeway; it can delay observing revocation or provider outages for that interval. See [Authentication](authentication.md#oidc-introspection) for storage bounds and miss behavior.
<!-- template:end oidc-introspection:docs-config-introspection-source -->
<!-- template:begin http-idempotency:docs-config-http-idempotency -->
- `http_idempotency.retention` (environment `APP__HTTP_IDEMPOTENCY__RETENTION`)
  is a non-secret human-readable duration, in a TOML file or the environment.
  An empty or whitespace-only value is vacant, like `app.instance_id`. A set
  value must fall within the inclusive range of 1 minute to 30 days. It is
  required only when at least one idempotent operation is served; an
  inactive boundary needs no value.
<!-- template:end http-idempotency:docs-config-http-idempotency -->
<!-- template:begin outbound-auth:docs-config-outbound-auth -->
- `integrations.<name>.oauth` is an immutable optional OAuth2 client-credentials
  tuple. Empty integration maps and entries without `oauth` are inert; a present
  tuple must contain valid `token_url`, `client_id`, and an environment-only
  nonempty `APP__INTEGRATIONS__<NAME>__OAUTH__CLIENT_SECRET`. Nonsecret
  `scopes` and optional `audience` follow normal TOML/environment layering;
  scopes use a TOML list or one space-separated environment value. File secrets
  are refused by the recursive secret guard. The [outbound machine-authentication
  guide](outbound-machine-authentication.md) owns endpoint admission and
  provider compatibility.
<!-- template:end outbound-auth:docs-config-outbound-auth -->

## OpenTelemetry Environment Policy

<!-- template:begin grpc:docs-config-grpc -->
The optional `grpc` section defaults disabled. Enabling it requires an address
and explicit plaintext or TLS security; bearer verification remains valid with
either mode. PEM certificate/CA values are ordinary configuration, while
`grpc.private_key` and `integrations.<name>.grpc.private_key` are environment-only
secrets. The transport builds the listener config at startup. A disabled
listener performs no TLS or network work. Config Debug omits all trust and
identity material. Client integration inputs select a trusted destination,
explicit security, optional CA and optional paired certificate/key; they do
not create a client registry or token owner. See [gRPC](grpc.md).
<!-- template:end grpc:docs-config-grpc -->

Typed configuration owns service identity and takes precedence; the official
OpenTelemetry environment stays a supported platform fallback:

- `service.name`, `service.version`, `vcs.ref.head.revision`,
  `service.instance.id`, and `deployment.environment.name` come from the
  typed snapshot (`observability.otel.service_name`, `app.version`,
  `app.commit`, `app.instance_id`, `app.env`). Additional
  `OTEL_RESOURCE_ATTRIBUTES` survive underneath them. A missing or empty
  `app.instance_id` is occupancy: the composition root fills the hostname,
  which is the pod name on Kubernetes.
- A typed `observability.otel.exporter.otlp_endpoint` wins. Missing, empty, or
  whitespace-only is vacant occupancy, the same rule as `app.instance_id`.
  A typed URL without a path is a collector root and gets `/v1/traces`.
  Otherwise the SDK
  reads `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`, then `OTEL_EXPORTER_OTLP_ENDPOINT`
  as the collector root. When neither holds a non-blank value the exporter
  stays disabled: spans still get trace ids for log correlation but nothing
  is exported. A blank variable is vacant because the SDK would otherwise
  fall back to `localhost:4318`.
- When the typed endpoint selects the destination, an occupied
  `OTEL_EXPORTER_OTLP_HEADERS` or `OTEL_EXPORTER_OTLP_TRACES_HEADERS` fails
  startup: the SDK merges them over the typed headers, so one collector's
  credential would reach another. Configure the credential through
  `APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_HEADERS`; a malformed
  `name=value` entry there fails startup instead of being dropped.
- The OTLP/HTTP exporter verifies the collector with the platform trust
  store and does not read `..._CERTIFICATE`, `..._CLIENT_KEY`, or
  `..._CLIENT_CERTIFICATE`. An occupied one is named in a startup warning.
- When the platform supplies the endpoint, it also owns the matching standard
  credentials, trust material, and exporter tuning.
- The sampler is always typed (`observability.otel.traces_sampler` and
  `traces_sampler_arg`, default `parentbased_traceidratio` at `0.10`);
  `OTEL_TRACES_SAMPLER` is not consulted.

Telemetry setup failures never block the service: a failed exporter build is
logged with a bounded reason and the process continues with the exporter
`degraded`. The startup summary carries `tracing.exporter` as `initialized`,
`disabled`, or `degraded`, and the diagnostics listener exposes
`service_startup_trace_exporter_active`. This is a startup-configuration
signal, not continuous delivery health.

`observability.metrics.addr` owns the Prometheus diagnostics listener. It
defaults to `:9090`, which binds IPv4 all-interfaces (`0.0.0.0`) so a scraper in another pod
can reach it; an empty value disables HTTP exposition. Binding failure blocks
startup. Deployment network policy must keep this listener private.

## Logging

`log.level` is a `tracing_subscriber::EnvFilter` directive (`info`,
`debug`, `info,hyper=warn`). `RUST_LOG` is not read; `APP__LOG__LEVEL` is the
override channel like every other key. `log.format` is `json` (production:
one flattened object per line with `openTelemetry.traceId` and `spanId` on
every record inside a request) or `text` (local development).

## Runtime Budget Policy

- `http.request_timeout` (default `8s`) is the per-request handler budget and
  the only bound on how long one request may hold a task and its pooled
  resources. Body reads happen inside it because extractors run inside the
  handler future. Expiry answers a `504` problem with code `request_timeout`.
  It must not exceed the drain budget left after readiness propagation, so
  in-flight requests can finish inside the drain.
- `http.header_read_timeout` (default `5s`) bounds delivery of a request head
  and, because hyper restarts it whenever an HTTP/1 connection goes idle, is
  also the HTTP/1 keep-alive idle bound. HTTP/2 idle uses a separate PING
  cadence. It also closes a client that connects and sends nothing.
- `health.probe_budget` (default `4s`) bounds one background readiness
  evaluation across every probe; `/health/ready` itself never runs a probe.
  The verdict names the probe that ran out of the budget.
- `http.drain_timeout` (default `25s`) bounds the HTTP drain, including
  the `http.readiness_propagation_delay` (default `15s`) in front of it.
  `http.grace_period` (default `45s`) is the platform's SIGTERM-to-SIGKILL
  window; it must cover `drain_timeout` plus the `17s` teardown tail
  (diagnostics close `2s`, background join `5s`, dependency close `5s`,
  telemetry flush `5s`). The default worst case is 42 seconds inside 45.

  **This is a deployment precondition on every platform.** Configure the
  grace period explicitly:

  | Platform | Setting |
  | --- | --- |
  | Kubernetes | `terminationGracePeriodSeconds: 50` |
  | Docker | `docker run --stop-timeout 45` / `docker stop --time 45` |
  | Compose | `stop_grace_period: 45s` |
  | ECS | `stopTimeout: 45` |

  Changing `http.drain_timeout` changes this number; re-derive it.
- `http.max_header_bytes` (default `16 KiB`) is the HTTP/1 read-buffer
  ceiling for one request head; overflow answers hyper-native `431` before
  the router, not a Problem. HTTP/2 applies the same number as uncompressed
  header-list size. hyper refuses HTTP/1 values below `8 KiB`.
  `http.max_body_bytes` (default `1 MiB`) answers `413`.
- `http.max_in_flight` (default `256`) bounds concurrent handler executions;
  the excess is shed with `503` and `Retry-After: 1` without queueing. Zero
  disables shedding. `http.max_connections` (default `4096`) bounds accepted
  connections; the excess is closed at accept without a response. It must be
  at least `max_in_flight` so the informative rejection stays the common one.
- `http.access_log_health_probes` defaults to `false`, so matched
  `GET /health/live` and `GET /health/ready` requests are served without an
  access-log line. The exclusion is by route template: an unmatched path that
  merely resembles a probe is still recorded.
- `health.refresh_interval` (default `2s`), `health.probe_budget`
  (default `4s`), and `health.failure_threshold` (default `3`) drive the
  background readiness refresher. A cached verdict older than the staleness
  bound is refused, so a dead refresher cannot leave a stale "healthy"
  standing; `health::RefreshPolicy::stale_after` owns the formula.
<!-- template:begin postgres:docs-config-postgres-budget -->
- `postgres.enabled` (default `false`) selects the PostgreSQL profile;
  `postgres.max_connections` (default `4`, `1..500`) is the pool's upper
  bound and the one database capacity value an operator sets. The acquire
  budget (`3s`), the session `statement_timeout` and
  `idle_in_transaction_session_timeout` (`8s`), and the migration budgets
  are constants in the adapter
  ([Persistence](architecture/persistence.md#budgets)); the readiness probe
  draws `health.probe_budget`, and the pool closes inside the `5s`
  dependency-close stage. Enabled service and worker startup also bound the
  read-only embedded migration-history check to `5s`, including pool acquire.
  This is a separate sequential startup step, with no new configuration key.
<!-- template:end postgres:docs-config-postgres-budget -->
<!-- template:begin jobs:docs-config-jobs -->
- `jobs.max_workers` (environment `APP__JOBS__MAX_WORKERS`, default `1`,
  `1..500`) is the most attempts one jobs worker process runs at once; every
  binary validates it and only the worker uses it. The worker refuses
  `postgres.max_connections` below `jobs.max_workers + 2`. The worker reuses
  `http.grace_period` and `http.drain_timeout` with its own `17s` teardown
  tail (release `2s`, listeners `2s`, background join `3s`, dependency close
  `5s`, telemetry flush `5s`) and no readiness propagation delay, so its
  default worst case is also 42 seconds inside 45, and the platform settings
  above fit both entrypoints. The worker derives its identity from
  `observability.otel.service_name` as `{service_name}-jobs-worker` (its
  OpenTelemetry `service.name` and its PostgreSQL `application_name`, with
  the service name cut to 51 bytes so the suffix survives PostgreSQL's
  63-byte limit); no key controls it.
  See the [guide](background-jobs.md#configure-and-size-the-worker).
<!-- template:end jobs:docs-config-jobs -->
<!-- template:begin authn:docs-config-authn-budgets -->
- Authentication provider calls have an independent fixed three-second cap through body completion; discovery plus initial keys share a six-second startup cap. Authentication accepts no request deadline or response reserve. The outer hardened timer alone emits `504 request_timeout`; a completed provider timeout is `503 authentication_unavailable` while the request is live. Introspection admits its configured number of simultaneous exchanges and rejects excess distinct misses as unavailable without queueing; live cache hits and coalesced waiters need no extra permit.
<!-- template:end authn:docs-config-authn-budgets -->
<!-- template:begin cache:docs-config-cache-budget -->
- `cache.command_timeout` (environment `APP__CACHE__COMMAND_TIMEOUT`, default
  `100ms`, inclusive `1ms` to `1s`) bounds one cache call, including reconnect
  wait. It must satisfy `2 * cache.command_timeout <= http.request_timeout`,
  so one degraded call still leaves at least half of the request budget; a
  feature with several sequential cache calls budgets each of them. Connect, backoff, and TCP stay adapter constants. During an
  outage each call costs at most `command_timeout`. There is no per-command
  retry. See the [cache guide](cache.md).
<!-- template:end cache:docs-config-cache-budget -->
<!-- template:begin object-storage:docs-config-object-storage-budget -->
- `object_storage.operation_timeout` (environment
  `APP__OBJECT_STORAGE__OPERATION_TIMEOUT`, default `5s`, inclusive `1s` to
  `15m`) bounds one call up to its response headers, a read's three attempts
  included (a put or delete makes one); connect stays a `3.1s` constant and a
  download body is bounded by the SDK's stalled-stream protection (5 s without
  progress) instead. On a request path the handler
  budget still applies: a put dropped by `http.request_timeout` has an unknown
  outcome. `object_storage.max_concurrency` (default `8`, `1..512`) admits
  that many calls at once and refuses the excess without queueing; a download
  holds its slot until its body ends. `object_storage.max_object_bytes`
  (default `8 MiB`, at most 4.995 GiB, the smallest single-upload limit of the
  supported providers) bounds a put and a get. Buffered reads cost up to
  `max_concurrency * max_object_bytes` of memory. See the
  [object storage guide](object-storage.md).
<!-- template:end object-storage:docs-config-object-storage-budget -->

## Adding A Config Key

1. Add the typed field, its default in the section's `impl Default`, and its
   validation, all in the section's own file under `crates/config/src/`. One
   section is one file: the reason a value was chosen sits beside the rule
   that enforces it.
2. Add a loader test in `crates/config/src/load.rs` that sets the key through
   the environment and asserts the decoded value, and a validation test for a
   rejected value.
3. Update `env/config/local.toml` only where the key belongs for a non-secret
   local example.
4. Update this document when the key changes secret-source or runtime-budget
   behaviour.

`#[serde(deny_unknown_fields, default)]` on the section type is what makes an
undeclared key fail and a missing key take its default; there is no second
registry to maintain.

## Decisions Recorded Here

<!-- template:begin webhooks-common:docs-config-webhooks-snapshot -->
## Webhook configuration snapshot

Webhook configuration follows normal typed TOML/environment layering and is an
immutable startup snapshot: configuration or secret rotation takes effect only
after restart. Secret values are `SecretString` values, supplied only through
the corresponding `APP__...` environment paths and never a file. The recursive
secret-file guard covers endpoint fields and dynamic maps. There is no
environment-variable indirection, JSON-in-environment manifest, or remote secret
provider.

Endpoint IDs and non-secret key references need only be nonempty and NUL-free.
<!-- template:end webhooks-common:docs-config-webhooks-snapshot -->

<!-- template:begin webhooks:docs-config-webhooks-outbound -->
`webhooks.endpoints` maps endpoint IDs to destination URL, required `secret`,
and optional `previous_secret`. There is no outbound `webhooks.secrets` map or
reference field. Those values are supplied through
`APP__WEBHOOKS__ENDPOINTS__<ID>__SECRET` and `...__PREVIOUS_SECRET`; the
required secret and an explicit predecessor cannot be blank. Provider
construction owns URL admission and decoded Standard Webhooks keys of 24--64
bytes. Outgoing producers receive only configured endpoint IDs and final bytes;
workers build each endpoint's client and keys before claiming jobs. A restart with
changed URL or keys applies to pending work. Keep the predecessor secret through
the rotation cutover, then remove it and restart.
<!-- template:end webhooks:docs-config-webhooks-outbound -->

<!-- template:begin inbound-webhooks:docs-config-webhooks-inbound -->
`inbound_webhooks.endpoints` maps endpoint IDs to active/optional previous key
references; `inbound_webhooks.secrets` maps references to verification keys. The
receiving service resolves keys; a processing worker consumes verified durable
work without them. Empty endpoint maps stay inert. Active endpoint binding or
PostgreSQL failure is a causal startup error without secret values.

Inbound route construction percent-encodes an endpoint ID as one path segment;
configuration does not add a URL-slug grammar.
<!-- template:end inbound-webhooks:docs-config-webhooks-inbound -->

Made in stage 2 with the research behind them; a later change reopens one
only with new evidence.

| Decision | Alternative rejected | Why |
| --- | --- | --- |
| The `config` crate (`toml` feature only) with `serde`, layered builder, `#[serde(deny_unknown_fields, default)]` per section | `figment` | no release since 2024 and it silently drops a malformed environment name; config-rs reports the unknown field, and the template's pre-scan names the variable |
| TOML baseline files | YAML | the Rust convention with a maintained crate; `serde_yaml` is archived, `serde_yml` carries RUSTSEC-2025-0068; config-rs's `yaml` feature stays available for a service that must consume YAML |
| Secrets as `secrecy::SecretString`; environment is the only secret source; each file is pre-scanned for non-empty secret-like keys (`password`, `secret`, `token`, `dsn`, `authorization`, `api_key`, `private_key`, `otlp_headers`) | trusting file contents | a committed baseline cannot leak a credential; `Debug` prints `[REDACTED]` |
| `tracing` + `tracing-subscriber` `EnvFilter`; `json-subscriber` for `log.format = json`, `fmt::layer()` for `text`; `log` records bridged | a bespoke JSON layer | flattened event and span fields plus trace and span ids on every record inside a request come from the crate |
| Tracer provider always installed; the OTLP HTTP/protobuf batch exporter added only when a typed endpoint or a standard `OTEL_EXPORTER_OTLP_*ENDPOINT` resolves one; `TraceContextPropagator` installed explicitly | exporter `disabled` when no endpoint, provider absent | trace ids in every log line cost nothing without an exporter and avoid connection-refused noise against the SDK's `localhost:4318` default |
| Ambient `OTEL_EXPORTER_OTLP_*HEADERS` fail validation when the typed endpoint selects the destination; unread trust variables are named in a startup warning | letting the SDK merge them | one collector's credential is never sent to another; the mechanism stays the SDK's, the safety property is a validation rule |
| The `metrics` facade with `metrics-exporter-prometheus` (`default-features = false`), the HTTP adapter's own server metrics under the `axum-prometheus` names (route template or `<unmatched>`), `metrics-process`, `tokio-metrics` | OpenTelemetry SDK metrics with `opentelemetry-prometheus` and OTLP push | the facade is the dominant Rust idiom, process and Tokio metrics have no OTel-native crates, and `opentelemetry-prometheus` was deprecated, un-deprecated, and is still Beta. A collector `prometheus` receiver scraping `:9090` serves OTLP-only platforms |
| `log.format` added; `runtime.memory_limit_ratio`, `GOMAXPROCS` awareness, and `observability.pprof` not ported | Go parity | human-readable local logs are a Rust convention; there is no garbage collector, `available_parallelism` honours cgroup quotas, and there is no standard-library profiler to expose |

Version discipline: every OpenTelemetry crate stays on one minor and moves
together, with `tracing-opentelemetry` one ahead (0.33 ↔ 0.32). A dependency
that pins another minor creates a second `global::` whose data goes to a
no-op provider silently; check `cargo tree -d -i opentelemetry` after a
dependency change. A dependency enabling `opentelemetry-otlp/reqwest-client`
flips the exporter to the async client, which the batch processor does not
support; check `cargo tree -e features -i opentelemetry-otlp`.
`reqwest-rustls` must stay enabled: without it an `https://` collector
fails at export with reqwest's "URL scheme is not allowed". That feature
selects rustls + `aws-lc-rs` and the platform verifier (system roots in
the distroless image). Do not add `reqwest-client` beside
`reqwest-blocking-client`.
`metrics-exporter-prometheus` default features pull `push-gateway` and a TLS
stack; keep them off. OTLP metric push and OTLP logs
(`opentelemetry-appender-tracing`) are deferred until a platform requires
them; both resolve into the same version set.
