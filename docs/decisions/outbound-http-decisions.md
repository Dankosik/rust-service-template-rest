# Outbound HTTP decisions

`crates/infra-outbound-http` and `Cargo.toml` own the current API, transport
limits, error variants, and dependency pins. This record keeps only rejected
alternatives, external protocol constraints, and reopen conditions.

| Rejected alternative | Why rejected | Reopen condition |
| --- | --- | --- |
| `reqwest`, `reqwest-middleware`, or `tower::retry` | `reqwest` adds URL/header copies and redirect/retry/proxy/decompression layers this profile disables; generic retry middleware hides operation-specific idempotency and deadline accounting ([vendored Happy Eyeballs patch](../../vendor/hyper-util/PATCHES.md) covers `hyper-util`). | Upstream `hyper-util` / `hyper-rustls` drops required transport hooks, or a caller requires HTTP/2 or proxy negotiation. |
| Custom public-IP / DNS egress filter (`infra-egress-dns`) | Blocked private providers holding valid TLS certificates while leaving TOCTOU gaps against custom resolvers; network policy, explicit origins, and TLS form the trust boundary. | An operation accepts untrusted caller-supplied destination URLs (requires a dedicated SSRF design). |
| Per-client `rustls::ClientConfig` or per-client metric-handle caches | Per-client root parsing cost ~6 ms and ~85 KB per endpoint; per-client metric-handle caches bind attempts to whichever recorder ran first in tests. | Linux trust-store reload without process restart becomes required. |
| `Content-Length` buffer presizing or automatic `url.full` / trace-header injection | Untrusted `Content-Length` reserves memory before bytes arrive; webhook URLs can carry provider secrets in paths or queries. | Never for untrusted response headers; opt in to trace propagation per trusted peer. |
| Default 90 s pooled idle timeout | Common provider front ends close idle keep-alive sockets at 60 s (AWS ALB) or 75 s (nginx), causing reuse races on half-closed connections. | Provider topology guarantees longer idle keep-alive windows. |
