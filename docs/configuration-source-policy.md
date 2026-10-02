# Configuration Source Policy

This template uses a strict split between non-secret and secret
configuration. The loader is the `config` crate with `serde`; the policy below
is what the template adds on top. The `service-config` crate owns it.

## Source Of Truth

- TOML files (`env/config/*.toml`) hold baseline non-secret defaults. TOML is
  the Rust ecosystem convention; a service that must consume YAML enables
  config-rs's `yaml` feature rather than adding a YAML crate.
- `APP__SECTION__KEY` variables hold per-environment overrides and every
  application-owned secret. `APP__HTTP__ADDR` sets `http.addr`;
  `APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_HEADERS` sets the collector
  credential. A variable has two carriers: the process environment, and a
  file of the same name in the [secrets directory](#secrets-directory).
  Profile-specific inputs are defined only when their section exists in the
  local typed snapshot.
- CLI flags are loader controls: `--config PATH` selects the base file,
  `--config-overlay PATH` (repeatable, ordered) adds overlays, and
  `--secrets-dir PATH` names the secrets directory. They never set
  individual keys, and a positional argument is refused.

Runtime value precedence, last wins:

1. code defaults (`impl Default` beside each section type)
2. `--config` base file
3. `--config-overlay` files in order
4. `APP__` variables in the secrets directory
5. `APP__` variables in the process environment

An empty `APP__` value is still an explicit final override; it flows into
validation and fails when the key cannot be empty. An optional text, path, or
secret key reads an empty or whitespace-only value as unset, so an empty
variable also unsets what a file set; the exceptions are the trust inputs
whose section says a blank value is refused, which fail validation instead. Unknown keys from files or
variables fail startup (`#[serde(deny_unknown_fields)]` on every
section), and so does a malformed variable name such as `APP____ADDR`,
`APP__HTTP__ADDR__`, or `APP__HTTP[0]`: each segment is letters, digits, `_`,
or `-`. Because every `APP__*` variable is read, an unrelated
`APP__FOO` in the process environment also fails startup: name the namespace
for this service only. Two spellings of one name in one carrier, such as
`APP__HTTP__ADDR` beside `APP__http__addr`, fail startup, and so does a
variable whose value is not valid Unicode; neither is resolved by guessing.

A variable name is lowercased and split on `__`, and each segment must be a
path identifier, so every key in a file must be one a variable can address:
lowercase letters, digits, `_`, and `-`, without `__` or a trailing `_`. A
file table named `Partner` fails startup naming the file and the key, because
a variable segment `__PARTNER__` would set a second entry, `partner`, and the
file's entry could never receive an environment-only secret or an override.
The same rule applies to a value that refers to an environment-supplied entry.

Values keep their human forms in both files and the environment: durations
as `"8s"`, `"250ms"`, `"1m 30s"`; byte sizes as `"1 MiB"`, `"16 KiB"`, or a
plain integer; booleans as `true`/`false`; enums by their documented spelling.
Because a variable is always text, config-rs converts between scalar forms on
demand in every section: numeric text sets a number, and an unquoted TOML
number or boolean given to a text key is read as its text (`1.50` as `1.5`).
Quote text in TOML.

## Secret Rules

- Do not place secrets in TOML. A secret-like key (any segment `password`,
  `secret`, `secrets`, `credentials`, `authorization`, `dsn`, `token` unless
  followed by `url`, `key` after `api` or `private`, `headers` after `otlp`)
  with a non-empty value in any file fails startup. Empty placeholders are
  allowed so a file can document the key. The guard reads key names, not
  types: a new secret field needs a name it recognizes.
- "Environment-only" in these documents and in the section doc comments
  means: set through an `APP__` variable, from either carrier, and never in a
  TOML file.
- Secret fields are `secrecy::SecretString`: `Debug` output and the startup
  summary print `[REDACTED]`, and the value is zeroed on drop.
- A value from a variable that fails to decode is never echoed, whichever
  carrier held it. The message is rebuilt from the key and the expected form
  (`invalid value, expected a boolean for key ...`); config-rs's own
  diagnostic would quote the value, which is a secret when a variable omits
  its last name segment. An unknown or missing key is reported as written,
  and a rejected file value is still shown, except in a section the loader
  lists as value-free (`VALUE_FREE_SECTIONS` in `load.rs`), whose trust
  inputs stay out of diagnostics as they stay out of `Debug`.
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
- `postgres.password_file` (unset by default; a blank value is unset) is
  the one alternative password source: a path to a file that holds the
  password alone, for a platform that rotates it. The URL then carries no
  password, both at once is refused, and the service and the jobs worker
  follow the file while they run, which a secrets-directory value, read
  once at startup, does not. The key names a path, not a credential, so the file guard lets
  it appear in TOML: `password` followed by `file` is the one exception to
  the `password` rule above.
- The `migrate` binary reads the same files and variables but decodes only
  `app`, `log`, `observability`, and `postgres`. It needs no other section's
  secrets, and it does not check the other sections or an unknown section
  name; the binaries that use them do.
<!-- template:end postgres:docs-config-postgres-source -->
<!-- template:begin messaging:docs-config-messaging-source -->
- `messaging` is an optional typed section. Its non-secret endpoint, stream,
  consumer, DLQ, TLS, timeout, concurrency, and delivery-size inputs use normal
  file/environment precedence, and a blank `root_ca_path` is unset; credentials are `SecretString`, environment-only,
  and redacted. `messaging.credentials_file` (unset by default; a blank
  value is unset) is the one alternative credential source: a path to a
  NATS credentials file, for a platform that rotates it. The client reads
  the file again for every connection, which a secrets-directory value,
  read once at startup, cannot do. Both at once is refused. The key names a
  path, not a credential, so it may appear in TOML. Active consumption requires complete named topology and distinct
  source/DLQ subjects. Local plaintext or unauthenticated use is an explicit
  development/test escape hatch, never a production default. Configuration
  validates shape and resource bounds before any provider I/O; the adapter maps
  the admitted snapshot to its client options.
  `messaging.max_payload_bytes` defaults to `256 KiB`, the Go template's
  default. A broker bounds payload and headers together, by default at
  1 MiB, so the payload limit plus the 8 KiB header limit must fit the
  broker's `max_payload`; startup refuses a larger one.
  `messaging.urls` uses a list or one comma-separated string, such as an
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
  file value is refused because `dsn` is secret-like. `cache.password_file`
  (unset by default) is the one alternative password source: a path to a
  file that holds the password alone, for a platform that rotates it. The
  DSN then carries no password, both at once is refused, and the client
  follows the file while it runs. The key names a path, not a credential,
  so it may appear in TOML. `cache.client_cert_path` and
  `cache.client_key_path` (unset by default) name the PEM certificate chain
  and private key the client presents to a server that requires a client
  certificate; they are set together, and one without the other fails
  startup. Both are paths, so they may appear in TOML; the key itself stays
  in the file. `password_file`, `root_ca_path`, `client_cert_path`,
  `client_key_path`, and `command_timeout` use normal file/environment
  precedence; a blank path is unset. `allow_plaintext`
  and `allow_unauthenticated` are accepted only when `app.env` is `local` or
  `development`. Admitted schemes are `redis`, `rediss`, `valkey`, and
  `valkeys`. `#insecure` and a unix socket are refused; Sentinel and Cluster
  URLs are not admitted. A CA path or a client certificate requires TLS. DSN form checks stay in `infra-cache`. The
  [guide](cache.md) owns admission.
<!-- template:end cache:docs-config-cache-source -->
<!-- template:begin object-storage:docs-config-object-storage-source -->
- `object_storage` is an optional typed section, inert while `provider` is
  `none`. `object_storage.secret_access_key` is `SecretString`,
  environment-only (`APP__OBJECT_STORAGE__SECRET_ACCESS_KEY`), and redacted;
  a nonempty file value is refused because `secret` is secret-like.
  `provider`, `bucket`, `region`, `endpoint`, `expected_bucket_owner`,
  `path_style`, `credentials`, and `access_key_id` use normal
  file/environment precedence. Each provider accepts only its own keys:
  `amazon_s3` takes `region` and `expected_bucket_owner` and no endpoint,
  `cloudflare_r2` and `railway` take `endpoint`, `s3_compatible` takes
  `endpoint` and alone takes `path_style`, and a key another provider owns
  fails startup instead of being ignored. `local` (an emulator, plaintext
  allowed) is accepted only when `app.env` is `local` or `development`.
  `credentials` is `access_key` (default) or, for `amazon_s3` only,
  `workload_identity`, which refuses a nonempty `access_key_id` or
  `secret_access_key`. The S3 client is built from these keys alone: the
  global `AWS_*` variables and AWS profile files never change its endpoint,
  region, or behavior. Under `workload_identity` the AWS SDK's own providers
  read what the platform injects for the workload's role
  (`AWS_WEB_IDENTITY_TOKEN_FILE` and `AWS_ROLE_ARN`, the
  `AWS_CONTAINER_*` variables) and the fixed instance metadata endpoint,
  only to obtain credentials; environment access keys are still not a
  source, and no profile file is read. On Railway, map the bucket's `${{Bucket.X}}`
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
- `authn.token_profile` is a public selector and the one exception to the secret-like `token` rule.
- JWT mode accepts `authn.token_profile = "resource-server"` or `"rfc9068"`, with `resource-server` as the omitted-value default; it accepts a nonempty `authn.algorithms` list of `RS256`, `ES256`, `PS256`, or `EdDSA`. `authn.audience` accepts one string or a nonempty exact-string list. The optional, nonsecret `authn.jwks_uri` (`APP__AUTHN__JWKS_URI`) names an HTTPS key set endpoint and replaces discovery; a blank value is rejected and the adapter validates its URL grammar. JWT provider configuration does not accept introspection fields.
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
- `integrations.<name>.oauth` is an immutable optional OAuth2 client tuple
  authenticated only by a private key, never a shared secret. Empty
  integration maps and entries without `oauth` are inert; a present tuple
  must contain valid `token_url`, `client_id`, `key_id`, `algorithm` (`RS256`,
  `PS256`, or `ES256`; required, with no default), `assertion_audience`, and
  an environment-only nonempty
  `APP__INTEGRATIONS__<NAME>__OAUTH__PRIVATE_KEY`. `client_secret` is
  an unknown key and fails startup. Nonsecret
  `scopes`, optional `audience` (not blank when set), and `exchange_cache_capacity` (a whole
  number, default 1024, inclusive 1–65536) follow normal TOML/environment
  layering; scopes use a TOML list or one space-separated environment value. File secrets
  are refused by the recursive secret guard. The [outbound machine-authentication
  guide](outbound-machine-authentication.md) owns endpoint admission and
  provider compatibility.
<!-- template:end outbound-auth:docs-config-outbound-auth -->

## Secrets Directory

`--secrets-dir PATH` reads `APP__` variables from files, for a platform that
mounts secrets instead of exporting them: a Kubernetes Secret volume, Docker
or Compose secrets, systemd credentials (`--secrets-dir
"$CREDENTIALS_DIRECTORY"`). It is a second carrier of the same variables, not
a second set of keys.

- Each directory entry whose name starts with `APP__` is one variable, and
  the file's content is its value. A file named
  `APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_HEADERS` sets the collector
  credential, exactly as the environment variable of that name does. Every other entry is
  skipped, which leaves out the `..data` links a Kubernetes volume keeps
  beside its files; subdirectories are not searched.
- A value ends before its trailing line breaks, because most tools write
  one. Every other byte is kept, so a multi-line PEM key is stored whole.
- The process environment wins over a file of the same name.
- The directory is read once, at startup, and symbolic links are followed. A
  rotated file takes effect at the next restart, like every other key of the
  immutable snapshot.
- A directory that cannot be listed, an entry that cannot be read, is not
  valid Unicode, or holds a NUL byte, and a malformed name fail startup. The message names the
  path and never the content.
- Any key may be supplied this way; the rule that keeps secrets out of TOML
  files is unchanged.

On Kubernetes, name the Secret's keys as the variables, or map a provider's
key names with `items`, and mount the volume read-only:

```yaml
containers:
  - name: service
    args: ["--config", "/etc/service/config.toml", "--secrets-dir", "/run/secrets/app"]
    volumeMounts:
      - { name: app-secrets, mountPath: /run/secrets/app, readOnly: true }
volumes:
  - name: app-secrets
    projected:
      sources:
        - secret: { name: service }
        - secret:
            name: collector-token
            items:
              - { key: headers, path: APP__OBSERVABILITY__OTEL__EXPORTER__OTLP_HEADERS }
```

## OpenTelemetry Environment Policy

<!-- template:begin grpc:docs-config-grpc -->
The optional `grpc` section defaults disabled. Enabling it requires an address
and explicit plaintext or TLS security; bearer verification remains valid with
either mode. PEM certificate/CA values are ordinary configuration, while
`grpc.private_key` and `integrations.<name>.grpc.private_key` are environment-only
secrets. In a [secrets directory](#secrets-directory), a certificate
manager's `tls.crt` and `tls.key` map onto `APP__GRPC__CERTIFICATE` and
`APP__GRPC__PRIVATE_KEY` with the volume's `items`. The transport builds the
listener config at startup. A disabled
listener performs no TLS or network work. Config Debug omits all trust and
identity material. Client integration inputs select a trusted destination,
explicit security, optional CA and optional paired certificate/key; they do
not create a client registry or token owner. See [gRPC](grpc.md).

The listener's runtime budgets are non-secret and follow normal file and
environment precedence; they are checked only while `grpc.enabled` is true.
`grpc.request_timeout` (default `8s`, `100ms` to `10m`) caps a business
call's time to response headers, authentication included, and must fit inside the effective HTTP drain
budget, which both listeners share. `grpc.max_in_flight` (default `256`,
zero disables shedding) bounds business calls running at once.
`grpc.max_connections` (default `4096`, zero is unbounded) bounds accepted
connections; many calls share one HTTP/2 connection, so it is independent of
`max_in_flight`. `grpc.max_connection_age` (default `30m`, `0s` off,
otherwise `1s` to `1d`) sends GOAWAY to a connection that reached it, spread
by up to 10% either way.
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
  store. `OTEL_EXPORTER_OTLP_CERTIFICATE` names a PEM file of trusted
  certificates instead: with it only those certificates are trusted, which
  is how a collector behind a private certificate authority is reached.
  `OTEL_EXPORTER_OTLP_CLIENT_CERTIFICATE` and `OTEL_EXPORTER_OTLP_CLIENT_KEY`
  name the PEM files of a client certificate and its key for a collector
  that requires one; one without the other is refused. Each variable has a
  `..._TRACES_...` variant that wins, and a blank value is vacant. They
  apply under a typed endpoint too: they are paths, read once at startup,
  and carry no credential to the collector. A file that is missing or not
  usable PEM leaves the exporter `degraded` with a reason that names the
  variable. The `trace exporter initialized` record carries
  `certificate_file` and `client_certificate`.
- `OTEL_EXPORTER_OTLP_COMPRESSION` and `OTEL_EXPORTER_OTLP_TRACES_COMPRESSION`
  select `gzip`; the default is uncompressed. Any other value fails the
  exporter build and leaves the exporter `degraded`.
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
signal, not continuous delivery health. Delivery is
`otel_sdk_exporter_span_exported_total`: the spans of every finished export,
with `error_type` (`timeout`, `already_shutdown`, or `internal_failure`) on a
failed batch. Spans the batch queue dropped before export appear only in the
SDK's own log records.

`observability.metrics.addr` owns the Prometheus diagnostics listener. It
defaults to `:9090`, which binds IPv4 all-interfaces (`0.0.0.0`) so a scraper in another pod
can reach it; an empty value disables HTTP exposition. Binding failure blocks
startup. Deployment network policy must keep this listener private. The
service also answers `GET /health/live` on it, outside the application
listener's connection cap; an empty value leaves liveness on the application
listener only.

## Logging

`log.level` is a `tracing_subscriber::EnvFilter` directive (`info`,
`debug`, `info,hyper=warn`). `RUST_LOG` is not read; `APP__LOG__LEVEL` is the
override channel like every other key. `log.format` is `json` (production:
one flattened object per line with `trace_id`, `span_id`, and `trace_flags` on
every record inside a request) or `text` (local development).

`log.level` chooses records, not traces. Spans at INFO and above (the HTTP
and gRPC server spans, job attempts, client calls) exist under every
directive, so `warn` or `off` keeps trace export and keeps the request id and
trace context on the records that remain; the sampler is what turns tracing
down. A span more verbose than INFO follows the directive.

A JSON line holds each key once. The line's own keys are `level`, `target`,
`timestamp`, `trace_id`, `span_id`, and `trace_flags`; an event or span field
with one of those names is left out. A key an event shares with a span in its
scope takes the event's value, and a nested span's value over its parent's. A
record bridged from the `log` crate carries its real target and no `log.*`
fields.

A panic is an ERROR record, `panicked`, with `panic.file`, `panic.line`,
`panic.column`, `panic.thread`, `panic.message`, and `panic.backtrace` when
`RUST_BACKTRACE` asks for one. Every binary replaces Rust's panic hook with
it once the subscriber is installed, so a panic is one parseable line in the
log stream and not plain text on stderr; like any ERROR record it follows
`log.level`. The jobs worker with messaging
retained leaves `panic.message` out: a handler may format a message's content
into its panic.

## Runtime Budget Policy

- `http.request_timeout` (default `8s`) is the per-request handler budget and
  the only bound on how long one request may hold a task and its pooled
  resources. Body reads happen inside it because extractors run inside the
  handler future. Expiry answers a `504` problem with code `gateway_timeout`.
  It must not exceed the drain budget left after readiness propagation, so
  in-flight requests can finish inside the drain.
- `http.header_read_timeout` (default `5s`) bounds delivery of a request head
  and, because hyper restarts it whenever an HTTP/1 connection goes idle, is
  also the HTTP/1 keep-alive idle bound. HTTP/2 idle uses a separate PING
  cadence. It also closes a client that connects and sends nothing, or only
  the start of the HTTP/2 preface.
- `health.probe_budget` (default `4s`) bounds one background readiness
  evaluation; every probe runs under it at the same time, and
  `/health/ready` itself never runs a probe. The verdict names the first
  probe that failed or ran out of the budget.
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
  disables shedding. The health probe routes are admitted without a permit,
  so a saturated instance is not restarted as dead. `http.max_connections` (default `4096`) bounds accepted
  connections; the excess is closed at accept without a response. It must be
  at least `max_in_flight` so the informative rejection stays the common one.
- `http.max_connection_age` (default `0s`, off) tells a connection that
  reached it to finish and close, as at drain, so its client reconnects and
  is balanced again; each connection's age is spread by up to 10% either
  way. A set value must be between `1s` and `1d`. It stays off by default
  because an HTTP/1 proxy that reuses an idle connection just as the server
  closes it sees a failed request; set it when HTTP/2 clients hold
  connections behind a connection-level balancer. The diagnostics listener
  shares the HTTP listener's options, this one included.
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
  bound and the one database capacity value an operator sets.
  `postgres.migration_deadline` (environment
  `APP__POSTGRES__MIGRATION_DEADLINE`, default `5m`, `1s..24h`) bounds one
  `migrate` run and each `-- no-transaction` migration in it; the service
  and the worker decode it and do not use it. The acquire
  budget (`3s`), the session `statement_timeout` and
  `idle_in_transaction_session_timeout` (`8s`), and the other migration
  budgets are constants in the adapter and the runner
  ([Persistence](architecture/persistence.md#budgets)); the readiness probe
  draws `health.probe_budget`, and the pool closes inside the `5s`
  dependency-close stage. Enabled service and worker startup also bound the
  read-only embedded migration-history check to `5s`, including pool acquire.
  This is a separate sequential startup step, with no new configuration key.
  `postgres.session_budgets` (`startup` by default, or `server`) does not
  change a budget: it says whether the service publishes the two session
  timeouts in each connection's startup packet or the database role already
  carries them, for a pooler that refuses startup parameters. Either way the
  pool refuses to open on a session without them.
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
  included (a put or delete makes one); one read attempt gets half of it, so
  a hung attempt leaves room for a retry; connect stays a `3.1s` constant and a
  download body is bounded by the SDK's stalled-stream protection (5 s without
  progress) instead. On a request path the handler
  budget still applies: a put dropped by `http.request_timeout` has an unknown
  outcome. `object_storage.max_concurrency` (default `8`, `1..512`) admits
  that many calls at once and refuses the excess without queueing; a download
  holds its slot until its body ends. `object_storage.max_object_bytes`
  (default `8 MiB`, at most 4.995 GiB, the smallest single-upload limit of the
  supported providers) bounds a put and a get. The payload collected by
  downloads still holding a slot is budgeted as
  `max_concurrency * max_object_bytes`; collection copies, SDK buffers, and
  allocation overhead add to it. Completed `Bytes` outlive their slots, so
  this is not a process memory ceiling. The consuming HTTP/job path owns
  concurrency and payload budgets for those retained responses. See the
  [object storage guide](object-storage.md).
<!-- template:end object-storage:docs-config-object-storage-budget -->

## Adding A Config Key

1. Add the typed field, its default in the section's `impl Default`, and its
   validation, all in the section's own file under `crates/config/src/`. One
   section is one file: the reason a value was chosen sits beside the rule
   that enforces it. An optional text or path key decodes a blank value as
   unset with `de::blank_as_none`, and an optional secret with
   `de::blank_secret_as_none`. A new multi-word enum value is spelled
   `snake_case`, like the keys; `authn.mode` and `authn.token_profile` keep
   the hyphenated spellings they shipped with, and identifiers a standard
   defines (`RS256`, `parentbased_traceidratio`) keep the standard's.
2. Add a loader test in `crates/config/src/load.rs` that sets the key through
   the environment and asserts the decoded value, and a validation test for a
   rejected value. For a secret, also add its dotted key to the vectors in
   `crates/config/src/secret_policy.rs` so the file guard is proven to
   recognize the name.
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
the corresponding `APP__...` variables and never a TOML file. The recursive
secret-file guard covers endpoint fields and dynamic maps. There is no
environment-variable indirection, JSON-in-environment manifest, or remote secret
provider.

Endpoint IDs and key references follow the file-key rule above: key
references name `APP__INBOUND_WEBHOOKS__SECRETS__<REF>` variables.
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

`webhooks.max_concurrent_deliveries` (environment
`APP__WEBHOOKS__MAX_CONCURRENT_DELIVERIES`, unset by default, at least `1`) is
the most deliveries one jobs worker process runs at once. Unset, deliveries
may take every `jobs.max_workers` slot; a value below `jobs.max_workers`
keeps the difference for the worker's other job kinds while a receiver
answers slowly. Only the worker uses it.
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
| `--secrets-dir`: one directory whose files are named as `APP__` variables, merged under the process environment | a `*_FILE` twin per variable; a path key beside every secret key; the environment as the only carrier | platforms mount secrets as files (Kubernetes Secret volumes, Docker secrets, systemd credentials), the CIS Kubernetes Benchmark (5.4.1) prefers that to environment variables, and a multi-line PEM key is awkward in a variable. A directory reuses the one variable namespace: no key gains a twin, validation and the decode-failure redaction cover both carriers, and a secret still cannot sit in TOML. `*_FILE` would collide with a real key whose name ends in `_file` and need a registry of declared keys to tell them apart; a path key per secret doubles every secret key. The shape is pydantic-settings' `secrets_dir`; config-rs has no such source and no crate on crates.io adds one (searched 2026-10-02), so the loader lists the directory itself, about thirty lines. Reopen for a key that must follow rotation without a restart: that is a decision for that key, with its own reader |
| TOML baseline files | YAML | the Rust convention with a maintained crate; `serde_yaml` is archived, `serde_yml` carries RUSTSEC-2025-0068; config-rs's `yaml` feature stays available for a service that must consume YAML |
| Secrets as `secrecy::SecretString`; `APP__` variables are the only secret source; each TOML file is pre-scanned for non-empty secret-like keys (`password`, `secret`, `credentials`, `token`, `dsn`, `authorization`, `api_key`, `private_key`, `otlp_headers`) | trusting file contents | a committed baseline cannot leak a credential; `Debug` prints `[REDACTED]` |
| `tracing` + `tracing-subscriber` (`EnvFilter` parses `log.level`; a directive without span filters runs as the equivalent `Targets`); the telemetry crate's JSON layer for `log.format = json`, `fmt::layer()` for `text`; `log` records bridged | `json-subscriber` 0.3 (chosen in stage 2) | it wrote the same line but built a JSON value map for the event and another for the span list on every record, and re-serialized all of a span's fields on every `record`: 61% of a small request's instructions. The crate's layer wrote the identical line (a differential corpus of 64 records matched byte for byte) with two thirds fewer instructions per record ([Telemetry performance](infra-telemetry-performance.md)). It has since left that line in three places where the line was the defect: a key an event shared with a span was written twice, which a strict JSON consumer rejects; a `log` crate record had the target `log`; and the trace context was nested as `openTelemetry.traceId` and `spanId`, where OpenTelemetry names it `trace_id`, `span_id`, and `trace_flags` for a non-OTLP log format. `EnvFilter` takes a shared lock on every span enter, exit, and close even without span directives. Reopen if an upstream layer flattens span fields without per-record maps |
| Tracer provider always installed; the OTLP HTTP/protobuf batch exporter added only when a typed endpoint or a standard `OTEL_EXPORTER_OTLP_*ENDPOINT` resolves one; `TraceContextPropagator` installed explicitly | exporter `disabled` when no endpoint, provider absent | trace ids in every log line cost nothing without an exporter and avoid connection-refused noise against the SDK's `localhost:4318` default |
| Ambient `OTEL_EXPORTER_OTLP_*HEADERS` fail validation when the typed endpoint selects the destination; the standard certificate and client-certificate variables build the exporter's HTTP client, which the SDK's OTLP/HTTP exporter does not do | letting the SDK merge them | one collector's credential is never sent to another; the mechanism stays the SDK's, the safety property is a validation rule |
| The `metrics` facade with `metrics-exporter-prometheus` (`default-features = false`), the HTTP adapter's own server metrics under the OpenTelemetry HTTP semantic-convention names (route template or `<unmatched>`), `metrics-process`, `tokio-metrics` | OpenTelemetry SDK metrics with `opentelemetry-prometheus` and OTLP push | the facade is the dominant Rust idiom, process and Tokio metrics have no OTel-native crates, and `opentelemetry-prometheus` was deprecated, un-deprecated, and is still Beta. A collector `prometheus` receiver scraping `:9090` serves OTLP-only platforms |
| `log.level` filters records; spans at INFO and above are enabled under every directive by one global filter around `Targets` or `EnvFilter` | the directive as the only filter; a per-layer filter on the format layer | the server, job, and client spans are INFO, so `log.level = warn` stopped trace export and stripped the request id and trace context from the remaining records. A per-layer filter hides a filtered span from the format layer, which loses the same fields, and costs bookkeeping on every span ([Telemetry performance](infra-telemetry-performance.md)) |
| Every histogram has buckets: the emitter's own, or the Prometheus client default for one nobody registered | an unregistered histogram rendered as a summary | a summary's quantiles cannot be aggregated across replicas, and a forgotten registration was silent |
| The OTLP exporter is wrapped to count finished exports as `otel_sdk_exporter_span_exported_total` (the SDK's semantic-convention name) | startup gauge only; SDK log records | export failures after startup were visible only as log lines. The SDK has no hook for spans its batch queue drops, so those stay in its log records |
| `opentelemetry-otlp/gzip-http` enabled, compression off by default | feature off | with the feature off, the standard `OTEL_EXPORTER_OTLP_COMPRESSION=gzip` fails the exporter build and tracing degrades. Costs `flate2` with its pure-Rust backends in the graph |
| `OTEL_EXPORTER_OTLP_*CERTIFICATE`, `*CLIENT_CERTIFICATE`, and `*CLIENT_KEY` build the exporter's `reqwest` blocking client (the client and TLS stack `opentelemetry-otlp` builds itself, with the SDK's timeout); none set leaves the SDK's own client | a startup warning naming the unread variables; typed `observability.otel.exporter` keys | the specification lists the three among the exporter's options, and the HTTP exporter of `opentelemetry-otlp` 0.33 reads none of them, so a collector behind a private certificate authority or one requiring a client certificate could not be reached. The standard variables are what a platform already sets; typed keys would be a second name for the same paths. A certificate file replaces the platform trust store, as the Go and Java SDKs do. Reopen when `opentelemetry-otlp` reads them itself |
| One panic hook for every binary, installed after the subscriber: a panic is an ERROR record with its place, thread, and message; the worker with messaging retained withholds the message | Rust's hook in the service and the migrator, a hook only in the messaging worker; the `tracing-panic` crate | Rust's hook writes plain text to stderr beside JSON records, and gRPC panic recovery relies on the hook for the message. `tracing-panic` is the same twenty lines without the choice to withhold the message |
| `deny.toml` refuses a second version of `opentelemetry` and `opentelemetry_sdk`; an exporter test delivers a span to a listening collector | a documented `cargo tree` check | a second version's `global` provider is a silent no-op, and no test had a span arrive anywhere, so a broken exporter client or feature set passed every check |
| `log.format` added; `runtime.memory_limit_ratio`, `GOMAXPROCS` awareness, and `observability.pprof` not ported | Go parity | human-readable local logs are a Rust convention; there is no garbage collector, `available_parallelism` honours cgroup quotas, and there is no standard-library profiler to expose |

Version discipline: every OpenTelemetry crate stays on one minor and moves
together, with `tracing-opentelemetry` one ahead (0.34 ↔ 0.33). A dependency
that pins another minor creates a second `global::` whose data goes to a
no-op provider silently; `deny.toml` refuses a second version of
`opentelemetry` or `opentelemetry_sdk`, so `make deny` fails on it. A dependency enabling `opentelemetry-otlp/reqwest-client`
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
