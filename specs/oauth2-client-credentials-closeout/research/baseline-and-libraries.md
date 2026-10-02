# Baseline and library evidence

Status: ready. Valid as of 2026-10-02. Supports Definition and Technical Design; it does not select a migration. Stop condition: establish the current defects and whether a credible maintained library changes the old absence claim. No throughput, RSS, live-provider or CI claim is made.

## Current checkout facts

Baseline `67be869acea112af271ec8ba621cbc50ae9d36b7`, inspected through the task worktree CodeGraph and current documentation:

| Fact and primary locator | Decision effect |
| --- | --- |
| [`Credentials::acquire_service_token`](../../../crates/infra-oauth2-client-credentials/src/lib.rs) serializes requests with a refresh mutex; a failed fetch returns before storing anything. The next waiter starts a fresh request. | Concurrent quick failures amplify provider requests serially; serialization is not failure coalescing. Close the service-token failure behavior. |
| `refresh_ahead` discards the spawned handle and captures a strong Credentials clone; its five-second fetch deadline starts after acquiring the refresh lock. | The operation is runtime-bounded but is not cancelled by the last external owner, and lock waiting is outside that five-second budget. Close ownership and total background budget. |
| `into_token` maps both absent expiry and failed `Instant::checked_add` to no cutoff; service-token reuse interprets that as indefinitely reusable. Exchanges already avoid retaining a missing-expiry token. | Separate omission from overflow; keep permissive omission without indefinite cache reuse. |
| `Credentials::prepare` uses Moka max_capacity as an entry count. The token response limit is one MiB and configured entry target is 1–65536. | Even an entry target would permit roughly 64 GiB of response-scale payload at its largest setting; this is a dimensional upper-envelope calculation, not measured memory or a strict bound. Add a payload retention target. |
| [Service-to-service guide](../../../docs/service-to-service-authentication.md) says iat/nbf are now. [Outbound guide](../../../docs/outbound-machine-authentication.md) and implementation say ten seconds back; the decision record says fifty seconds of remaining validity. | Correct the inconsistent guide while preserving existing wire behavior. |

[The Moka 0.12.16 Cache documentation](https://docs.rs/moka/0.12.16/moka/future/struct.Cache.html) explicitly describes best-effort eviction and a supported weigher for size-based capacity. An entry count is not an RSS bound. The byte target in the spec likewise cannot be presented as instantaneous memory admission.

[RFC 6749 section 5.1](https://www.rfc-editor.org/rfc/rfc6749#section-5.1) recommends expires_in but permits omission with lifetime supplied by other means or documented defaults. [RFC 8693 section 2.2.1](https://www.rfc-editor.org/rfc/rfc8693#section-2.2.1) retains recommended lifetime reporting for exchanged tokens. This profile has no trusted provider lifetime default, so one-call use avoids inventing one. Overflow is an implementation representability error, not evidence that the provider omitted expiry.

## Same-level alternatives for the existing two-grant adapter

Published source was downloaded in memory from crates.io API/download endpoints and inspected, without installing dependencies or changing Cargo.lock. docs.rs returned errors for Huskarl during this research; the published archives supplied the evidence instead. The comparison stops short of a compile probe because dependency admission and mechanism selection belong to Technical Design.

| Alternative | Evidence and supported fit | Limits / counter-evidence |
| --- | --- | --- |
| Retain template forms/cache policy on installed libraries | Current lib.rs implements the two grant forms, fixed outbound client, jsonwebtoken signing, closed errors, per-owner reuse and transport wrappers. Existing suite is reusable baseline evidence. | Owns protocol and concurrency policy, including the confirmed gaps above. Retention requires a positive local-fit justification; “no library exists” is false. |
| oauth2 5.0.0 | [Official versioned API](https://docs.rs/oauth2/5.0.0/oauth2/) covers client credentials and custom HTTP clients. Existing [decision record](../../../docs/outbound-machine-authentication-decisions.md) identifies the versioned endpoint/auth API gap. | No native private_key_jwt/token-exchange request in that public API. Supplying extra fields does not remove ownership of both grant semantics or signing/cache policy. It is not a complete same-level substitute for this profile. |
| Huskarl 0.11.4 + huskarl-core 0.10.5 | Published 2026-10-01; MIT OR Apache-2.0, MSRV 1.92. Published client source `src/grant/client_credentials.rs` and `src/grant/token_exchange.rs` defines both grants. README/core API documents private_key_jwt and DPoP. Core `src/http/mod.rs:342` supplies `HttpClient`; `src/crypto/signer/mod.rs:49` supplies `JwsSigner`, with a supported selector. Client features make platform crypto optional. | A credible actively maintained candidate, but pre-1.0 and a young project. Repository created 2026-03-24, pushed 2026-10-01 (GitHub API); no independent operational maturity or security-audit claim established. Its default ecosystem is broader than the requested grants. Existing error mapping, deadlines, assertion clock/typ/audience, no replay and cache policies require a fit assessment; its published examples include retry policies the template must not blindly inherit. |

Primary archive and metadata locators: [huskarl 0.11.4 metadata](https://crates.io/api/v1/crates/huskarl/0.11.4), [client source archive](https://crates.io/api/v1/crates/huskarl/0.11.4/download), [core source archive](https://crates.io/api/v1/crates/huskarl-core/0.10.5/download), and [maintainer repository metadata](https://api.github.com/repos/huskarl-rs/huskarl).

The published `huskarl-crypto-native` 0.11.1 Cargo.toml pins `rsa = 0.10.0-rc.18`; this is a prerelease dependency cost, not proof of a vulnerability. [Published crypto archive](https://crates.io/api/v1/crates/huskarl-crypto-native/0.11.1/download). A fair comparison must include disabling platform crypto and implementing the supported signer seam using the existing admitted backend; rejecting Huskarl because its optional default backend exists would be unsound. Other new dependencies include its core/macros, bon, snafu and arc-swap families; exact resolved transitive cost and vulnerability admission are unknown until the selected design evaluates its proposed graph.

The maintainer README claims conformance-suite and provider CI coverage and explicitly distinguishes this from formal OpenID certification. These are upstream claims, not independently rerun results. The library's existence disproves the old “no maintained Rust client” rationale for token exchange and DPoP. It does not prove a migration reduces this template's total mechanism or that DPoP should be deployed in this PR.

## Decision handoff and refresh conditions

Definition fixes observable behavior in [spec.md](../spec.md). Technical Design selects retain/replace after checking supported extension points, actual code removed versus adapters added, dependency admission, compatibility and proof burden. No new capability or cryptographic backend is necessary merely because one is available.

Refresh this research if a different library/version is proposed, an inspected upstream source changes before admission, a current security advisory affects the selected graph, or a provider requires behavior outside the preserved profile. Reopen Definition if the chosen design cannot preserve the accepted observable behavior without a new product/platform decision. Local test counts reported by the parent are historical baseline evidence only; no tests, live Keycloak or CI were rerun in this Definition phase.
