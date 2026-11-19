# T1 — Reduce service-token hit and credential cutoff work

Outcome:
Authenticated HTTP and gRPC service-token hits reuse the already eligible token synchronously before setting up asynchronous waiters when cooperative budget remains. Active service and exchange checks with a representable cutoff use the existing request stop check as the single primary deadline observation.

Depends on:
- none

Sources:
- [R1 and R2](../spec.md#r1--service-token-hits-and-refresh-waiting), [D1 and D2](../design/design.md#d1--select-a-reusable-service-token-before-setting-up-waiters) — ordering, cutoff provenance and preserved failure paths.
- `crates/infra-oauth2-client-credentials/src/credentials.rs::TokenSource::acquire_for_request`, `ServiceTokens::{check_active,service_token,reuse_service_token_and_schedule_refresh,acquire_service_token}`, `RegistrationState::check_active`, `AcquisitionDeadline::{stopped_error,fetch_context}`, `acquisition_deadline` — initial, inner, final and fetch-window checks and the still-synchronous cache decision.
- `crates/infra-oauth2-client-credentials/src/credentials/exchange.rs::ExchangeTokens::exchange`, `crates/infra-oauth2-client-credentials/src/token.rs::Token::{is_reusable,check_expiry}`, authenticated consumers in `crates/infra-oauth2-client-credentials/src/{http_client,grpc}.rs` — exchange work can suspend, and consumers still check actual token expiry at dispatch.
- `crates/infra-outbound-http/src/context.rs::RequestContext::{deadline,stopped,wait_stopped}`, `crates/operation-context/src/lib.rs::{Deadline::instant,OperationContext::stopped}` — primary deadline before its cancellation, followed by extension stops.
- `tokio@1.54.1/src/task/coop/mod.rs::has_budget_remaining`, `src/macros/select.rs::poll_budget_available`, `src/sync/watch.rs::Receiver::changed`; `moka@0.12.16/src/future/cache.rs::Cache::get` — budget probe does not spend budget, existing select yields and wakes, and exchange cache access awaits.

Provides:
- Credential acquisition fast path with unchanged stop and reuse semantics

Boundary:
- In scope: Credential acquisition, related cache or refresh arbitration only as specified in D1, cutoff provenance in the existing private credential owner, and affected credential coverage. Enter speculation only after the common active check, with cooperative budget and a successful inner service check. A miss, failed speculative check, exhausted budget or exchange takes the original biased select; preserve the inner service recheck there and the fresh final activity and expiry checks. Keep synthetic-ceiling expiry ahead of request stops and keep the stored cutoff and mapping for bounded waits.
- Out of scope: Context API changes, changes to HTTP/gRPC transport and observation, provider grants and the optional shared-cache variant.

Mutable owners:
- `crates/infra-oauth2-client-credentials/` acquisition and directly affected coverage.

Exclusive locks:
- none

Final validation:
- Observable claim: Through authenticated HTTP and gRPC acquisition, an eligible service hit with available budget bypasses construction of the registration/cancellation waiter path, including while a replacement grant is pending. Real waits and exhausted budget follow existing arbitration and still yield. A representable cutoff no longer repeats the primary clock check within one activity check. Synthetic cutoffs remain unavailable. Preserve G1, G2 and G5 including dispatch expiry, age-gated rejection and the selected token surviving a later rejection or replacement, stop and closure precedence, and grant and resource call counts.
- Additional proof: [R8 remote evidence](../design/comparison.md#workloads) for fresh and refreshing service hits and exchange work, owned by the Delivery Owner.

Reopen if:
- Technical Design owns a material matched cold-miss regression, a Tokio budget change, a suspending service cache selection, or a representable cutoff that can diverge from the request's primary deadline.
