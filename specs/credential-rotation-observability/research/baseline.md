# Decision evidence: refresh observation and authentication proof

Valid as of 2026-10-06. Static evidence at main
`699887b18594088a59bcc23a049d290d089f6da1`; no build, test, live provider or
production inspection was performed in Definition.

## Decision and stopping point

Determine which operational facts are missing and whether real authentication
is already covered. Stop when source identifies the exact success boundary,
existing coverage distinguishes positive and negative authentication, and the
resolved provider interfaces can support the proposed proof. A successful read
or transport operation with authentication disabled falsifies any stronger
authentication claim. Technical Design owns metric shape and harness choices.

## Current facts and implications

| Fact and primary locator | Decision effect and limitation |
| --- | --- |
| [PostgreSQL refresh](../../../crates/infra-postgres/src/credentials.rs), `Refresh::step`, reads every 5 seconds, retains old options on failure, and emits `postgres_password_reloaded` after replacing later connect options. | Installation is observable; no server acceptance follows from this event. Repeated successful reads of unchanged bytes are not rotation. |
| [Valkey supervisor](../../../crates/infra-cache/src/connection.rs), `connect`, `maintain`, `refresh`, rereads at setup and every 5 seconds, with the existing 1-second refresh envelope. `cache_password_reloaded` follows successful AUTH and a non-retired generation; rejected material stays pending. | Preserve this stronger event meaning and the existing recovery. A successful no-change read issues no AUTH and cannot create new authentication evidence. |
| [NATS credentials](../../../crates/infra-messaging/src/credentials.rs), `CredentialsFile::answer`, rereads and signs a challenge. `messaging_credentials_reloaded` observes a changed JWT before server acceptance. | Preparation is not authenticated acceptance. Seed-only differences need no new rotation-detection promise or secret fingerprint. |
| [JWKS owner](../../../crates/infra-bearerauthn/src/refresh.rs), `State`, `KeyStore::new`, `finish`, keeps keys, last-started time and last-result bool; `authn_jwks_refreshes_total` already records finite result/reason. Startup installs admitted keys. `fetch_key_set` rejects parse failures and no-usable-key sets. | Add last successful usable-fetch observability; preserve the existing counter and last-good policy. Fetching the same usable keys is fresh local acquisition evidence, not issuer key generation or revocation evidence. |
| [NATS real suite](../../../crates/infra-messaging/tests/jetstream.rs), `write_creds` and `a_credentials_file_is_read_again_for_a_reconnect`, explicitly uses an unauthenticated broker; it atomically renames the file and observes JWTs on reconnect. | Retain its transport/read proof where useful, but close the missing real broker rejection/acceptance boundary. This is not an atomic-publication gap. |
| [Valkey real suite](../../../crates/infra-cache/tests/valkey.rs), `options`, supplies `password_file: None`. [Protocol coverage](../../../crates/infra-cache/src/tests.rs) includes `a_rotated_password_file_authenticates_the_next_connection`, `reliability_rejected_unchanged_password_recovers_without_traffic`, and secret-safe rejection assertions. | Add real-server file rotation coverage, reuse existing deterministic failure/recovery tests instead of multiplying the suite. |
| [PostgreSQL suite](../../../test/tests/postgres.rs), `a_rotated_password_file_reaches_the_connections_opened_after_it`, and [jobs suite](../../../test/tests/jobs/execution.rs), `x2_a_lost_listener_reconnects_with_the_pools_rotated_password`, already cross actual database authentication. | No new PostgreSQL rotation matrix is justified. Source presence is not a current run receipt. |

## Versioned primary contracts and available reuse

[Cargo.lock](../../../Cargo.lock) resolves async-nats 0.50.0, redis 1.7.1 and
metrics 0.24.6. async-nats uses the repository's same-version patch; its local
source is the resolved authority. [ConnectOptions](../../../vendor/async-nats/src/options.rs)
at `with_auth_callback` and `credentials_file`, and
[connector](../../../vendor/async-nats/src/connector.rs) around its auth callback,
show that the native file builder loads once while the callback supplies each
connection's CONNECT material before the server reply. Keep the supported
callback; replacing it with the file builder loses reread behavior.

The pinned [Compose services](../../../env/docker-compose.yml) are NATS 2.15.0
and Valkey 9.1.2, with image digests. NATS's
[v2.15.0 authentication source](https://github.com/nats-io/nats-server/blob/v2.15.0/server/auth.go)
validates user claims and the nonce signature in operator/JWT mode. An enabled
JWT trust chain, not merely a `.creds` input or a token/password-only server,
is needed to demonstrate this adapter's credential mode.

Valkey's [AUTH contract](https://valkey.io/commands/auth/) returns success only
for an accepted username/password on the current connection. Its
[9.1.2 implementation](https://github.com/valkey-io/valkey/blob/9.1.2/src/acl.c),
`ACLCheckUserCredentials`, `checkPasswordBasedAuth` and `authCommand`, confirms
that distinction. A working old session is insufficient evidence of successful
replacement reAUTH.

Official versioned docs.rs URLs for async-nats 0.50.0, redis 1.7.1 and metrics
0.24.6 were unavailable through web retrieval. No API claim depends on those
failed fetches: the resolved NATS source and current adapter calls are enough
for Definition. Reopen supporting Research if Design needs an unverified API.

Existing [messaging runner](../../../scripts/ci/test-integration-messaging.sh)
and [cache runner](../../../scripts/ci/test-integration-cache.sh) accept supplied
test endpoints or use the existing pinned Compose services. Their current
default services do not by themselves provide authenticated rotation fixtures.
Design must close feasible reuse/isolation for those fixtures; Definition does
not authorize a new stack, runner or live service.

Repository metrics/tracing and the existing adapters are the sufficient live
solution family. A new metrics SDK, credential watcher framework, client
replacement or cloud SDK would duplicate existing owners without answering an
additional accepted question. No dependency upgrade is justified. Exact
instrument names and placement remain a Technical Design decision.

## Prior recommendation and uncertainty disposition

Historical PR #247 at `01ebf13070be8a1542413e83ca902c8138c17383`,
`specs/credential-refresh-hardening/spec.md`, deferred new metrics until a named
operator question required them; it separately deferred TLS reload and stale-key
cutoffs. The new accepted intent opens only observability and authenticated
NATS/Valkey regression coverage. Its jitter and broader guide changes are not
part of this base or imported into this task.

All table entries above are source facts, not measured runtime claims. The
availability of a particular authenticated test fixture is still unmeasured;
the standard suites/runners exist. Refresh the affected baseline if main or
resolved dependency/provider pins change, especially if #247 lands. Reopen
Specification only when that delta changes observable meaning; use Technical
Design for mechanism or fixture isolation and Implementation for concrete
cases and run receipts.
