---
name: rust-dependencies
description: "Resolution. Use before adding or upgrading a crate or feature, changing toolchain or edition, or writing a non-domain parser, encoder, wrapper, or coordination helper instead of a library."
---

# Rust Dependencies

**Resolution.** Weigh the behavior and maintenance the application would own against a crate's compatibility, operating, and supply-chain costs, preferring a maintained crate for non-trivial general mechanisms when it fits the requirement.

**Library versus custom code.** Check the current owner, `std`, and [workspace utility crates](references/workspace-crates.md) (plus sibling modules and optional features already in `Cargo.lock`) first, delegating generic mechanics to the library while keeping domain or protocol policy in the caller. When a gap remains, evaluate candidate crates from official docs and resolved source for the exact version, defaults, features, accepted inputs, cancellation, and failure precedence. Choose custom code when a tiny fixed grammar, an explicit `std`-only boundary (`crates/shared-rules`), or a concrete boundary/operating divergence favors it, pinning that divergence with a test rather than a comment.

**Proportional evaluation.** Match evaluation depth to the decision: record behavior, toolchain, license, and trust fit concisely in the change evidence for routine crates, and compare alternatives in depth when guarantees, maintenance, or trust boundaries materially diverge (recording durable capability choices in `docs/decisions/` or [Backend Library Selection](../../../docs/backend-library-selection.md)). Apply the same bar to custom general machinery.

**Workspace manifest and lockfile.** Declare shared versions once in `Cargo.toml` with `default-features = false`, enable required features per member crate, and check resolved features, duplicate versions, and coupled families (such as OpenTelemetry). Update `Cargo.lock` deliberately through Cargo during dependency changes, inspect and include its diff, and run validation with `--locked` while keeping Cargo's integrity checks intact.

**Toolchain and scope.** Move `rust-toolchain.toml` and workspace `edition`/`rust-version` together in one reviewed change, verifying compatibility beyond the declared minimum version rather than assuming Cargo metadata proves runtime fit. Review advisories, licenses, build scripts, and proc-macros in proportion to trust impact, and verify default-feature and runtime behavior directly where claimed.
