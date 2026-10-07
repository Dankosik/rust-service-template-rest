# T4 — Bounded NATS recovery inside the configured trust boundary

Outcome:
Admission/reconnect attempts spend one five-second server budget, graceful or
forced shutdown preserves observed-completion truth, and discovered recovery
destinations come only from the selected authenticated/trusted INFO boundary.

Consumes:
- [Specification](../spec.md#nats-bounded-recovery) and [discovery behavior](../spec.md#nats-discovery-trust-and-compatibility).
- [Design](../design/transport.md#nats-attempt-and-recovery-ownership), [native close](../design/transport.md#closing-the-native-runner), [discovery matrix](../design/transport.md#nats-discovery-and-configuration) and [source custody](../design/transport.md#source-and-file-custody).
- [Execution boundary](execution-boundary.md) — shared carrier writer and final assembly.

Provides:
- Pinned async-nats 0.50.0 native repair, complete adapter/config/worker consumption, profile/delivery custody, tests and compatibility guidance.

Boundary:
Vendor published source with verified checksum/provenance, exact diff and
retirement. Change native runtime only in connector.rs, client.rs, lib.rs and
tls.rs as designed: absolute attempt deadline/candidate shares; blocking root
load awaited inside it; close-request and observed-completion watch ownership.
Keep raw Subscriber and unsubscribe-on-drop ownership, no sender held by runner,
drop resources before completion, preserve Closed/disconnected reporting.
Use completion receipts in close_client; forced/unobserved close stays degraded
and never starts a fresh wait budget. Keep retry pacing, cached readiness,
durable consumer/settlement/DLQ/publish finality and credential/trust ownership.

Carry `messaging.tls_first`, default false and APP__MESSAGING__TLS_FIRST through
typed config, environment loading, MessagingOptions and all constructions.
Reject TLS-first with any plaintext seed in config and direct admission.
Implement the complete accepted discovery matrix and ordinary-TLS migration
guidance without downgrade, hostname blacklist or TLS bypass.

Mutable owners:
- New `vendor/async-nats/` source archive, native tests and PATCHES.md.
- `crates/infra-messaging/src/messaging.rs` and its provider tests/fixtures; `crates/config/src/messaging.rs`, messaging fields in existing loader/config examples; MessagingOptions construction sites and messaging composition in `crates/jobs-worker/src/bootstrap.rs`.
- Messaging slices of root Cargo manifests/lock, Docker context/image sources, `scripts/lib/template_profiles.json`, changed-surface classifier and existing initializer/classifier fixtures.
- `docs/durable-messaging.md`, messaging assertions in `docs/configuration-source-policy.md`, `docs/architecture/runtime-lifecycle.md` and existing config examples.

Exclusive locks:
- `dependency-profile-delivery`, shared with T5: root Cargo.toml/Cargo.lock, .dockerignore, build/docker/Dockerfile, template_profiles.json, changed-surfaces.sh and shared initializer/classifier fixtures. One assembly writer holds these through its task slice; all other writers exclude them.

Final validation:
- Claim: Native attempts and termination remain bounded with honest completion, same-owner recovery and trusted discovery; all retained/removed messaging profiles carry the patch coherently.
- Checks: Matching assembled build/tests/docs and selected delivery/dependency/classifier route; CI owns selected heavy integration/image/initializer gates. No additional local gate.
- Observable: Pending DNS/candidate/handshake cannot retain an unbounded owned attempt; restored broker remains usable; force-close ends native recovery without false graceful success; raw Subscriber retains native lifetime; untrusted pre-TLS INFO cannot extend ordinary-TLS seeds, while TLS-first/trusted-network discovery remains admitted. Evidence stays at its exercised boundary.

Reopen if:
Native source cannot provide accepted budget/ownership semantics (Technical
Design); compatibility, trust boundary or selected behavior must change
(Definition). A mechanical construction-site/fixture locator needs no reopen.
