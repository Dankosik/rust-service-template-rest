# Native gRPC

`GRPC=enabled` keeps native unary, client-streaming, server-streaming and bidi
RPCs on a separate listener. That listener is `infra_http::Server`, not a
second transport. Selection alone starts no listener, client connection,
provider request or background task. `GRPC=none` is the initializer default
and removes the transport, contracts, schema, generator, configuration,
example and their exclusive dependencies.

The [decision record](grpc-decisions.md) records why those boundaries exist.
HTTP keeps its OpenAPI and RFC 9457 contract. Neither transport owns feature
business rules.

## Select and enable

Select the profile when initializing a clean template checkout:

```sh
make template-init SERVICE_NAME=catalog-api \
  REPOSITORY=https://github.com/example/catalog-api \
  DESCRIPTION='Catalog API' CODEOWNER=@example/platform \
  DATABASE=none AUTHN=none GRPC=enabled
```

Enable a listener with an address and an explicit security mode:

```toml
[grpc]
enabled = true
addr = "0.0.0.0:50051"
security = "plaintext"
```

Plaintext is an operator trust decision, including platform TLS termination
or mesh mTLS. Retaining bearer authentication does not require in-process
TLS. With an authentication profile retained, configure its real verifier
before enabling gRPC. Runtime `authn.mode = "none"` cannot expose the
listener. The standard health service, `Check` and `Watch`, is public and
ignores supplied credentials, as load balancers and Kubernetes gRPC probes
expect. Every application method requires the opening bearer. Selecting
`AUTHN=none` removes that requirement.

Process TLS uses the same accept loop. `security = "tls"` builds a rustls
`ServerConfig` in `infra_grpc` and passes it to
`infra_http::Server::bind_tls`. The handshake has its own five-second bound.
Supply the PEM certificate chain in `grpc.certificate` and
`APP__GRPC__PRIVATE_KEY` through the environment. The private key is rejected
in configuration files. `grpc.client_ca` makes verified client certificates
mandatory. The server admits TLS 1.3 only, with `h2` ALPN. Invalid material
refuses startup with an error that names the certificate, private key or CA,
never its value. The key is borrowed from its secret wrapper, not copied. Certificates reload on process restart. Configuration Debug
omits certificate, CA and key material.

Four more keys bound the listener, each with the default its HTTP
counterpart has:

```toml
[grpc]
request_timeout = "8s"      # cap on a call's time to response headers
max_in_flight = 256         # business calls at once; 0 never sheds
max_connections = 4096      # accepted connections; 0 is unbounded
max_connection_age = "30m"  # GOAWAY after this age; "0s" sets no age
```

`grpc.request_timeout` must fit inside the effective HTTP drain budget
(`http.drain_timeout` minus `http.readiness_propagation_delay`), because both
listeners drain under it. A longer value refuses startup and says so.
Disabled gRPC does not bind, read TLS material, check these limits or require
a verifier. [Configuration source policy](configuration-source-policy.md)
owns precedence and secret custody.

## Register a service

Own schemas under `api/proto/<package>/v1`. Use versioned packages and reserve
removed field numbers and names. `api/proto/example/v1/echo.proto` is an
isolated transport example with all four cardinalities. The default service
does not register it.

A schema may import protobuf's well-known types. They generate as
`prost_types` paths, so add `prost-types` to the workspace table and to
`grpc-contracts` with the first such import. Any other import needs its module
in `buf.yaml` `deps` and a committed `buf.lock`; its package is then generated
beside the owned ones, unless `tools/grpc-codegen` maps it with `extern_path`
to a crate that already owns those types.

Implement the generated tonic server trait. Describe the contract, register
the generated server once and run it through the service bootstrap:

```rust,ignore
fn register(
    services: &mut infra_grpc::Services,
    _state: &service::AppState,
) -> Result<(), infra_grpc::Error> {
    services.describe(grpc_contracts::FILE_DESCRIPTOR_SET)?;
    services.add(EchoServiceServer::new(Echo))
}

fn main() -> std::process::ExitCode {
    service::run_with_grpc(std::env::args_os(), register)
}
```

See the complete [example](../crates/service/examples/grpc.rs). Bootstrap
calls the registration once, after it opened the dependencies, with the
`AppState` the HTTP routes also receive. A feature adds its state to
`AppState` as one more field; its generated server takes that field when it
is constructed, as its HTTP handlers take it through `State`.

`Services::describe` reads the services and methods of a descriptor set. A
generated server does not list its methods, so the set is how the transport
knows them: for scope requirements, for metric labels and for reflection.
`Services::add` records `NamedService::NAME` for health and adds the server
to tonic `Routes`. A server whose service no described set holds fails
startup with `Error::UndescribedService`, a duplicate name with
`Error::DuplicateService`. A server from another crate needs that crate's
descriptor set described first; tonic crates export one as
`FILE_DESCRIPTOR_SET`. Do not build another listener or per-handler
middleware stack. Handler `Status` values pass through unchanged.

With authentication retained, a successful verify inserts
`infra_bearerauthn::Principal` into the request extensions and removes
`Authorization`. Declare a method's required scopes after adding its service,
by request path:

```rust,ignore
services.add(EchoServiceServer::new(Echo))?;
services.require_scopes("/example.v1.EchoService/Unary", &["echo.read"])?;
```

Every listed scope is required. A principal that lacks one gets
`PERMISSION_DENIED` / `the verified principal lacks the required scope`
before the handler. A method with no declared requirement admits any
authenticated caller, as an OpenAPI operation without scopes does. A path
that is not a described method of a registered service, such as a misspelled
method name, or a second requirement for one method, fails startup.

## Reflection

Server reflection is off unless the registration adds it:

```rust,ignore
services.add_reflection()?;
```

`grpcurl`, `grpcui`, Postman and `buf curl` can then list and call the
registered services, and health, without the schema files. It serves every
described set, whether described before or after this call. It is
`grpc.reflection.v1` from `tonic-reflection`, served as a business route: it
needs the same bearer as any application method and is limited and
deadline-bound with them. It publishes the whole schema, comments included,
to every authenticated caller, so add it only in the environments where
operators need it. Without it, `buf curl --schema api/proto` calls the service
from a checkout.

## Middleware

Outermost to innermost:

1. Observation.
2. Panic recovery, inside the observation layer. The response is `INTERNAL` / `request failed`. The payload
   goes to the process panic hook, which records it as an ERROR log record,
   as on HTTP.
3. Business routes only: bearer authentication, when that profile is
   retained. Health is outside it. Missing, malformed and invalid bearers are
   `UNAUTHENTICATED` / `authentication failed`. Provider unavailability is
   `UNAVAILABLE` / `authentication is unavailable`. A verified principal
   without a method's declared scopes is `PERMISSION_DENIED`. Each
   authentication outcome is counted
   in `authn_verifications_total{transport="grpc"}` by the same
   `Verifier::authenticate` the HTTP boundary uses.
4. Business routes only: the `grpc.max_in_flight` concurrency limit, 256
   unless configured, and none at zero. A shed call is
   `RESOURCE_EXHAUSTED` / `server is at capacity` and increments
   `grpc_server_shed_requests_total`. Health is outside this limit. A permit
   is held until response headers, so it bounds unary and client-streaming
   calls; server-streaming and bidi streams that are already open are bounded
   by the connection cap and the HTTP/2 stream limit instead.
5. Business routes only: deadline `min(grpc-timeout, grpc.request_timeout)`,
   measured until the handler returns response headers. Expiry is
   `DEADLINE_EXCEEDED` / `request deadline exceeded`. A malformed
   `grpc-timeout`, including more than eight digits, counts as absent and
   `grpc.request_timeout` applies.

An authentication failure or a shed call is answered after reading the rest
of its request body, for at most 100 ms and 64 KiB. A caller sends request
DATA after the headers; answering first would make h2 reset each stream when
that DATA arrives, and after 1024 such resets hyper closes the connection
with every other call on it.

## Deadlines

`grpc.request_timeout`, eight seconds unless configured, is tonic
`Server::timeout` placement with a `DEADLINE_EXCEEDED` status. It is not a
body-lifetime timer.

- Unary: the whole call, because the handler returns the response.
- Client-streaming: the upload must finish and the handler must return within
  the budget.
- Server-streaming and bidi: the handler must return its stream within the
  budget. After response headers, the stream is bounded by the caller and by
  process drain, not by this timer.

Health is outside the deadline.

## Listener bounds

Listener options, shared with HTTP except for the values below:

- `grpc.max_connections` connections, 4096 unless configured. Excess
  connections are closed without a response.
- `grpc.max_connection_age`, thirty minutes unless configured. A connection
  that reaches it gets HTTP/2 GOAWAY: its open calls and streams finish, and
  the client opens a new connection for the next call. A gRPC channel
  otherwise keeps one connection for the life of its process, so behind a
  connection-level balancer, such as a Kubernetes `Service` without a mesh,
  replicas added later would get no calls from existing clients. Each
  connection's age is spread by up to 10% either way, as grpc-go spreads
  `MaxConnectionAge`, so connections opened together do not all reconnect
  together. There is no forced close after the age: a stream that outlives
  it keeps its connection until it ends.
- 5 seconds for the TLS handshake, then 5 seconds for the HTTP/2 preface.
  A handshake error or either timeout closes the connection without a
  response.
- 16 KiB of request metadata.
- HTTP/2 PING keepalive every 20 seconds, with a 20 second timeout.
- `TCP_NODELAY`, so response headers, data, and trailers do not wait for the
  peer's delayed ACK, and HTTP/2 adaptive receive windows sized to the
  bandwidth-delay product, as grpc-go does. Both apply to the HTTP listener too.
- Hyper's default concurrent-stream limit, 200 in locked hyper 1.11.1. This
  listener does not set its own, and hyper does not treat that number as stable.
- Tonic's default 4 MiB decode limit on business RPCs and health. The
  transport sets no encode cap.
- 2 KiB initial codec buffers per call, from `grpc_contracts::codec`,
  instead of tonic's 8 KiB. Larger messages grow the buffer.

## Handler validation

The transport does not validate protobuf fields. Handlers reject their own
input. The example refuses an empty or oversized message before any effect:

```rust,ignore
fn accepted(message: String) -> Result<String, Status> {
    if (1..=1024).contains(&message.len()) {
        Ok(message)
    } else {
        Err(Failure::new(Code::BadRequest)
            .field_violation("message", "must be 1 to 1024 bytes")
            .into())
    }
}
```

Its client-streaming method also rejects an aggregate over 1024 bytes with
`Code::RequestEntityTooLarge`. Features own any further aggregate, idle or
cancellation limit. The transport does not cancel work a handler has spawned.

## Failures

Return a failure from the shared catalog with `infra_grpc::Failure`:
`Err(Failure::new(Code::NotFound).into())`. The catalog code fixes the status
code and a safe message. `google.rpc.ErrorInfo` carries the code a client
matches on: `reason` is the catalog code in upper case, such as `NOT_FOUND`
for the HTTP problem code `not_found`, and `domain` is
`infra_grpc::ERROR_DOMAIN`, the service name the initializer writes.
`Failure::field_violation` adds a `google.rpc.BadRequest` violation, as
`invalid_params` does in an HTTP problem; it names the field and the failed
constraint, never the submitted value. `Failure::retry_after` adds
`google.rpc.RetryInfo` and does not enable retries.

| Catalog code | gRPC code |
| --- | --- |
| `bad_request`, `unsupported_media_type`, `unprocessable_content`, `idempotency_key_mismatch`, `webhook_rejected` | `INVALID_ARGUMENT` |
| `unauthorized`, `authentication_required`, `authentication_malformed`, `authentication_invalid` | `UNAUTHENTICATED` |
| `forbidden` | `PERMISSION_DENIED` |
| `not_found` | `NOT_FOUND` |
| `already_exists` | `ALREADY_EXISTS` |
| `conflict`, `idempotency_request_in_progress` | `ABORTED` |
| `method_not_allowed` | `UNIMPLEMENTED` |
| `request_entity_too_large`, `too_many_requests` | `RESOURCE_EXHAUSTED` |
| `service_unavailable`, `authentication_unavailable`, `idempotency_unavailable` | `UNAVAILABLE` |
| `gateway_timeout` | `DEADLINE_EXCEEDED` |
| `internal_error` | `INTERNAL` |

The transport answers its own rejections from the same catalog, so each
carries a reason:

| Condition | gRPC code | Reason |
| --- | --- | --- |
| Missing bearer | `UNAUTHENTICATED` | `AUTHENTICATION_REQUIRED` |
| Malformed bearer | `UNAUTHENTICATED` | `AUTHENTICATION_MALFORMED` |
| Invalid bearer | `UNAUTHENTICATED` | `AUTHENTICATION_INVALID` |
| Authentication provider unavailable | `UNAVAILABLE` | `AUTHENTICATION_UNAVAILABLE` |
| Missing required scope | `PERMISSION_DENIED` | `FORBIDDEN` |
| Concurrency shed | `RESOURCE_EXHAUSTED` | `SERVICE_UNAVAILABLE` |
| Header deadline elapsed | `DEADLINE_EXCEEDED` | `GATEWAY_TIMEOUT` |
| Recovered panic | `INTERNAL` | `INTERNAL_ERROR` |

The shed is the one answer whose gRPC code differs from its catalog row: it
keeps the identity HTTP's shed uses, and `RESOURCE_EXHAUSTED` keeps a client
that retries `UNAVAILABLE` from adding load. A malformed bearer is HTTP 400
and gRPC `UNAUTHENTICATED`, because gRPC has no bad-request status for
credentials.

A handler `Status` is not rewritten, and a status built without `Failure`
carries no reason. Tonic's own decode-limit status is unchanged.

## Reuse clients and original deadlines

Create one lazy `infra_grpc::Client` per trusted operator destination.
`Client::new(destination, ClientSecurity)` performs no DNS or socket I/O.
`ClientSecurity::Plaintext` or `ClientSecurity::Tls(ClientTlsMaterial)` is
explicit, and the destination scheme must agree: `http` for plaintext,
`https` for TLS. Tonic applies TLS only to `https`, so a mismatch is
`Error::DestinationSecurityMismatch` rather than a silent plaintext
connection. TLS uses tonic `ClientTlsConfig`: normal certificate and hostname
verification, native roots unless a CA is supplied, and an optional
`ClientIdentity` whose key is a `SecretString`. Unusable PEM input fails
construction with the variant that names it. The server remains TLS 1.3-only; the client does not. Construction
sets a 5 second connect timeout, a 60 second TCP keepalive, HTTP/2
keepalive at 60 seconds with a 20 second timeout, and adaptive receive
windows. The keepalive PING is sent only while a call is open, and no more
often than gRPC's keepalive guide asks of clients. A grpc-go, grpc-java or
C-core server that keeps its default five-minute ping allowance can still
answer a stream that stays silent for minutes with `GOAWAY too_many_pings`;
such a server sets its `PermitWithoutStream`/`MinTime` policy for long quiet
streams. Clones share the lazy channel and its metric handles.

Set the call budget with tonic `Request::set_timeout`. That writes
`grpc-timeout`, and tonic's channel ends the call when it runs out. The
caller then gets `DEADLINE_EXCEEDED` / `request deadline exceeded`, never
`UNAVAILABLE`: the server may still be running the call, so a caller that
retries `UNAVAILABLE` must not repeat it. The server still applies
`min(grpc-timeout, grpc.request_timeout)`.

```rust,ignore
let channel = infra_grpc::Client::new(destination, security)?;
let mut client = EchoServiceClient::new(channel);
let mut request = tonic::Request::new(UnaryRequest { message: "hello".into() });
request.set_timeout(std::time::Duration::from_secs(2));
let response = client.unary(request).await?;
```

The client injects the current trace context and records its span and metrics
from response headers. Any other transport failure is `UNAVAILABLE` /
`transport unavailable` to the caller, because a handler may forward that
status; the cause is logged as `grpc_client_transport_failed` inside the
client span. There is no application retry, replay, hedging,
discovery or client health polling.

<!-- template:begin outbound-auth-grpc:docs-grpc-oauth -->
When OAuth is also selected, bind the channel inside the private credential
owner before giving it to the generated client:

```rust,ignore
let channel = infra_grpc::Client::new(destination, security)?;
let authenticated = credentials.grpc(channel);
let client = EchoServiceClient::new(authenticated);
```

A caller-supplied `Authorization` is `INTERNAL` before any token or
resource I/O. To call on behalf of a verified user instead of as the service
itself, attach `OnBehalfOf::new(principal.access_token().clone())` to the
call's extensions through `tonic::Request::extensions_mut` before dispatch;
the client then sends an exchanged token addressed to this integration
instead of the service token. The acquisition deadline is `grpc-timeout` when that header is
present and well formed; otherwise it is the owner's five-second fetch
timeout. Token wait spends that deadline: a wait of at least a millisecond
rewrites `grpc-timeout` to the remaining budget before dispatch. Acquisition
failure prevents dispatch: `UNAVAILABLE` / `client credentials unavailable`
when the provider could not be reached or answered 5xx or 429, and
`UNAUTHENTICATED` / `client credentials refused` when it refused the request
or answered unusably, as gRPC clients report credentials that produced no
call metadata. The message is fixed because a handler may forward the
status; the closed `AcquisitionError`, with the provider's registered error
code, is the status source. A handler should translate this status rather
than forward it: `UNAUTHENTICATED` here means the service's own credentials
were refused, not its caller's. `credentials.grpc(channel).require_on_behalf_of()`
binds a client that answers `INTERNAL` to a call without `OnBehalfOf`
instead of sending the service token. Both refusals are `INTERNAL` because
they are this service's composition mistakes, and gRFC A54 keeps
`INVALID_ARGUMENT` for the application. One bearer is inserted at
opening and is not refreshed mid-stream.

Eviction runs only on the initial response: `grpc-status` `UNAUTHENTICATED`,
or HTTP 401 with no `grpc-status`. Trailers are not inspected. The response
is returned unchanged. Conditional eviction of that exact token, once it is
thirty seconds old, is an in-memory
update and starts no background work. A newer cached replacement survives.
`PERMISSION_DENIED` keeps the credential. The
[OAuth owner](outbound-machine-authentication.md) keeps the token, cache and
reuse policy.
<!-- template:end outbound-auth-grpc:docs-grpc-oauth -->

## Health, shutdown and observation

`Check` reads the cached readiness verdict. It does not probe dependencies.
The empty service name means the overall service. A name registered with
`Services::add`, plus `grpc.health.v1.Health`, is known. Any other `Check`
name is `NOT_FOUND`. A known service is `SERVING` only while the verdict is
ready, and `NOT_SERVING` otherwise, including before the first successful
admission. Health is a readiness answer and shares the gRPC listener's
connection cap; for a platform liveness probe use `GET /health/live` on the
diagnostics listener
([Runtime Lifecycle](architecture/runtime-lifecycle.md#readiness-and-liveness)).

`Watch` streams changes and does not spawn a task. A known service emits the
current status, then each change. When readiness is draining it emits
`NOT_SERVING` if that was not already the last status, then ends, so the
stream does not hold drain. An unknown `Watch` emits `SERVICE_UNKNOWN` once
and ends when readiness is draining, without a later `NOT_SERVING`.

Shutdown starts readiness drain first, so health becomes `NOT_SERVING` during
the propagation delay. HTTP and gRPC then drain concurrently, each with the
same remaining drain budget. The budget must cover `grpc.request_timeout`.
In-flight calls may finish; health watchers do not hold the drain. An overrun
votes in the existing degraded shutdown. There is no second budget and no
separate gRPC cleanup stage.

The server span, like the HTTP one, carries only `otel.name` and `otel.kind`
as `tracing` fields, so the JSON log layer does not serialize and repeat the
RPC attributes on every record; `rpc.system`, `rpc.service`, `rpc.method`,
`rpc.grpc.status_code`, `server.address`, `server.port` and
`user_agent.original` go to the OpenTelemetry span alone, as does
`failure.code` when the answer is a catalog failure. The parent is the
extracted incoming context, kept current even when the span is disabled.
Client spans are built the same way. Metrics follow the grpc-ecosystem Prometheus
names, so standard gRPC dashboards and alerts apply:
`grpc_server_started_total` and `grpc_client_started_total`
(`grpc_service`, `grpc_method`), `grpc_server_handled_total` and
`grpc_client_handled_total` (`grpc_service`, `grpc_method`, `grpc_code`), the
histograms `grpc_server_handling_seconds` and `grpc_client_handling_seconds`
(`grpc_service`, `grpc_method`), and `grpc_server_shed_requests_total`.
`grpc_server_failures_total` (`grpc_service`, `grpc_method`, `failure_code`)
is the template's own addition: it counts the calls answered with a failure
from the shared catalog, under the catalog code as HTTP's access log spells
it in `problem_code`, so the failures that share one `grpc_code`
(`authentication_unavailable` and `service_unavailable` are both
`Unavailable`) stay apart. A status a handler builds without
`infra_grpc::Failure` is not counted there.
Started minus handled is the number of calls waiting for response headers.
`grpc_code` is the grpc-go code name, one of all 17: `OK`, `Canceled`,
`InvalidArgument`, `FailedPrecondition` and so on. The histograms measure time
to response headers, with the Prometheus default buckets that
`go-grpc-middleware` uses (`HANDLING_SECONDS_BUCKETS`, registered by the
bootstrap). Metric handles are kept per method after the first call.

On the server, `grpc_service` and `grpc_method` come from the request path
only when it is a described method of a registered service or of health.
Every outcome of such a call carries its method: an answer, an
authentication failure, a shed, a deadline, a recovered panic. Any other
path is `"unknown"` in the labels and in the span name, so a caller-chosen
path cannot create a series. A call its caller abandons before the response
headers, by resetting the stream or closing the connection, is handled as
`Canceled`, as grpc-go counts it; so is a client call whose caller stops
waiting. Spans and metrics use the response-header status. A missing `grpc-status` header is recorded as ok, so
a streaming error sent only in trailers is not reflected. Payloads, metadata
values, bearer tokens and raw errors are not transport attributes.

## Generate and verify

`make grpc-generate` asks pinned Buf 1.73.0 for a file descriptor set of the
owned files and their imports; prost needs an imported message's descriptor to
generate a field of that type. Stock `tonic_prost_build` generates the set,
and its `include_file` nests each package in its module path: `example.v1` is
`grpc_contracts::example::v1`. The set itself is committed beside the Rust as
`file_descriptor_set.binpb` and exported as
`grpc_contracts::FILE_DESCRIPTOR_SET` for reflection. The run replaces the
generated directory whole, so a removed package leaves no file. Generation
does not invoke protoc. Commit schema, generated Rust and descriptor set
together. Never edit generated files.
Application contracts are not generated by normal service builds or by the
runtime image.

`make grpc-check` runs Buf format and STANDARD lint, generates once and
compares the result with committed Rust, and checks FILE compatibility with
`buf breaking --against .git#ref=<base commit>`. The base is `GRPC_BASE_REF`
(the pull-request base in CI). An initial addition reports that the base has
no protobuf contract; an unreadable base fails. The generator is not a runtime
workspace member. CI owns that heavy check.
