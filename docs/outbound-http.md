# Bounded outbound HTTP

Select `OUTBOUND_HTTP=bounded` (or `--outbound-http bounded`) during
initialization to retain `infra-outbound-http`. The default `none` removes the
client, its tests and this guide. Selection adds no startup request, provider
configuration, credentials, route or readiness dependency.

## One client per fixed dependency

The provider adapter owns its endpoint, credentials, parsing, business errors,
retry eligibility and readiness policy. Bootstrap constructs one reusable
`infra_outbound_http::Client` with the runtime's existing `TaskTracker` and a
child of its root `CancellationToken`. Construction reads resolver configuration
without DNS or network I/O. The client never closes the shared tracker.

```rust,ignore
use std::time::Duration;
use infra_outbound_http::{Client, Limits};

let client = Client::new(
    "https://api.example.com/v1/",
    Limits {
        max_active: 16,
        operation_timeout: Duration::from_secs(2),
        request_header_count: 32,
        request_header_bytes: 8 * 1024,
        response_header_count: 64,
        response_header_bytes: 16 * 1024,
        request_body_bytes: 64 * 1024,
        response_body_bytes: 1024 * 1024,
    },
    tracker.clone(),
    root_cancel.child_token(),
)?;
```

These are example provider policy values, not defaults. Every limit must be
positive and representable; zero or overflow refuses construction. Header
counts cannot exceed the underlying HeaderMap capacity of 32,768 entries. Clones
share the same operation slots. Exhaustion returns `AtCapacity` immediately,
without a queue or DNS work, and a slot stays held through the complete body.

Only public HTTPS destinations are supported. The base refuses userinfo,
query, fragment, controls and non-public IP literals. Each target must retain
the base's canonical host and effective port. Relative references follow
`Url::join`: `items` under `/v1/` becomes `/v1/items`, while `/items` starts at
the origin root. A base ending in `/v1` treats `v1` as the last path segment
and replaces it. Same-authority query strings are permitted. Never derive a
target authority from inbound data. Host overrides and fragments refuse.

Every DNS connection uses only the actual admitted answer set. An empty set,
failed lookup or any non-public answer rejects the whole attempt. The shared
`infra-egress-dns` policy denies private, loopback, link-local, multicast,
documentation, reserved and ambiguous special-purpose addresses. IPv4-mapped
IPv6 and the well-known NAT64 prefix apply the IPv4 rule to their embedded
address; other IPv6 must be in `2000::/3` and pass the special-range rules,
including refusal of `2002::/16`. The configured hostname remains the TLS
verification and SNI name. There is no private-network or custom-root escape.

## Use the enclosing deadline and cancellation

For request-owned work, read the stamped `infra_http::RequestDeadline` from
request extensions. Missing stamp refuses the integration; do not invent a
new budget. The provider chooses enough response reserve for parsing and the
inbound response. This example reserves 100 ms only as an adapter choice:

```rust,ignore
use infra_outbound_http::{Error, HeaderMap, Method, Operation, Request};

let parent = extensions
    .get::<infra_http::RequestDeadline>()
    .ok_or(Error::InvalidConfiguration)?;
let deadline = parent.at()
    .checked_sub(Duration::from_millis(100))
    .ok_or(Error::Timeout)?;
let response = client.execute(
    Request {
        method: Method::GET,
        target: "items?limit=10".to_owned(),
        headers: HeaderMap::new(), // Set provider credentials explicitly here.
        body: Vec::new(),
    },
    Operation {
        deadline,
        cancel: request_cancel.child_token(),
        timeout: Some(Duration::from_secs(1)),
        response_body_bytes: Some(64 * 1024),
    },
).await?;
```

`request_cancel` is the invoking adapter's cancellation token. The handler
awaits `execute` directly, so dropping the handler drops the exchange; do not
spawn it as detached work. Background callers supply their own finite enclosing
deadline and cancellation token. Optional timeout/body limits may narrow the
client ceilings; zero or a larger value refuses. One absolute deadline covers
DNS, connection, TLS, response headers and body. An expired deadline or observed
cancellation refuses before sending. A timeout or cancellation after sending
does not establish that the provider performed no effect; the transport never
retries or reconciles it.

Dropping the exchange releases its slot and cancels its lookup. Hickory tasks
remain tracked until joined. On shutdown the runtime cancels the root token,
closes its tracker and waits within its existing background shutdown budget
([Runtime Lifecycle](architecture/runtime-lifecycle.md#shutdown)). The client
adds no background service or separate teardown stage.

## Returned bytes and headers

`Response` returns status, bounded headers and buffered body for every status,
including non-2xx and redirects. The provider maps status and parses content.
No raw client, response stream or builder is exposed. HTTP/1 is used without
idle connection reuse, redirects, retries, ambient proxies, cookie storage or
automatic referer generation. Normal TLS chain/name checks remain enabled.

The client removes `traceparent`, `tracestate`, `baggage`, `X-Request-ID` and
`Accept-Encoding` before checking supplied headers. It does not propagate an
inbound token or trace automatically. Explicit `Authorization` belongs to the
provider. Each field value, including duplicates, counts as one header and
`name.len + value.len + 4` bytes. Generated protocol fields such as `Host` and
`Content-Length` are outside this caller-supplied count. Response header count
is also capped during HTTP/1 parsing; aggregate bytes are checked after parsing,
so this is not a separate parser allocation ceiling. A parser refusal before
headers exist returns `Transport`; a parsed excess returns
`ResponseHeadersTooLarge`.

Request bodies above their cap refuse before sending. Advertised response
length above the cap refuses early. Unknown/chunked lengths are checked as
framed bytes arrive; exact-cap bodies succeed only at framed EOF, overflow
fails, and truncation is a transport failure. Bytes after a complete frame
are outside that response. Automatic decompression is disabled: byte limits
apply to returned encoded bytes. A provider accepting compressed data must
own a separate bounded decoding step. Internal transport chunks may exceed
the remaining allowance, but accumulated and returned bodies never exceed it.

## Failure mapping

`Error` carries static labels and exposes no URL, headers, body, DNS answers or
underlying transport error. Client/request/response diagnostics redact their
contents. The adapter translates these errors into its existing feature/HTTP
boundary; the transport creates no inbound Problem response.

| Error | Meaning |
| --- | --- |
| `InvalidConfiguration` | Invalid ceiling or operation override |
| `InvalidTarget` | Malformed or forbidden target syntax |
| `Denied` | Different authority or non-public destination |
| `AtCapacity` | All operation slots are occupied |
| `Timeout`, `Cancelled` | Effective budget expired or cancellation observed |
| `RequestHeadersTooLarge`, `RequestBodyTooLarge` | Supplied request exceeds its cap |
| `ResponseHeadersTooLarge`, `ResponseBodyTooLarge` | Response exceeds its cap |
| `Transport` | DNS lookup, connection, TLS, framing or other transport failure |

The auth-private transport keeps its own JSON/200-only policy, three-second
cap, 100 ms reserve, 32-exchange admission and count-only header limit. It
shares only admitted DNS and tracked cancellation with this profile.

## Mechanism and reopen conditions

The selected mechanism uses the resolved `reqwest` 0.13.5 client and
`hickory-resolver` 0.26.3. A small shared DNS crate is needed because auth and
outbound can be selected independently while both require the same admitted
answer set and tracked lookup cancellation. The outbound client retains its
own authority, byte, header and operation policy; the auth client retains its
provider-specific HTTP policy. The Definition and design comparison are
preserved in the local stage-10.2 implementation commit.

Reassess connection admission and error-source handling when changing reqwest
or Hickory versions, and reassess address rules when the IANA special-purpose
registries change. Private destinations, a proxy, connection pooling, HTTP/2,
streaming or automatic decompression require a new contract and proof; this
profile does not silently enable them.
