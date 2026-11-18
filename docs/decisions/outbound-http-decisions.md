# Outbound HTTP decisions

`crates/infra-outbound-http` and `Cargo.toml` own the current API, transport
limits, error variants, and dependency pins. This record keeps only rejected
alternatives, external protocol constraints, and reopen conditions.

The `reqwest` rejection covers this client only and leaves the template with the
`outbound-http` profile; the `infra-telemetry` OTLP exporter keeps its
`reqwest` transport.
<!-- template:begin authn:docs-outbound-http-authn-scope -->
`infra-bearerauthn` provider calls keep `reqwest` for the reasons in [library
selection](../backend-library-selection.md#rejected-candidates).
<!-- template:end authn:docs-outbound-http-authn-scope -->

| Rejected alternative | Why rejected | Reopen condition |
| --- | --- | --- |
| `reqwest`, `reqwest-middleware`, or `tower::retry` | `reqwest` adds URL/header copies and redirect/retry/proxy/decompression layers this profile disables; generic retry middleware hides operation-specific idempotency and deadline accounting ([vendored Happy Eyeballs patch](../../vendor/hyper-util/PATCHES.md) covers `hyper-util`). | Upstream `hyper-util` / `hyper-rustls` drops required transport hooks, or a caller requires HTTP/2 or proxy negotiation. |
| Custom public-IP / DNS egress filter (`infra-egress-dns`) | Blocked private providers holding valid TLS certificates while leaving TOCTOU gaps against custom resolvers; network policy, explicit origins, and TLS form the trust boundary. | An operation accepts untrusted caller-supplied destination URLs (requires a dedicated SSRF design). |
| Per-client `rustls::ClientConfig` or per-client metric-handle caches | Per-client root parsing cost ~6 ms and ~85 KB per endpoint; per-client metric-handle caches bind attempts to whichever recorder ran first in tests. | Linux trust-store reload without process restart becomes required. |
| `Content-Length` buffer presizing or automatic `url.full` / trace-header injection | Untrusted `Content-Length` reserves memory before bytes arrive; webhook URLs can carry provider secrets in paths or queries. | Never for untrusted response headers; opt in to trace propagation per trusted peer. |
| Default 90 s pooled idle timeout | Common provider front ends close idle keep-alive sockets at 60 s (AWS ALB) or 75 s (nginx), causing reuse races on half-closed connections. | Provider topology guarantees longer idle keep-alive windows. |

<!-- template:begin webhooks:docs-outbound-http-webhook-delivery -->
## Outbound webhook delivery

A receiver can accept a message even when the sender loses its acknowledgement.
The worker therefore retries with the same `webhook-id`. When a receiver reports
repeated deliveries, correlate that ID before replaying or applying its effect again.

- **Successful acknowledgement.** The worker completes a delivery after a
  complete `2xx` response within the 64 KiB body limit and the attempt deadline.
  A receiver can log `200` and still see a retry if its reply exceeds that
  limit or its body fails to arrive. An empty `204` is sufficient. This keeps
  a receiver's response within the bounded HTTP client's resource budget.
- **Retiring an endpoint.** A complete `410 Gone` permanently fails this job
  with `endpoint_gone`. The same applies when the worker saw `410` and reading
  its body then exceeded the size limit or failed in transport. A deadline or
  cancellation can still end the attempt. Endpoint bindings come from startup
  configuration, so later jobs can still send. Stop producers from enqueueing
  new deliveries for the endpoint, account for pending, running and failed
  jobs, then remove the binding and restart the affected processes. Removing
  the binding first makes remaining deliveries retry with `missing_endpoint`.
  Revisit automatic disabling if endpoints gain a durable owner for changes at
  runtime.
- **Spacing retries.** Delivery uses the queue's jittered fourth-power backoff
  so one scheduler owns all attempts. A positive `Retry-After` on a complete
  retryable HTTP response sets a delay floor, capped at 24 hours. The worker
  accepts delta-seconds or a future HTTP date. Zero, an elapsed date, a missing
  field or an invalid value leaves the queue schedule in effect. The 24-hour
  cap applies to the receiver's hint, and the queue delay can be longer.

Standard Webhooks treats a `2xx` as success and recommends disabling an endpoint
on `410`. It recommends exponential backoff for retries.
[Standard Webhooks delivery guidance](https://github.com/standard-webhooks/standard-webhooks/blob/bece768d960f09e242f5cd5686d859e475d6b478/spec/standard-webhooks.md#delivery-success-and-failure).
The delay field follows
[RFC 9110 §10.2.3](https://www.rfc-editor.org/rfc/rfc9110#section-10.2.3).
<!-- template:end webhooks:docs-outbound-http-webhook-delivery -->
