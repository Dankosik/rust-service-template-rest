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
`infra_http::Server::bind_tls`. The handshake runs after the first-byte peek.
Supply the PEM certificate chain in `grpc.certificate` and
`APP__GRPC__PRIVATE_KEY` through the environment. The private key is rejected
in configuration files. `grpc.client_ca` makes verified client certificates
mandatory. The server admits TLS 1.3 only, with `h2` ALPN. Invalid material
refuses startup with an error that names the certificate, private key or CA,
never its value. The key is borrowed from its secret wrapper, not copied. Certificates reload on process restart. Configuration Debug
omits certificate, CA and key material.

The effective HTTP drain budget must be at least eight seconds
(`infra_grpc::CALL_DEADLINE_CAP`) when gRPC is enabled. A shorter budget
refuses startup and says so. Disabled gRPC does not bind, read
TLS material or require a verifier. [Configuration source
policy](configuration-source-policy.md) owns precedence and secret custody.

## Register a service

Own schemas under `api/proto/<package>/v1`. Use versioned packages and reserve
removed field numbers and names. `api/proto/example/v1/echo.proto` is an
isolated transport example with all four cardinalities. The default service
does not register it.

Implement the generated tonic server trait. Register the generated server
once and run it through the service bootstrap:

```rust,ignore
fn register(services: &mut infra_grpc::Services) -> Result<(), infra_grpc::Error> {
    services.add(EchoServiceServer::new(Echo))
}

fn main() -> std::process::ExitCode {
    service::run_with_grpc(std::env::args_os(), register)
}
```

See the complete [example](../crates/service/examples/grpc.rs). `Services::add`
records `NamedService::NAME` for health and adds the server to tonic
`Routes`. A duplicate name fails startup with `Error::DuplicateService`. Do not build another listener or
per-handler middleware stack. Handler `Status` values pass through unchanged.

With authentication retained, a successful verify inserts
`infra_bearerauthn::Principal` into the request extensions and removes
`Authorization`.

## Middleware

Outermost to innermost:

1. Observation.
2. Panic recovery. The response is `INTERNAL` / `request failed`. The payload
   goes to the normal panic hook, as on HTTP. There is no suppressing hook.
3. Business routes only: bearer authentication, when that profile is
   retained. Health is outside it. Missing, malformed and invalid bearers are
   `UNAUTHENTICATED` / `authentication failed`. Provider unavailability is
   `UNAVAILABLE` / `authentication is unavailable`. Each outcome is counted
   in `authn_verifications_total{transport="grpc"}` by the same
   `Verifier::authenticate` the HTTP boundary uses.
4. Business routes only: a concurrency limit of 256. A shed call is
   `RESOURCE_EXHAUSTED` / `server is at capacity` and increments
   `grpc_server_shed_requests_total`. Health is outside this limit. A permit
   is held until response headers, so it bounds unary and client-streaming
   calls; server-streaming and bidi streams that are already open are bounded
   by the connection cap and the HTTP/2 stream limit instead.
5. Business routes only: deadline `min(grpc-timeout, 8s)`, measured until the
   handler returns response headers. Expiry is `DEADLINE_EXCEEDED` /
   `request deadline exceeded`. A malformed `grpc-timeout`, including more
   than eight digits, counts as absent and the eight-second cap applies.

## Deadlines

The eight-second cap (`CALL_DEADLINE_CAP`) is tonic `Server::timeout` placement with a
`DEADLINE_EXCEEDED` status. It is not a body-lifetime timer.

- Unary: the whole call, because the handler returns the response.
- Client-streaming: the upload must finish and the handler must return within
  the budget.
- Server-streaming and bidi: the handler must return its stream within the
  budget. After response headers, the stream is bounded by the caller and by
  process drain, not by this timer.

Health is outside the deadline.

## Listener bounds

Fixed listener options, shared with HTTP except for the values below:

- 4096 connections. Excess connections are closed without a response.
- 5 seconds to the first byte, then a separate 5 second TLS handshake bound.
  A handshake error or timeout closes the connection without a response.
- 16 KiB of request metadata.
- HTTP/2 PING keepalive every 20 seconds, with a 20 second timeout.
- Hyper's default concurrent-stream limit, 200 in locked hyper 1.11.1. This
  listener does not set its own, and hyper does not treat that number as stable.
- Tonic's default 4 MiB decode limit on business RPCs and health. The
  transport sets no encode cap.

## Handler validation

The transport does not validate protobuf fields. Handlers reject their own
input. The example refuses an empty or oversized message before any effect:

```rust,ignore
fn accepted(message: String) -> Result<String, Status> {
    if (1..=1024).contains(&message.len()) {
        Ok(message)
    } else {
        Err(classified_status(ClassifiedFailure::new(Code::BadRequest)))
    }
}
```

Its client-streaming method also rejects an aggregate over 1024 bytes with
`Code::RequestEntityTooLarge`. Features own any further aggregate, idle or
cancellation limit. The transport does not cancel work a handler has spawned.

## Failures

Use `infra_grpc::classified_status` for a shared domain failure.
`google.rpc.ErrorInfo` carries the stable code in the fixed `service` domain.
An optional policy-owned retry delay becomes `google.rpc.RetryInfo` and does
not enable retries. The message is a fixed safe string. HTTP projects the
same shared identity into its existing status, title, URI and payload.

| Condition | gRPC code |
| --- | --- |
| Classified bad request | `INVALID_ARGUMENT` |
| Missing, malformed or invalid bearer; classified unauthenticated | `UNAUTHENTICATED` |
| Classified forbidden | `PERMISSION_DENIED` |
| Classified not found | `NOT_FOUND` |
| Classified already exists | `ALREADY_EXISTS` |
| Classified conflict | `ABORTED` |
| Classified unimplemented | `UNIMPLEMENTED` |
| Concurrency shed; classified resource limits | `RESOURCE_EXHAUSTED` |
| Authentication provider unavailable; classified unavailable | `UNAVAILABLE` |
| Header deadline elapsed | `DEADLINE_EXCEEDED` |
| Recovered panic | `INTERNAL` |

A handler `Status` is not rewritten. Only the rows above are transport-owned
or catalog-owned. Tonic's own decode-limit status is unchanged.

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
sets a 5 second connect timeout, a 60 second TCP keepalive, and HTTP/2
keepalive at 20 seconds with a 20 second timeout. Clones share the lazy
channel.

Set the call budget with tonic `Request::set_timeout`. That writes
`grpc-timeout`. The server still applies `min(grpc-timeout, 8s)`.

```rust,ignore
let channel = infra_grpc::Client::new(destination, security)?;
let mut client = EchoServiceClient::new(channel);
let mut request = tonic::Request::new(UnaryRequest { message: "hello".into() });
request.set_timeout(std::time::Duration::from_secs(2));
let response = client.unary(request).await?;
```

The client injects the current trace context and records its span and metrics
from response headers. A transport failure is `UNAVAILABLE` /
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

A caller-supplied `Authorization` is `INVALID_ARGUMENT` before any token or
resource I/O. The acquisition deadline is `grpc-timeout` when that header is
present and well formed; otherwise it is the owner's five-second fetch
timeout. Acquisition failure prevents dispatch. One bearer is inserted at
opening and is not refreshed mid-stream.

Eviction runs only on the initial response: `grpc-status` `UNAUTHENTICATED`,
or HTTP 401 with no `grpc-status`. Trailers are not inspected. The response
is returned unchanged. Conditional invalidation of that exact credential
spends only the remaining deadline and starts no background work. A newer
cached replacement survives. `PERMISSION_DENIED` keeps the credential. Hard
expiry before dispatch is a timeout and sends no resource request. The
[OAuth owner](outbound-machine-authentication.md) keeps the token, cache and
coalescing policy.
<!-- template:end outbound-auth-grpc:docs-grpc-oauth -->

## Health, shutdown and observation

`Check` reads the cached readiness verdict. It does not probe dependencies.
The empty service name means the overall service. A name registered with
`Services::add`, plus `grpc.health.v1.Health`, is known. Any other `Check`
name is `NOT_FOUND`. A known service is `SERVING` only while the verdict is
ready, and `NOT_SERVING` otherwise, including before the first successful
admission.

`Watch` streams changes and does not spawn a task. A known service emits the
current status, then each change. When readiness is draining it emits
`NOT_SERVING` if that was not already the last status, then ends, so the
stream does not hold drain. An unknown `Watch` emits `SERVICE_UNKNOWN` once
and ends when readiness is draining, without a later `NOT_SERVING`.

Shutdown starts readiness drain first, so health becomes `NOT_SERVING` during
the propagation delay. HTTP and gRPC then drain concurrently, each with the
same remaining drain budget. The budget must be at least eight seconds.
In-flight calls may finish; health watchers do not hold the drain. An overrun
votes in the existing degraded shutdown. There is no second budget and no
separate gRPC cleanup stage.

Server spans come from
`tracing_opentelemetry_instrumentation_sdk` gRPC helpers. The parent is the
extracted incoming context. Metrics follow the grpc-ecosystem Prometheus
names, so standard gRPC dashboards and alerts apply:
`grpc_server_handled_total` and `grpc_client_handled_total`
(`grpc_service`, `grpc_method`, `grpc_code`), the histograms
`grpc_server_handling_seconds` and `grpc_client_handling_seconds`
(`grpc_service`, `grpc_method`), and `grpc_server_shed_requests_total`.
`grpc_code` is the grpc-go code name, one of all 17: `OK`, `Canceled`,
`InvalidArgument`, `FailedPrecondition` and so on. The histograms measure time
to response headers.

On the server, `grpc_service` and `grpc_method` come from the request path
only when the call was dispatched to a registered service or to health and the
header status is not `UNIMPLEMENTED`. Otherwise both are `"unknown"`, so a
caller-chosen path cannot create a series. Spans and metrics use the
response-header status. A missing `grpc-status` header is recorded as ok, so
a streaming error sent only in trailers is not reflected. Payloads, metadata
values, bearer tokens and raw errors are not transport attributes.

## Generate and verify

`make grpc-generate` asks pinned Buf 1.73.0 for a temporary file descriptor
set, then runs stock `tonic_prost_build` over the owned files. Generation
does not invoke protoc and does not commit a descriptor set. Commit schema
and generated Rust together. Never edit generated Rust. Application contracts
are not generated by normal service builds or by the runtime image.

`make grpc-check` runs Buf format and STANDARD lint, repeats generation,
compares committed Rust, and checks FILE compatibility against
`GRPC_BASE_REF` (the pull-request base in CI). An initial addition reports
that the base has no protobuf contract; an unreadable base fails. The
generator is not a runtime workspace member. CI owns that heavy check.
