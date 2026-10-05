# T6 — Close native publication ownership and removable source custody

Outcome:
A messaging resource bounds source/outbox/DLQ outstanding publication through native admission and ACK cleanup, with correct receiving limits and the same patched dependency graph in every retained messaging profile.

Consumes:
- [S5](../spec.md#s5-give-publication-its-own-finite-admission), [selected design](../design/selected-design.md), specifically S5 native ownership, separate reserves and dependency/delivery custody.
- [R6–R8/R9 exact ownership](../design/ownership.md#files); prepared-event/outbox contracts are closed inputs independent of T4 implementation.
- Same-version async-nats 0.50.0 and upstream draft1629 commit 7db17cf15830a1a65e7ba73cecda65aee72b1ea7; final [Technical Design review](../technical-design-review.md#bounded-final-source-selection-delta). No further mechanism selection or upstream search.

Provides:
- One complete S5 implementation: native adaptive pruning and retained ACK borrow; immediate native P admission; source/outbox receiving M/final-H checks; DLQ settlement compatibility; verified-source custody, deliberate lock source change, removable messaging profile and truthful messaging guide.

Boundary:
Keep native parser/transport/recovery. P=floor(64 MiB/(M+8192)); finalize tracing and expected-stream before checking the source/outbox message. DLQ shares P but keeps broker-dependent wire ceiling and confirmation-before-source-ACK. Adopt only handler-local adaptive pruning from the fixed upstream commit plus native ACK ownership repair; no custom manager, dirty/guard receiver variant, new task/channel or dependency upgrade. Preserve finite stale metadata as defined by design, not a false live-P map bound. Patch and its profile/source removal are one unit: no partially consumable vendor-only or admission-only task.

Mutable owners:
- Dependency source: vendor/async-nats pristine 0.50.0 package, licenses/normalized manifest and PATCHES.md; only src/lib.rs and src/jetstream/context.rs carry the selected production repair and adjacent native regression cases. Record archive checksum, pristine/patched hashes, exact diff, upstream reference and removal condition; unchanged package source remains byte-identical.
- infra-messaging publication: crates/infra-messaging/src/messaging.rs, producer.rs and consumer.rs; their adjacent tests and existing tests/jetstream.rs and tests/idle_pull.rs. T4's prepared.rs and wire_compat.rs stay disjoint.
- Root dependency/source/profile delivery: Cargo.toml, Cargo.lock, .dockerignore, build/docker/Dockerfile, scripts/lib/template_profiles.json, scripts/ci/changed-surfaces.sh.
- docs/durable-messaging.md: receiving bounds, native command/subscription/write/acker/request reserves, cancellation cleanup lifetime, DLQ broker-sized window and caller-owned prepared backing.

Exclusive locks:
- Root dependency graph and Cargo.lock source resolution; one deliberate supported lockfile update in this unit, never as validation side effect.
- Messaging profile/source custody, Docker source routing and changed-surface classification.
- Publication/native source scopes above; separate lanes inside this unit may code concurrently only after the Lead assigns disjoint exact files and locks. In particular manifest/lock/profile custody has one writer; ACK/context and handler/lib source lanes must not both own the vendor directory wholesale. All lanes join before Implemented.

Final validation:
- Claim: native ACK/request lifetime and public adapter parity uphold S5; source integrity and unchanged version/dependency/features are established; both retained and messaging-absent profile graphs route/remove the source correctly.
- Checks: one consolidated local Completion plus existing dependency, classification, initializer/profile and image/optional-integration CI gates. Executor chooses native/adapter cases and commands; no new infrastructure matrix or repeated profile builds. Dependency Review/cargo-deny and existing gates remain; no waiver.
- Observable: pre-dispatch rejection and unchanged ambiguity/recovery/settlement; finite live ownership versus separately bounded stale metadata; Cargo uses the intended path source at 0.50.0 and messaging removal removes patch/exclusion/copied source together.

Reopen if:
Fixed source or native lifetime proof contradicts design, or declarative profile removal cannot represent the edge: Technical Design R6–R8. Do not silently alter initializer implementation or choose another source mechanism. Changed publication/DLQ behavior: Definition.
