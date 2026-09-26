# Native gRPC decisions

The [adoption guide](grpc.md) owns the supported API and operator contract.
These decisions record why the implementation has its present boundaries.
Source inspection was refreshed on 2026-09-26; version selection is distinct
from the exact locked graph and executable CI evidence.

## Ownership and library selection

| Decision | Alternative and decisive gap |
| --- | --- |
| Separate native HTTP/2 listener over tonic 0.14.6 and prost 0.14.4 | REST transcoding, gRPC-Web and a second business API are outside the required native transport profile. |
| `service-failure` owns closed `Code`, meanings and safe classification; HTTP owns its RFC 9457 projection | A second gRPC domain catalog would drift. Moving `Problem` into the leaf would make domain failure identity depend on HTTP. Existing HTTP status, title, URI, body, header and OpenAPI output remain unchanged. |
| `infra-grpc` owns policy and transport lifetime; `grpc-contracts` owns committed generated messages/native traits and the generated policy adapter | Putting policy in each handler makes validation, panic/privacy and stream admission optional. A feature-facing replacement stream/trait would lose native tonic composition. |
| prost-reflect 0.16.5 and prost-protovalidate 0.6.0 with CEL | The validator's tonic helpers require per-handler/per-message calls. Its generated-validator companion does not provide the accepted Prost CEL bridge. No template-owned CEL engine or protobuf parser is introduced. |
| Existing bearer verifier, readiness reader, OTel 0.32/tracing bridge 0.33 and metrics facade | A second verifier/cache, per-health-RPC probes, separate exporter or another OTel minor would duplicate an existing authority. |
| Optional concrete `Credentials::grpc` inside the existing OAuth owner | A public token getter or generic speculative authorizer loses custody; a second token cache or resource retry can replay an effect. |

The Rust validator is third-party, not a Buf-maintained implementation. Its
maintained upstream and [published API](https://docs.rs/prost-protovalidate/0.6.0/prost_protovalidate/)
support the chosen reflection/CEL path. Startup preloads registered inbound
roots and reachable descriptors with lazy compilation disabled. An ordinary
constraint violation from an empty message proves compilation only; compiler
and evaluator failures refuse registration. Unknown constraint fields are not
allowed. Owned annotations have valid/invalid fixtures; this is not a claim of
complete cross-language conformance.

## Generated authority and two codec roles

Buf 1.73.0 owns parsing, STANDARD lint and FILE compatibility. Proto3 with explicit
optional presence is supported by the selected Prost/validation ecosystem;
Edition 2023 is not required. Packages are versioned and removed fields/names
must be reserved. Buf's pinned schema dependency is recorded in `buf.lock`.

The [development generator](../tools/grpc-codegen/src/main.rs) consumes Buf's
import-complete `FileDescriptorSet`. One prost pass emits messages. A composite
public `prost_build::ServiceGenerator` invokes tonic's server-only and
client-only generators, then adds a proxy implementing the same native server
trait and a finite method catalog. Descriptor traversal attaches supported
reflection attributes, including exact full message names; it is not a schema
parser or a rewrite of emitted tonic code.

The generator is installed on `prost_build::Config`, which calls
[`compile_fds`](https://docs.rs/prost-build/0.14.4/prost_build/struct.Config.html#method.compile_fds)
directly. Tonic's `compile_fds_with_config` convenience method replaces a
previously installed generator and would silently remove the proxy. The
generator has its own lock and is excluded from the runtime workspace.

Server dispatch uses `ValidatedCodec`: native Prost decoding followed by
semantic validation before the feature receives a message. Its decoder captures
the private per-call state at codec construction and carries it with a moved
`Streaming` value. First semantic validation failure is sticky, so swallowing
that validation error cannot later return success or another response item.

Client dispatch uses a separate stateless `BoundedClientCodec`, delegating to
the public Prost encoder/decoder. It checks `Message::encoded_len()` before
encoding and `DecodeBuf::remaining()` before decoding, each at 4 MiB. The latter
is already a single framed, uncompressed message supplied by tonic. There is no
framing parser, serializer or server call-state capture. This closes two native
client gaps: encoding defaults to unbounded, and a fluent maximum alone can be
omitted or raised. It also keeps standalone and nested outbound calls correct.
The native receive limit remains in place; the codec bounds remain effective
if a caller changes fluent limits. Native generic clients continue to accept
the private OAuth `Service`.

Tonic's [encoder](https://docs.rs/tonic/0.14.6/src/tonic/codec/encode.rs.html)
wraps encoder errors in a new `INTERNAL` status, dropping their code/source.
Hyper propagates body failure as HTTP/2 `INTERNAL_ERROR`; tonic's `server`
feature enables its [native HTTP/2 status mapping](https://docs.rs/tonic/0.14.6/src/tonic/status.rs.html)
even for a Channel. That feature is enabled, while the listener still uses
Routes/Hyper. [gRPC status guidance](https://github.com/grpc/grpc/blob/master/doc/statuscodes.md)
recommends `RESOURCE_EXHAUSTED` for configured-size overflow; this client-local
deviation preserves the selected library's actual behavior and the accepted
preference for minimal custom policy. No outbound poll scope, latch, raw-text
parsing or client facade is added solely to force code 8. Reopen if tonic gains
a supported error-preserving hook or a consumer contract requires a distinct
local-size status.

Remote Buf generation would add registry execution and availability to the
source boundary. A separate vendored compiler for application generation would
duplicate Buf's compiler. Descriptor-driven generation avoids both. Owned
application messages and descriptors are committed and reproduced in CI, not
generated in ordinary service builds.

## Compiler for the maintained validator dependency

The unchanged upstream `prost-protovalidate-types` 0.6.0 package compiles its
packaged standard-rule schema in its build script, with or without reflection.
It exposes no feature to disable that compilation. This upstream dependency
build is separate from generation of owned application schemas.

A managed official protoc 36.2 host archive, verified against its pinned SHA256,
supplies the supported `PROTOC` setting. Plain Cargo, make, initialized trees,
CI and the Docker builder use the same resolver; there is no system compiler
fallback or nested Cargo invocation. Provisioning is cached. The final runtime
image includes neither compiler nor Python/build dependencies.

A fork of validator/types would add an upstream maintenance obligation merely
to change package generation. `protoc-bin-vendored` 3.2.0 pulls eight platform
archives (about 26 MiB); an official host archive is about 2.5–3.5 MiB for each
supported Linux/macOS architecture. The managed host tool is the narrower
provisioning boundary. Reopen when upstream ships pregenerated types or offers
a supported no-codegen package feature.

## Status provenance, panics and cancellation

Generated policy invokes feature methods inside the typed boundary. Shared
failures use tonic's private Rust `Status::source`; arbitrary handler statuses
cannot forge that source through metadata. Unknown methods and framing errors
that remain outside feature code use tonic's native statuses, including
`OUT_OF_RANGE` for its receive-size guard. A native streaming error returned by
feature code as raw `Status` is sanitized like other raw handler statuses; the
transport does not add a framing parser or claim sticky handling for all native
framing errors. Semantic validation has its separate sticky owner. Classified
details contain a stable `ErrorInfo`; policy-owned retry delay uses `RetryInfo`.
Validation detail identifiers come from the descriptor-owned validation path,
not arbitrary feature-provided strings. Rich details share the metadata bound.

The same guard catches initial-future and later stream-poll panics. Because
Rust invokes its hook before unwinding, enabled startup installs one hook that
suppresses payload output only inside private guarded polls and delegates to
the previous process hook otherwise. Per-request hook replacement would race
other requests; catching unwind without the scope would already have leaked
the payload.

A permit ends with the actual call, not the initial response headers. A private
shared slot owns the real tonic response body. Terminal deadline/cancellation
takes and drops that body before releasing the permit, independently of peer
flow-control progress. A bounded transport-owned waiter handles deadlines when
the peer stops reading. It performs no feature work and exits on terminal/drop.
Feature-spawned work remains feature-owned.

## Why Routes and Hyper own the listener

Tonic's native codec/dispatch remains authoritative. The HTTP/2 listener uses
public [`tonic::service::Routes`](https://docs.rs/tonic/0.14.6/tonic/service/struct.Routes.html)
with existing Hyper/Tokio mechanisms because tonic's transport Server has an
unavoidable private `GrpcTimeout` outside user layers. That timeout ends at
response headers, can report `CANCELLED` rather than the full-call owner's
`DEADLINE_EXCEEDED`, and logs malformed timeout values. A tracing filter or
cancellable incoming stream does not disable the conflicting timer. Reopen if
tonic exposes a supported outer/disabled timeout hook with equivalent task
ownership.

Existing rustls/tokio-rustls configs explicitly select TLS 1.3, normal hostname
and chain validation, and required client certificates when a client CA exists.
Tonic `ServerTlsConfig` has no protocol-floor setter; relying on absence of a
Cargo `tls12` feature is unsound when another retained dependency enables it.
Operator-selected plaintext remains valid with bearer authentication behind
the deployment's trust boundary.

The listener retains connection permits through TLS handshakes and connection
drop. Hyper enforces the header/stream limits. An initial five-second admission
cap reuses the HTTP default. A connection `JoinSet` is not enough: Hyper submits
H2 stream futures separately. A private supported Executor tracks those futures
and deadline waiters, prevents new task admission after closure, and cancels and
joins them on forced drain. No transport future is deliberately detached.

## Health, observation and shutdown

`health::ReadinessReader::changed_verdict` wakes on publication or the exact
staleness boundary. It does not create a second probe loop. The small standard
Health adapter uses upstream tonic-health protocol types; the stock reporter
starts overall health at SERVING and ends unknown Watch with NOT_FOUND, contrary
to the accepted startup latch and standard SERVICE_UNKNOWN watch behavior.

HTTP/gRPC startup admission opens only after required listeners and dependencies
are ready. First stop closes business admission and publishes terminal health
before propagation. Both transports share the remaining effective drain interval
and existing teardown tail. Health watchers do not own business-drain completion.
NATS and outbox keep their existing process owners and close within the same
process lifecycle; this profile adds no second signal or timeout budget.

The maintained `tonic-tracing-opentelemetry` 0.38 fits the repository's OTel
versions, but its source ends at the initial HTTP response and labels raw paths.
Wrapping it would retain a duplicate, prematurely finished span. The existing
terminal owner therefore uses the current tracing/OTel and metrics APIs directly
for known-method, full-call observation. No exporters or semantic-convention
framework are copied. Reopen when a maintained layer supplies a policy-controlled
catalog and full-body lifetime without duplicate spans.

## Projection and evidence

The five additional retained graphs cover gRPC without auth, with JWT, with
introspection, with OAuth and with the maximal compatible OAuth/NATS/outbox pack.
They do not multiply harnesses or repeat existing database proof. Initializer
pruning follows actual retained ownership, preserving the shared failure leaf
for HTTP and shared prost/TLS families where still needed.

Executable proof must cover the shipped registration/codec/client boundaries,
TCP/TLS and coordinated service process shutdown. Generated bytes must reproduce,
and compatibility uses the real PR base. Static design or local metadata alone
does not establish compilation, protocol correctness, a deployment or capacity.
The template adds no production deployment, publication, database migration,
certificate watcher, retries, discovery plane or performance certification.
