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
request_timeout = "8s"      # cap on a call's time to response headers, authentication included
max_in_flight = 256         # active business calls, open streams included; 0 never sheds
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
    _background: &mut service::BackgroundRegistration<'_>,
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

The third argument is a borrowed, non-cloneable `BackgroundRegistration`.
A feature with process-owned work calls `spawn(name, |stop, failure| async move
{ ... })` during registration. `name` is static, `stop` is a child of the
service's cancellation token, and the future returns `Result<(), E>`; the
service never formats `E`. Registration and future construction must return
promptly. The handle exposes no task set, root token, runtime or deadline.

Register one manager for the feature's work. On cancellation it closes admission
and joins every admitted operation before returning `Ok(())`. On live failure
it closes admission, calls `failure.report()` immediately, then keeps joining
before returning `Err`. The reporter latches only the registered name into the
existing root failure watch; repeated reports are harmless and reporting does
not complete the manager. The process starts its failure transition immediately
and retains that manager in the same background owner through cleanup. Returning
`Err` after requested cancellation still votes for degraded shutdown. An early
`Ok` or a panic also fails the required task. The existing root deadline owns
all waiting; a timeout cannot certify that a started blocking closure stopped.

Tasks registered before a later registration failure or unwind remain in the
same cleanup owner. This is an intentional change from the former two-argument
Rust callback; there is one registration path. The generated contract and wire
routes are unchanged, and the default `service::run` registers no feature work.
See [process-owned work](architecture/runtime-lifecycle.md#integrating-process-owned-work)
for lifecycle and exit-code meaning.

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
`grpc.reflection.v1` and the `grpc.reflection.v1alpha` it replaced, both from
`tonic-reflection`, as grpc-go registers both: older tools ask only for
`v1alpha`. Both are served as business routes: reflection
needs the same bearer as any application method and is limited and
deadline-bound with them. It publishes the whole schema, comments included,
to every authenticated caller, so add it only in the environments where
operators need it. Without it, `buf curl --schema api/proto` calls the service
from a checkout.

## Middleware

Outermost to innermost:

1. Observation. It follows a call until its status is known; see
   [Health, shutdown and observation](#health-shutdown-and-observation).
2. Panic recovery, inside the observation layer. The response is `INTERNAL` / `request failed`. The payload
   goes to the process panic hook, which records it as an ERROR log record,
   as on HTTP.
   A response stream that panics after its headers ends with the same
   status in its trailers, instead of a reset stream.
3. Business routes only: an opening budget of
   `min(grpc-timeout, grpc.request_timeout)` through response headers. A valid
   caller `grpc-timeout` also bounds the response DATA and trailers from the
   same admission time. Expiry is `DEADLINE_EXCEEDED` /
   `request deadline exceeded`, with catalog reason `GATEWAY_TIMEOUT`. A malformed
   `grpc-timeout`, including more than eight digits, counts as absent and
   `grpc.request_timeout` applies. The deadline is outside authentication,
   so the time a verification takes is spent from the caller's budget, as
   the HTTP request timer covers its authentication.
4. Business routes only: bearer authentication, when that profile is
   retained. Health is outside it. Missing, malformed and invalid bearers are
   `UNAUTHENTICATED` / `authentication failed`. Provider unavailability is
   `UNAVAILABLE` / `authentication is unavailable`. A verified principal
   without a method's declared scopes is `PERMISSION_DENIED`. Each
   authentication outcome is counted
   in `authn_verifications_total{transport="grpc"}` by the same
   `Verifier::authenticate` the HTTP boundary uses.
5. Business routes only: the `grpc.max_in_flight` concurrency limit, 256
   unless configured, and none at zero. A shed call is
   `RESOURCE_EXHAUSTED` / `server is at capacity` and increments
   `grpc_server_shed_requests_total`. Health is outside this limit. It is
   innermost, so a call that failed authentication never holds a permit. Each
   admitted call holds one permit through terminal status, failure, deadline
   or cancellation. Open server-streaming and bidi calls consume capacity too;
   size the limit for those live streams as well as short calls. Headers alone
   do not release capacity. The listener limits are not measured application
   capacity.

An authentication failure or a shed call is answered after reading the rest
of its request body, for at most 100 ms and 64 KiB. A caller sends request
DATA after the headers; answering first would make h2 reset each stream when
that DATA arrives, and after 1024 such resets hyper closes the connection
with every other call on it.

## Deadlines

`grpc.request_timeout`, eight seconds unless configured, caps opening:
authentication, handler work and response headers. A valid incoming
`grpc-timeout` supplies a separate whole-call budget that starts at the same
boundary. The earlier bound applies before headers; only the caller budget
continues after them. Without caller metadata, an opened server stream has no
transport lifetime or idle cap. Headers never prove that unary DATA has been
decoded or terminal trailers received. Health is exempt from both business
budgets and capacity.

Expiry drops the transport-owned future or stream and releases its permit and
observation even if the peer is flow-controlled and the response is not being
polled. The response owns one deadline/cancellation waiter; it adds no message queue
or read-ahead. Once a terminal status is observed before expiry it stays
final. An expired call emits deadline trailers when the transport can read
them, but a disconnected or unread peer cannot be promised status delivery.
Hyper may still hold one DATA frame awaiting send capacity.

The response retains the timer handle and reaps completion when polled.
Dropping the response clears application resources synchronously and requests
timer abortion; that request is not an awaited join. The timer has only a weak
reference to the call, so it cannot retain its stream or permit. Synchronous
feature polling and destructors must cooperate with the runtime.

Admitted tonic handlers receive `operation_context::OperationContext` in request
extensions for opening work, and `infra_grpc::ResponseContext` for response work.
The response carrier retains only the original caller deadline, if any; its
`operation()` therefore remains usable after the local opening cap. Propagate
these contexts explicitly to dependencies. Successful headers transfer the call
cancellation guard; EOF, error, drop, deadline and parent cancellation release
body, capacity and upload custody, even when the body is unpolled.

Feature-spawned tasks remain the feature's responsibility: instrument
them explicitly and stop producers when their receiver closes. Cancellation
neither aborts detached tasks nor rolls back effects. Features own aggregate
message and idle policies; there is no global idle timer.

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
  it keeps its connection until it ends. Connection age is neither a stream
  lifetime nor an application idle timeout, and does not discover endpoints.
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
  instead of tonic's 8 KiB. Larger messages grow the buffer. The 32 KiB
  streaming encode batch threshold remains unchanged; neither value is a
  message-size cap. Framing, encoding, decoding and errors remain stock prost/tonic.

## Feature-owned message and stream budgets

Generated clients and servers expose supported per-instance size setters. For
a business service, choose `request_limit_bytes` and `response_limit_bytes`
from its contract, then apply both directions before registration or use:

```rust,ignore
let server = EchoServiceServer::new(Echo)
    .max_decoding_message_size(request_limit_bytes)
    .max_encoding_message_size(response_limit_bytes);
services.add(server)?;

let channel = infra_grpc::Client::new(destination, security, timeout)?;
let mut client = EchoServiceClient::new(channel)
    .max_encoding_message_size(request_limit_bytes)
    .max_decoding_message_size(response_limit_bytes);
let mut request = tonic::Request::new(UnaryRequest { message: "hello".into() });
request.set_timeout(call_budget);
let response = client.unary(request).await?;
```

The names above stand for feature-chosen byte ceilings and a finite caller
budget, not new template defaults. A server receives requests and sends responses;
a client does the reverse. These setters bound each encoded protobuf message,
not the sum of a stream or the heap used by its decoded fields. Limit decoded
collection sizes and application fan-out where those values enter feature work.
The encoder limit also cannot prevent allocations used to construct an outgoing
message before encoding. Compression remains disabled in both directions; no
decompression policy is added. Unconfigured instances retain 4 MiB decoding and
unlimited default encoding (`usize::MAX`).

For streaming methods, combine these limits with a finite aggregate message
count/byte budget and bounded producer queues. Consume messages as they arrive
instead of collecting an unbounded stream. The existing server admission holds
one permit through terminal status/drop, including open business streams; a
feature may need a smaller stream allowance and separate outbound fan-out bound.
Keep any feature admission guard with the stream and its retained resources,
not just the future that opens it, and stop owned producers on cancellation.

Use a finite caller deadline for the whole call and the feature's own idle or
lifetime policy where required. `grpc.request_timeout` alone caps opening;
without a caller deadline an opened server stream has no transport lifetime
cap. The [client timeout policy](#reuse-clients-and-original-deadlines) describes
the default full-RPC budget and the explicit opening-only alternative. A per-read
timeout only runs while that read is polled; it does not reclaim a reader left
unpolled by its owner.

Drop a completed `tonic::Streaming` reader promptly: even after terminal status
releases transport admission, that reader may still retain its grown decoder
buffer. Decoded messages already returned to callers live independently too.
Wire ceilings, codec capacity, aggregate stream budgets and result lifetimes
must therefore be accounted for separately; none of these numbers is a hard
process-memory/RSS guarantee.

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
| Opening or caller lifetime deadline elapsed | `DEADLINE_EXCEEDED` | `GATEWAY_TIMEOUT` |
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
`Client::new(destination, security, timeout)` performs no DNS or socket I/O.
`ClientSecurity::Plaintext` or `ClientSecurity::Tls(ClientTlsMaterial)` is
explicit, and the destination scheme must agree: `http` for plaintext,
`https` for TLS. Tonic applies TLS only to `https`, so a mismatch is
`Error::DestinationSecurityMismatch` rather than a silent plaintext
connection. TLS uses tonic `ClientTlsConfig`: normal certificate and hostname
verification, native roots unless a CA is supplied, and an optional
`ClientIdentity` whose key is a `SecretString`. Unusable PEM input fails
construction with the variant that names it. The server remains TLS 1.3-only; the client does not. Construction
takes the client's timeout and sets a 5 second connect timeout, a 60 second TCP keepalive, HTTP/2
keepalive at 60 seconds with a 20 second timeout, and adaptive receive
windows. The keepalive PING is sent only while a call is open, and no more
often than gRPC's keepalive guide asks of clients. A grpc-go, grpc-java or
C-core server that keeps its default five-minute ping allowance can still
answer a stream that stays silent for minutes with `GOAWAY too_many_pings`;
such a server sets its `PermitWithoutStream`/`MinTime` policy for long quiet
streams. Clones share the lazy channel and its metric handles.

`Client::new(destination, security, timeout)` selects
`ClientTimeout::FullRpc(timeout)`: one finite budget from adapter entry through
channel readiness, queueing, headers, DATA and terminal trailers, for every
cardinality. The local policy sends no `grpc-timeout` of its own. A propagated
`OperationContext` in request extensions also bounds the call. The OAuth wrapper
prepares the concrete client's call before credentials, so acquisition spends
this same allowance.

For intentionally long-lived streams, opt in explicitly:

```rust,ignore
let channel = infra_grpc::Client::with_timeout_policy(
    destination,
    security,
    infra_grpc::ClientTimeout::OpeningOnly(std::time::Duration::from_secs(10)),
)?;
```

`OpeningOnly` bounds credentials, readiness, queueing and headers, but has no local
lifetime cap after opening. A valid supplied `Request::set_timeout` bounds the
whole RPC under either policy, as does a propagated parent context. The earliest
supplied deadline wins; a longer one
cannot extend the local FullRpc budget or either policy's opening cap. Zero
expires immediately; malformed metadata keeps the absent-value behavior.

`Client::prepare_call(request)` returns an opaque `PreparedCall` bound to that
client and its fixed cutoffs. It exposes `opening_context()` and `headers_mut()`;
`send()` consumes the same value. The ordinary Tower call uses this path too.
Preparation performs no I/O. Dropping preparation cancels only its child scope.

At handoff to tonic, the adapter writes the remaining caller/parent deadline
as `grpc-timeout` after readiness waiting. Tonic's opaque queue
and subsequent network transit can consume more time after this header is
fixed; the peer does not receive an identical absolute expiry. The independent
local deadline still includes those intervals. Expiry yields
`DEADLINE_EXCEEDED` / `request deadline exceeded`, including while waiting for
a message or trailers. A previously observed terminal peer status remains
final.

Expiry before handoff prevents submission. After handoff, cancellation closes
the response receiver so Tower discards buffered work when it observes that
closure. Concurrent admission/dispatch can win that race: the remote effect
is then unknown. Expiry or response drop also clears the locally owned upload
source even if HTTP/2 would keep its send half open. Already queued bytes
cannot be recalled. None of these outcomes promises rollback or safe replay;
no retry is added.

```rust,ignore
let timeout = std::time::Duration::from_secs(10);
let channel = infra_grpc::Client::new(destination, security, timeout)?;
let mut client = EchoServiceClient::new(channel);
let mut request = tonic::Request::new(UnaryRequest { message: "hello".into() });
request.set_timeout(std::time::Duration::from_secs(2));
let response = client.unary(request).await?;
```

The client injects the current trace context and records its span and metrics
when the call's status arrives. Any other transport failure is `UNAVAILABLE` /
`transport unavailable` to the caller, because a handler may forward that
status; the cause is logged as `grpc_client_transport_failed` inside the
client span. There is no application retry, replay, hedging,
discovery or client health polling. A `Client` is one HTTP/2 connection to
whatever address its destination resolved to when it connected: it does not
watch DNS or balance RPCs across addresses. This is appropriate for a service
or mesh destination; GOAWAY lets a later connection be assigned by its
balancer. A headless DNS name supplies no endpoint watcher or per-RPC
balancing, and connection age does not add either. A concrete dependency
requiring direct multi-endpoint discovery reopens the integration design. gRPC messages are not compressed
in either direction: tonic's compression features are off, so a peer that
sends a compressed message gets `UNIMPLEMENTED`.

<!-- template:begin outbound-auth-grpc:docs-grpc-oauth -->
When OAuth is also selected, bind the channel inside the private credential
owner before giving it to the generated client:

```rust,ignore
let channel = infra_grpc::Client::new(destination, security, timeout)?;
let authenticated = credentials.grpc(channel);
let client = EchoServiceClient::new(authenticated);
```

A caller-supplied `Authorization` is `INTERNAL` before any token or
resource I/O. To call on behalf of a verified user instead of as the service
itself, attach `OnBehalfOf::new(principal.access_token().clone())` to the
call's extensions through `tonic::Request::extensions_mut` before dispatch;
the client then sends an exchanged token addressed to this integration
instead of the service token. The client is prepared before cached or fetched
credentials are handled. Acquisition uses its opening context, including the
selected local policy, propagated parent, and valid caller deadline. The
owner's five-second fetch ceiling may stop acquisition sooner; it never extends
the resource allowance. The prepared client forwards the remaining supplied
budget before dispatch. Acquisition
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
`infra_grpc::Failure` is not counted there. Catalog failures in response
trailers and recovered stream panics are counted too.
Started minus handled is the number of calls in progress, open streams
included. `grpc_code` is the grpc-go code name, one of all 17: `OK`, `Canceled`,
`InvalidArgument`, `FailedPrecondition` and so on. The histograms measure the
whole call, to its status, with the Prometheus default buckets that
`go-grpc-middleware` uses (`HANDLING_SECONDS_BUCKETS`, registered by the
bootstrap); a stream that stays open longer than ten seconds, a health
`Watch` above all, lands in the `+Inf` bucket when it ends. Metric handles
are kept per method after the first call.

A call is handled when its status is known, on both sides. A rejection, a
deadline and a unary failure carry `grpc-status` in the response headers and
are handled there. Every other answer is followed through its response body
to the trailers, so a stream that fails after its first message is counted
with that failure, not as `OK`, and the span ends with the call and carries
its real status. Frames pass through without an added queue or read-ahead.
Finite lifetimes use one weak response-owned timer to release the raw body,
upload source, permit and observation even when body polling stops. The span
and incoming fallback context are entered only during each synchronous lazy
response poll, so logs and nested RPCs created there correlate with the call.
The context is restored between polls; feature-spawned tasks still need
explicit instrumentation, for example `tracing::Instrument::in_current_span`.

On the server, `grpc_service` and `grpc_method` come from the request path
only when it is a described method of a registered service or of health.
Every outcome of such a call carries its method: an answer, an
authentication failure, a shed, a deadline, a recovered panic. Any other
path is `"unknown"` in the labels and in the span name, so a caller-chosen
path cannot create a series. A call its caller abandons before its status,
by resetting the stream or closing the connection, is handled as
`Canceled`, as grpc-go counts it; so is a client call whose caller stops
waiting or drops a response stream it has not read to the end. An answer
that ends with no `grpc-status` at all is `Unknown`, as tonic's client reports
it. Payloads, metadata
values, bearer tokens and raw errors are not transport attributes.

Clients derive metric and span method identity from tonic's native `GrpcMethod`
extension only when its service and method exactly match the two URI segments.
A raw URI alone uses `unknown/unknown`; routing still uses the original URI.
The extension is public, so one process-wide registry admits at most 256
distinct pairs plus unknown, with at most 256 bytes per component. Oversized
or mismatched candidates and new pairs after saturation use unknown. Existing
admitted identities remain; there is no eviction or reset when clients are
reconstructed. This bounds cached handles and recorder labels together. An
adopter needing more than 256 observed generated methods must revisit that
telemetry policy. Handles bind to the recorder installed before serving starts.

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
