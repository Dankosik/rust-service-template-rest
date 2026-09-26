# Native gRPC

`GRPC=enabled` retains native unary, client-streaming, server-streaming and bidi
RPCs on a separate HTTP/2 listener. Selection alone starts no listener, client
connection, provider request or background task. `GRPC=none` is the initializer
default and removes the transport, contracts, schema, generation tools,
configuration, example and their exclusive dependencies.

The [decision record](grpc-decisions.md) explains the library and policy seams.
HTTP keeps its existing OpenAPI and RFC 9457 contract. Neither transport owns
feature business rules.

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

Plaintext is an operator trust decision, including platform TLS termination or
mesh mTLS. Retaining bearer authentication does not require in-process TLS.
With an authentication profile retained, configure its real verifier before
enabling gRPC. Runtime `authn.mode = "none"` cannot expose protected RPCs.
Only exact `grpc.health.v1.Health/Check` is public and ignores supplied
credentials. Watch, application methods and other utilities require the
opening bearer identity. Selecting `AUTHN=none` removes that requirement.

For process TLS, select `security = "tls"`, supply the PEM certificate chain in
`grpc.certificate`, and provide `APP__GRPC__PRIVATE_KEY` through the environment.
The private key is rejected in configuration files. `grpc.client_ca` makes
verified client certificates mandatory. The listener admits TLS 1.3 only;
invalid identities or trust roots refuse startup. Certificates reload on
process restart. Configuration Debug omits certificate, CA and key material.

The existing effective HTTP drain budget must be at least eight seconds when
gRPC is enabled. Disabled gRPC does not bind, read TLS material or require a
verifier. [Configuration source policy](configuration-source-policy.md) owns
precedence and secret custody.

## Implement and register a service

Own schemas under `api/proto/<package>/v1`, use versioned packages, and reserve
removed field numbers and names. `api/proto/example/v1/echo.proto` is an isolated
transport example with all four cardinalities; it is not registered by the
default service and does not define a business API.

Implement the generated native tonic server trait. Its streaming requests stay
`tonic::Streaming<T>` and responses stay native `tonic::Response<T>` or streams
of `Result<T, tonic::Status>`. Register through the generated helper once:

```rust,ignore
fn register(services: &mut infra_grpc::Services) -> Result<(), infra_grpc::Error> {
    grpc_contracts::generated::register_echo_service(services, Echo)
}

fn main() -> std::process::ExitCode {
    service::run_with_grpc(std::env::args_os(), register)
}
```

See the complete [example](../crates/service/examples/grpc.rs). It uses the
shipped service bootstrap, readiness and shutdown. Registration checks exact
descriptor identity, method cardinality and validation programs before serving;
duplicate or unusable registrations fail startup. Do not build another tonic
server, raw registration path or per-handler middleware stack.

The central policy rejects process/capacity admission before authentication or
feature work, removes raw Authorization, and exposes only the sealed verified
principal. It validates every decoded request message before the feature sees
it, including messages received later in a stream. A swallowed validation error
still terminates the call; earlier valid effects are not rolled back.
Protovalidate rule errors carry schema field/rule identifiers without submitted
values or CEL text. Unsupported owned constraints refuse startup rather than
silently skipping validation. Adding a schema annotation also adds a
discriminating valid/invalid contract example.

Features own bounded aggregate collection, additional business idle/duration
limits and cancellation of work they spawn. The transport owns the RPC future,
body and admission permit, not arbitrary detached feature tasks.

## Failures and limits

Use `infra_grpc::classified_status(service_failure::ClassifiedFailure::new(code))`
for a shared domain failure. An optional policy-owned retry delay becomes
`google.rpc.RetryInfo`; it does not enable retries. `google.rpc.ErrorInfo` carries
the stable shared code in the fixed `service` domain. HTTP continues to project
the same shared identity into its existing status, title, URI and payload.

| Shared meaning | gRPC code |
| --- | --- |
| Bad request, invalid input, malformed authentication | `INVALID_ARGUMENT` |
| Missing/invalid credentials or unauthorized | `UNAUTHENTICATED` |
| Forbidden | `PERMISSION_DENIED` |
| Not found | `NOT_FOUND` |
| Already exists | `ALREADY_EXISTS` |
| Conflict | `ABORTED` |
| Unsupported method | `UNIMPLEMENTED` |
| Classified limits and admission capacity | `RESOURCE_EXHAUSTED` |
| Unavailable dependency, trust or draining service | `UNAVAILABLE` |
| Expired request budget | `DEADLINE_EXCEEDED` |
| Unexpected error or recovered panic | `INTERNAL` |

Arbitrary handler statuses and panics become `INTERNAL` / `request failed`,
including faults after partial stream output. Private Rust provenance
distinguishes classified and framework failures; wire metadata cannot forge it.
Scoped panic recovery suppresses the panic payload before the process hook can
print it, while unrelated HTTP/background panics keep their existing behavior.

Fixed transport bounds are 256 concurrent business RPCs, a separate 4096-call
health pool, 4096 connections, 100 HTTP/2 streams per connection, 16 KiB aggregate
request metadata and 4 MiB per message in both directions. Admission never
waits for a business permit. A returned stream holds its permit through terminal
status or cancellation. A deadline takes and drops the actual suspended body
before releasing that permit, even when the peer stops reading.

The failure table describes classified service and admission failures.
Native tonic receive-size rejection uses `OUT_OF_RANGE`; native streaming
framing errors forwarded through a feature's raw `Status` still pass through
the handler privacy guard and can become sanitized `INTERNAL`. Semantic
Protovalidate rejection is independently sticky and cannot be swallowed.
Client-local encoding overflow follows tonic 0.14.6: the stateless codec rejects
the oversized message before serialization, while the native send path reports
`INTERNAL` with a fixed safe description. Classified resource/admission mapping
remains `RESOURCE_EXHAUSTED`. The bound applies to each unary or streaming
message and never enables retry or replay.

Unary RPCs have an eight-second safety deadline; an earlier caller deadline wins.
Authentication and validation spend that same deadline. Streams have no imposed
unary backstop: caller deadlines/cancellation and the shared process drain still
apply. A malformed `grpc-timeout` follows tonic's compatible ignore-as-absent
semantics without logging its raw value.

## Reuse clients and original deadlines

Create one lazy `infra_grpc::Client` per trusted operator destination. Explicitly
choose `ClientSecurity::Plaintext` or TLS with normal certificate/hostname
verification, optional CA and optional client identity. Construction performs no
DNS or socket I/O. Clones share the channel's connection/reconnect resources.

Attach the generated immutable method catalog before constructing a native stub:

```rust,ignore
let transport = infra_grpc::Client::new(destination, security)?;
let transport = grpc_contracts::generated::echo_service_client_transport(transport)?;
let mut client = grpc_contracts::generated::echo_service_client::EchoServiceClient::new(transport);
let mut request = tonic::Request::new(grpc_contracts::generated::UnaryRequest {
    message: "hello".into(),
});
request.extensions_mut().insert(infra_grpc::Operation { deadline });
let response = client.unary(request).await?;
```

The absolute operation deadline is required, bounded by any current parent and
spent across readiness, connect, dispatch and the response's terminal lifetime.
The governed server reports its expired deadline as `DEADLINE_EXCEEDED`.
Before response headers, tonic's native `Channel` also enforces `grpc-timeout`
and can report `CANCELLED` when its timer wins. Client timeout codes therefore
retain native tonic behavior while spending the same operation budget.
The stateless generated client codec enforces per-message size without capturing
an inbound call, so nested outbound calls remain independent. No automatic
application retry, replay, hedging, discovery or client health polling is added.

<!-- template:begin outbound-auth-grpc:docs-grpc-oauth -->
When OAuth is also selected, bind the configured transport inside its existing
private credential owner before giving it to the native generated client:

```rust,ignore
let transport = grpc_contracts::generated::echo_service_client_transport(transport)?;
let authenticated = credentials.grpc(transport);
let client = grpc_contracts::generated::echo_service_client::EchoServiceClient::new(authenticated);
```

Preexisting Authorization is rejected before token or resource I/O. Acquisition
and hard-expiry checks spend the original deadline; failure prevents dispatch.
One sensitive bearer value is injected at opening. A stream does not refresh it
mid-call. Resource `UNAUTHENTICATED` and `PERMISSION_DENIED` pass through without
token invalidation or replay. The [OAuth owner](outbound-machine-authentication.md)
keeps the token, cache and coalescing policy; there is no public token getter.
<!-- template:end outbound-auth-grpc:docs-grpc-oauth -->

## Health, shutdown and observation

Standard Check and Watch read the existing cached readiness verdict for the
overall service and registered service names. They never probe dependencies.
Before startup admission, on stale/failed readiness and after stopping, known
services are `NOT_SERVING`. Unknown Check is `NOT_FOUND`; unknown Watch publishes
`SERVICE_UNKNOWN` and remains open. A readiness update cannot undo shutdown.

The first signal closes business admission and publishes terminal health before
the propagation delay. HTTP and gRPC then drain concurrently under the one
remaining process deadline. Existing business RPCs may finish; health watchers
do not prevent exit. Forced expiry drops calls and joins transport-owned
connection, HTTP/2 stream and deadline tasks. The existing 17-second cleanup tail,
second-signal handling and exit meanings remain: 0 clean, 3 overrun, 1 startup
failure. Partial startup resources enter the same bounded cleanup path.

Tracing uses the existing providers and W3C propagation. Known generated methods
have one full-call outcome and duration through final stream status or drop;
routine health polling and unknown peer paths create no method series. Counters
and histograms are `grpc_calls_total` and `grpc_call_duration_seconds`, with
finite method/direction/outcome labels. Payloads, metadata values, bearer tokens,
identities and raw errors are never transport attributes.

## Generate and verify

`make grpc-generate` uses pinned Buf descriptors and the separately locked
development generator. Commit schema, `buf.lock`, descriptors and generated Rust
together. Never edit generated Rust. Application contracts are not generated by
normal service builds or runtime images.

The maintained validator dependency compiles its own packaged schemas. Run
`make grpc-tools` once before direct Cargo use; normal build/test make targets
do that preflight automatically. Provisioning downloads a pinned, checksum-checked
official compiler into the managed tool cache. After provisioning, compiler
invocation is cache-only and offline builds require no system `protoc` or
generation network. The final runtime image contains no compiler.

`make grpc-check` runs actual Buf format/lint, repeats generation, compares
committed bytes and checks breaking changes against `GRPC_BASE_REF` (the exact
pull-request base in CI). An initial addition reports that the valid base has no
old protobuf contract; an unreadable base fails. Buf STANDARD lint and FILE
compatibility remain enabled. The generator is not a runtime workspace member.

CI owns the heavyweight build, protocol/process tests and retained initializer
graphs. Five gRPC graphs cover none/JWT/introspection/OAuth and the maximal
compatible OAuth/NATS/outbox tuple without multiplying harnesses or database
suites. `GRPC=none` preserves shared failure, telemetry/prost and TLS dependencies
that surviving profiles still own. Profile choices and replay are recorded in
`template.lock`; old locks without gRPC select `none`.
