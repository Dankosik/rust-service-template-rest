# Credential and trust baseline

Status: ready. Evidence inspected 2026-10-05. This is supporting Definition
research, not runtime proof or a standalone Research completion claim.

## Identity and scope

Base: `5927ffbba351af2f7fb8635316bbfa4ae5b31da6`, remote main supplied by the
continuation owner. Worktree:
`/Users/daniil/.codex/worktrees/credential-refresh-hardening/rust-service-template-rest`;
branch `codex/credential-refresh-hardening-20261005`. The worktree was clean
before Definition artifacts were created. No build, tests, real credentials,
external mutation, or deployed service were used in this phase.

The coordinator's earlier research supplied the cross-provider inventory.
The decision-changing facts below were checked against source or existing
contract owners in this base. Prior dirty-main hotpath findings are not defects
of this candidate. In particular, this base already has the OAuth driver,
failure suppression, provider admission, checked expiry, and Redis supervisor.
Do not transplant the other branch or turn its regressions into new work here.

## Current facts and their decision effect

| Surface | Evidence and current meaning | Decision effect / reopen |
| --- | --- | --- |
| Configuration | [Architecture](../../../docs/repo-architecture.md), [configuration source policy](../../../docs/configuration-source-policy.md): typed configuration is an immutable startup snapshot. Environment and `--secrets-dir` provide input, not a general watcher. Named path readers have separate ownership. | Preserve. Clarify reload scope. Reopen for a consumer needing a particular mutable key. |
| PostgreSQL password | [credentials.rs](../../../crates/infra-postgres/src/credentials.rs), `refresh_password_periodically`, `Refresh::step`, `read`: immediate first read then 5 s cadence; `set_connect_options`; failed reads preserve previous options with one warning per outage. Read has no explicit deadline or size bound. [dsn.rs](../../../crates/infra-postgres/src/dsn.rs) admits the initial file and keeps identity fields fixed. | Correct overclaimed cutover guarantees; retain last-good policy. File timeout/size machinery has no established untrusted-file boundary or incident requiring a new policy here. Reopen for a real source latency/size constraint. |
| PostgreSQL sessions | [Persistence](../../../docs/architecture/persistence.md) calls 30 min a connection lifetime. [jobs claim owner](../../../crates/infra-jobs/src/claim.rs), `listener_pool`, `follow_connect_options`: LISTEN uses a separate pool with `max_lifetime(None)` and follows engine options before reconnect. SQLx 0.9.0 checks age on pool return and idle maintenance, not by forcibly interrupting checked-out sessions. | Remove implication that 30 min is a hard revocation deadline; document long-lived LISTEN. No pool lifecycle change. |
| Redis credentials | [Cache guide](../../../docs/cache.md), [connection owner](../../../crates/infra-cache/src/connection.rs): 5 s refresh on an established connection; file read and AUTH share 1 s; only accepted AUTH updates accepted bytes; rejected unchanged bytes retry. Read failure can preserve a usable old connection. Reconnect reads current file within 1 s, retries with bounded jittered backoff. | Preserve existing implementation and 7 s retained-connection / 11 s reconnect conditional recovery bounds. Do not replace with a streaming credentials abstraction. |
| NATS credentials | [Messaging::connect](../../../crates/infra-messaging/src/messaging.rs) uses `authenticated` and `with_auth_callback`; [credentials owner](../../../crates/infra-messaging/src/credentials.rs) rereads one `.creds` tuple on challenge. [Messaging guide](../../../docs/durable-messaging.md) documents startup admission, reconnect reread and retry after malformed file. | Preserve atomic tuple and per-challenge reading. No proactive expiry-switching owner is required by a current consumer. |
| NATS reconnect | Locked async-nats 0.50.0 `src/options.rs`, `ConnectOptions::default`: no maximum reconnect count, default callback. `src/connector.rs::reconnect_delay_callback_default`: attempts 0/1 are immediate; subsequent delays are `min(2^(attempts-1) ms, 4 s)` without jitter. Template does not override callback. | Add bounded per-attempt spread using a supported extension point; retain retry ownership and ceiling. Reopen on dependency behavior change. |
| OAuth service token | [lib.rs](../../../crates/infra-oauth2-client-credentials/src/lib.rs), `RefreshDriver::run`, `reusable_service_token`, `refresh_ahead`, `into_token`; [guide](../../../docs/outbound-machine-authentication.md), [decisions](../../../docs/outbound-machine-authentication-decisions.md). Driver owns one queued/pending refresh; background 5 s includes lock wait; default provider limit 32; 1 s completed failure suppression; reuse ends 10 s before monotonic expiry; deterministic lead `min(5 min, reuse lifetime/4)` and 30 s refresh retry. | Preserve lifecycle, bounds and authority; spread eligibility and retry times. Cache hit triggers refresh; there is no autonomous token timer to add. |
| OAuth expiry and invalidation | Same owner: no `expires_in` means originating call only; invalid/overflowing or expired lifetime rejects dispatch. 401 invalidates only the exact used token when at least 30 s old; no resource replay; 403 preserves token. Exchange tokens have no background refresh. | Already adequate. Reuse existing coverage; do not add duplicate fixes or broaden jitter to exchange traffic. |
| JWKS | [refresh.rs](../../../crates/infra-bearerauthn/src/refresh.rs), `run_refresh_worker`, `KeyStore`: deterministic 15 min interval; unknown-key 30 s cooldown; one coalescing owned worker; failed fetch/parse/no usable keys preserves last-good set. [authentication](../../../docs/authentication.md), [runtime lifecycle](../../../docs/architecture/runtime-lifecycle.md) explicitly reject cached-key revocation semantics. Known kid with bad signature is not a refresh trigger. | Spread periodic work while preserving cooldown, coalescing, expiry and last-good semantics. No invented cached-key expiry. |
| Fixed authentication policy | Outbound signer key/kid and inbound introspection secret are admitted into existing constructed owners; issuer, algorithms, audience and discovery choices are startup policy. JWT `exp` remains checked, with existing 30 s leeway. | Document restart and provider overlap; rotating signing material does not itself revoke previously issued tokens. |
| TLS material | [gRPC TLS owner](../../../crates/infra-grpc/src/tls.rs) constructs an immutable rustls server config; [cache guide](../../../docs/cache.md) says client certificate/key are read once. [outbound HTTP](../../../crates/infra-outbound-http/src/lib.rs), `tls_config`: process `OnceLock`, shared verifier and session cache; rebuilding an HTTP Client does not reload the process configuration. [OTLP policy](../../../docs/configuration-source-policy.md) owns the constructed exporter client. | Document each material owner; promise no universal reload. Linux outbound trust-store changes require process replacement. Open sessions and TLS resumption are separate from a new full handshake. |

SQLx's resolved source is available under Cargo's registry as `sqlx-core-0.9.0`
(`src/pool/connection.rs::return_to_pool`, `src/pool/inner.rs` idle maintenance,
`src/pool/mod.rs::set_connect_options`) and `sqlx-postgres-0.9.0`.
The current template enables rustls with bundled WebPKI roots. Its TLS adapter
reads an explicit root-cert path during handshake and adds those roots; a file
does not imply replacement of all bundled trust. `sslmode=require` supplies
encryption without the `verify-full` identity guarantee. Technical Design must
retain source-qualified wording for provider-specific trust-store details.

## External file owner and library choices

Existing clients remain the solution family. No new credential framework or
replacement driver is justified by this outcome. async-nats 0.50.0 offers
`reconnect_delay_callback` directly; its `credentials_file` builder reads once
and calls `credentials`, so it is not a replacement for the callback here.
The installed crate's `src/options.rs` is the version-matched official API
documentation; docs.rs pages for the resolved future versions were unavailable
through the browser during this phase. SDK source was used instead.

[Kubernetes Secret documentation](https://kubernetes.io/docs/concepts/configuration/secret/#using-secrets-as-files-from-a-pod)
states mounted Secret propagation is eventual and `subPath` mounts do not
receive updates. This adds upstream publication latency; a local polling
interval is not an end-to-end cutover SLA.

[Vault Agent templates](https://developer.hashicorp.com/vault/docs/agent-and-proxy/agent/template)
can render and renew files outside the application. That is compatible with an
existing file reader only when the rendered format and identity match its
contract. A password-only reader cannot follow a concurrently changing username.
No Vault deployment or direct secret-manager SDK integration is selected.

Earlier research also identified AWS S3's existing SDK workload-identity chain
and SPIFFE as a separate trust-domain adoption. These remain preservation and
defer dispositions respectively, not new runtime claims or implementation work.
Reopen either only for an actual consumer whose identity contract requires it.

## Limits and falsifiers

This is source/contract evidence, not measured provider behavior. No live
cutover, token lease, revocation delay, cluster propagation time, filesystem
stall, or fleet-load benefit was measured. Inspecting deterministic schedules
supports a synchronization risk, not a quantified outage claim. The meaningful
falsifier for the runtime delta is scheduling that remains identical across
independent owners or exceeds the accepted ranges, while existing deadline,
expiry, cancellation or coalescing guarantees regress.

Reopen the smallest affected fact if the base, dependency version, profile,
trust-store implementation, or consumer requirement changes. The behavior
decision is [spec.md](../spec.md); Technical Design owns mechanism and placement.
