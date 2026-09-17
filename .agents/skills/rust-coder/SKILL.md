---
name: rust-coder
description: "Use to implement an authorized Rust change in this workspace with accepted behavior, including its tests and cleanup."
metadata:
  invocation: model
  kind: method
---

# Rust Coder

Implementation starts at the **earliest valid owner** and ends at the accepted
observable behavior.

`criterion -> earliest owner -> existing path -> far side -> proof -> cleanup`

Bind each accepted criterion to a current owner before editing. Owners are
crates: `crates/service` composes and owns lifecycle; `crates/config` owns
the typed snapshot; `crates/health` owns readiness; `crates/infra-*` adapt
one transport or provider; `crates/<feature>` will own business behavior and
depends on no transport or provider crate. Extend the existing policy path
instead of adding a parallel one: a new operation joins `infra_http::router`
and its feature handler, not the hardened chain; a new key joins its section
file in `crates/config/src`; a new background task joins the `TaskTracker`
in `bootstrap` with a child `CancellationToken`.

Inspect the far side of every touched boundary: callers, the error identity
they match on, drop order, the shutdown stage that must now wait, and the
config validation that must now hold. A new helper, layer, trait, or crate
must carry a current constraint, variation, or dependency direction;
otherwise keep the behavior local. Before adding a crate or feature, apply
`rust-dependencies`.

Choose tests while writing the code, beside the owner under `#[cfg(test)]`,
from accepted behavior rather than the implementation's current output; state
what wrong behavior each new test rejects. Bound every wait; join every task.
Consult `rust-testing` only for a non-obvious proving layer.

Finish with `cargo fmt`, `make build`, and `make test-package PKG=<crate>` for
each changed crate (`make test` when `Cargo.toml` or `Cargo.lock` changed).
`make lint` runs clippy at `pedantic` with warnings as errors; fix the finding
or add a site-local `#[allow]` with the reason, never a workspace-wide
relaxation. Every Cargo command runs `--locked`; a lockfile change is part of
the change. Delete each new seam whose removal preserves accepted behavior.

Complete when each criterion has a causal edit or a grounded no-change
disposition and its proof, no superseded path remains, and no implementation
choice is left open. The active task owns validation timing; do not add
checks for confidence beyond [AGENTS.md](../../../AGENTS.md#validation-budget).
