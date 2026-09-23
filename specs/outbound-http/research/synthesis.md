# Research synthesis: bounded outbound HTTP

Valid as of 2026-09-23. Decision affected: the stage-10.2 behavior contract and
later transport/ownership design. This is primary-source and current-checkout
evidence, not implementation proof. Refresh on dependency upgrade, a changed
IANA registry, a new target class, or a proposal to alter auth provider
semantics.

## Baseline and Go decisions

- Rust `main` at `098b4ab18dd5b2d158a94e126798d8cc429ad735` was clean and
  aligned with `origin/main` when Definition began. `Cargo.toml` already
  declares `reqwest = 0.13.5` and `hickory-resolver = 0.26.3` with default
  features off. `crates/infra-bearerauthn/src/provider.rs` uses the former for
  exact HTTPS provider calls, no proxy/redirect/retry, response-body ceiling,
  and request deadline. Its `provider/dns.rs` owns a cancellation-aware Hickory
  resolver and post-DNS public-address predicate. `docs/authentication.md`
  explicitly says the auth client caps header *count*, without a separate
  application-selected aggregate header-byte guarantee. This is a local fact;
  no general-client guarantee can be inferred from the existing code.
- The Go template was read from its committed HEAD
  `cdfb6a876f103a643d92da260aab974bd8a64f5a`; its working tree was dirty,
  so uncommitted Go changes were not treated as authority. Its
  `internal/infra/httpclient/client.go`, `target_policy.go`,
  `propagation.go`, and `docs/first-production-feature.md` separate one fixed
  HTTPS authority, dial-time IP admission, disabled proxy/redirect, mandatory
  response-header/body/concurrency limits, operation budgets, and removal of
  `traceparent`, `tracestate`, `baggage`, `X-Request-ID`, and
  `Accept-Encoding`. Provider auth, retry decisions, errors, and telemetry stay
  outside the common transport. Go also has a private-network constructor; it
  is not part of Rust stage 10.2. Go's `net/http` dial and body wrappers are
  precedents for guarantees, not APIs to port line by line.
- `docs/template-sync.md#validation-boundary` currently proves 48 canonical
  DATABASE × AUTHN × harness projections and six runtime graphs. Its public
  initializer still runs full locked metadata, formatting, and OpenAPI
  preflight before changing a target. Adding a binary outbound choice doubles
  the projection and runtime-graph counts, without making the harness a Rust
  build/test dimension. This is an inference from the accepted matrix contract;
  implementation must prove it against the resulting source tree.

## Current primary Rust mechanisms

| Claim and applicability | Primary evidence | Decision effect and limit |
| --- | --- | --- |
| [`reqwest` 0.13.5](https://docs.rs/crate/reqwest/0.13.5) is the current published line (2026-09-08), maintained in the active reqwest repository. It is already resolved locally. | [Version history](https://docs.rs/crate/reqwest/0.13.5), [ClientBuilder](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html) | Use it rather than introduce a second HTTP client. No upgrade is needed for this stage. |
| `ClientBuilder::dns_resolver` replaces resolution with a `Resolve` implementation; `no_proxy` disables automatic system proxy; `https_only`, `redirect`, `retry`, `referer`, and compression toggles are supported. Default redirects and protocol-NACK retries require explicit override. | [ClientBuilder methods](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.dns_resolver), [no_proxy](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.no_proxy), [retry](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.retry) | A private builder plus resolver can enforce the one-hop policy while retaining the URL hostname for TLS. A preflight DNS lookup followed by reqwest's ordinary resolution would be a TOCTOU gap. No caller access to a mutable builder or raw reqwest client may bypass these controls. |
| `http1_max_headers` limits response header **count** and `http1_only` selects HTTP/1. The 0.13.5 builder has no HTTP/1 application-selected aggregate header-byte setter; its HTTP/2 header-list setter is feature-specific. | [HTTP/1 header methods](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.http1_max_headers), [HTTP/2 method](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.http2_max_header_list_size) | A Rust aggregate header-byte promise needs an additional check on parsed headers or a separately justified lower transport. An after-parse check can keep oversized headers from callers but is not a claim about parser memory before rejection. Auth's existing count-only contract remains truthful. |
| A reqwest total timeout covers connection through completion of the response body; there is no default timeout. Automatic decompression can transform what response headers and body bytes mean. | [timeout](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.timeout), [decompression](https://docs.rs/reqwest/0.13.5/reqwest/struct.ClientBuilder.html#method.no_gzip) | A deadline must cover DNS, headers, and bounded body read. Disable transparent decompression so byte ceilings have one meaning, or prove decoded-byte enforcement if enabled. A streaming return would require separate ownership of permits and deadlines. |
| [`hickory-resolver` 0.26.3](https://docs.rs/crate/hickory-resolver/latest) is current (2026-09-10) and maintained; the installed `system-config` and `tokio` features provide system configuration and async lookup. | [Version history](https://docs.rs/crate/hickory-resolver/latest), [system config](https://docs.rs/hickory-resolver/0.26.3/hickory_resolver/system_conf/index.html), [Tokio resolver](https://docs.rs/hickory-resolver/0.26.3/hickory_resolver/type.TokioResolver.html) | Reuse the resolved library. The auth resolver demonstrates why DNS task cancellation and join need explicit ownership; do not assume a dropped application future joins library-internal tasks. |
| IANA's current IPv4 and IPv6 special-purpose registries were last updated 2025-10-09 and define a `Globally Reachable` flag with specific exceptions. | [IPv4 registry](https://www.iana.org/assignments/iana-ipv4-special-registry), [IPv6 registry](https://www.iana.org/assignments/iana-ipv6-special-registry) | Public-unicast admission must reject local, private, reserved, documentation, and ambiguous special addresses, including embedded/mapped IPv4 cases. Registry status is a policy basis, not a promise of Internet routability. The existing auth predicate and Go corpus are relevant comparators; any shared predicate change requires auth regression proof. |

## Comparison and decision implications

`reqwest` plus its custom resolver seam is the required and already resolved
transport. `hickory-resolver` is already resolved and handles asynchronous DNS;
reqwest's built-in DNS feature alone is insufficient because it does not apply
the stage's public-address predicate to the answer set. Raw `hyper` would move
HTTP, TLS, redirection, pooling, and response handling back into template code
without meeting the requested reqwest choice. A second HTTP crate therefore has
no current justification. Existing `url`, Tokio, and `tokio-util` supply URL
parsing, deadlines, cancellation, and task tracking.

The auth client's *transport facts* are useful evidence, but the entire client
is an unsuitable public API: it has fixed JSON 200 semantics, three-second auth
budget and 100 ms response reserve, auth failure identity, auth tracker/cancel
ownership, no connection reuse, and JWT/introspection-specific call shapes.
Common reuse is justified for a pure public-address policy and perhaps a
well-defined DNS admission component if Design proves that its lifecycle can
serve both owners unchanged. Otherwise keep auth transport intact and explain
the bounded duplication. Do not move JWT/introspection parsing, failure
taxonomy, refresh custody, or per-request deadlines into the general client.

The Go client's mandatory header-byte cap is stronger than the present auth
client's count-only guarantee. `reqwest` 0.13.5 supports an HTTP/1 parse-time
count cap but not a configurable HTTP/1 parser byte cap. The feasible current
contract is a finite application-visible aggregate header-byte check after
parsing, plus the native count cap and a total deadline. Design must either
implement that truthful boundary or produce fresh evidence for an equivalent
stronger reqwest-compatible mechanism; it must not describe post-parse
rejection as a pre-parse allocation ceiling.

## Falsifiers and downstream proof

The strongest counterexample to a prelookup design is DNS rebinding between
the lookup and reqwest's connection. The actual resolver handed to reqwest must
return only admitted answers, and a fixture should show that rejected answers
cannot be connected to. A mixed public/private answer set, IP literals,
mapped IPv4, and an attempted proxy are discriminating denials. A large single
header distinguishes count from aggregate bytes. A chunked response that
exceeds its cap distinguishes `Content-Length` checking from bounded reads.
No live public provider or production network is needed for Definition; local
TLS and DNS fixtures can prove the eventual implementation. Refresh this
evidence if the resolved versions change or if the design enables HTTP/2,
compression, connection reuse, or another destination class.
