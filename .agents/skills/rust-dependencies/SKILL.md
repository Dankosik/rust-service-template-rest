---
name: rust-dependencies
description: "Use before adding or upgrading a crate or feature, changing the toolchain pin or edition, or choosing between a library and template-owned code."
metadata:
  invocation: model
  kind: method
---

# Rust Dependencies

Prefer a **maintained crate that already solves the problem idiomatically**
over template-owned code, and prove the choice before declaring it.

Before a new crate or a new mechanism, survey the candidates against the
actual requirement and record the comparison under
`specs/<topic>/research/synthesis.md` as the [roadmap](../../../docs/roadmap.md#working-rules-for-every-stage)
requires: latest version and release date from crates.io, maintenance signal
from the repository, the exact behavior verified in source or a scratch
project outside the repository, known issues, and what the crate does not do.
The Go template supplies the problem and its reasons, never the shape of the
solution; when Rust solves it differently, record the deviation.
Template-owned code exists only for a gap the synthesis names.

Declare the version once in `[workspace.dependencies]` with
`default-features = false`, enable features per crate, and keep every Cargo
invocation `--locked`; a lockfile change ships with the change that caused
it. Check what actually resolved: `cargo tree -d` for duplicate versions,
`cargo tree -e features -i <crate>` for features another dependency turned on
(a second `opentelemetry` minor or an unexpected `opentelemetry-otlp`
client feature silently disables export). The OpenTelemetry family moves
together, with `tracing-opentelemetry` one minor ahead.

The toolchain is pinned exactly in `rust-toolchain.toml` and mirrored by
`rust-version` in `Cargo.toml`; bump both in one reviewed change after
`make check` passes on the new compiler, and let clippy's new suggestions
land in that change or a follow-up, not silently. Edition 2024 forms
(let-chains, `cast_signed`) are preferred where they clarify; a modernization
must preserve behavior and error identity.

Review maintenance, licensing, and advisory evidence in proportion to the
change; a build script or procedural macro joins the trust analysis. Never
edit `Cargo.lock` by hand or bypass integrity checks.

Complete when the crate is declared once, its features are explicit, the
resolved tree has been inspected, the synthesis names the decision, and
`make check` passes.
