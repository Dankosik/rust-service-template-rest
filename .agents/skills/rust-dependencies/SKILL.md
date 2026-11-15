---
name: rust-dependencies
description: "Resolution. Use before adding or upgrading a crate or feature, changing toolchain or edition, or choosing library vs custom code."
---

# Rust Dependencies

**Resolution.** Compare the behavior and maintenance the application would own with the dependency's compatibility, operating, and supply-chain costs. Prefer a maintained crate for a non-trivial general mechanism when it meets the requirement; a smaller dependency count alone does not establish simplicity.

Check the existing owner, standard library, and installed dependencies first. If they leave a gap, evaluate a suitable maintained crate using official documentation for the exact selected version, including defaults, features, and failure semantics. A tiny fixed grammar, an explicit std-only constraint, or a concrete compatibility or operating limit can justify custom code; state that reason rather than treating dependency absence as a benefit by itself.

Scale research to the open choice. A routine library addition needs evidence of behavior, toolchain, license, and trust fit, recorded briefly in the existing change record; compare alternatives in depth when they materially differ in guarantees, maintenance, compatibility, or operating cost, or when an upgrade or new trust boundary makes those consequential. In this service, a new capability records that decision in its accepted artifact under `specs/`, without creating a new specs tree or full architecture study for a routine library. Apply the same scrutiny to custom general mechanisms.

Follow the workspace manifest layout: declare shared versions once, disable default features, and enable required features per crate. Inspect resolved features and duplicate versions when the change can affect compatibility, footprint, or provider behavior; successful resolution alone does not establish runtime delivery. For telemetry changes, the `Cargo.toml` comment on the OpenTelemetry family records how its versions move together.

Separate dependency changes from validation. Initial resolution or an accepted dependency change deliberately updates `Cargo.lock` through Cargo; inspect and include that diff with the declarations, and keep subsequent checks on `--locked`. Never edit lockfiles by hand or bypass integrity checks to repair a download.

Keep the existing toolchain and edition unless changing them is part of the task; the exact pin and workspace minimum version move together in one reviewed change, after which `make modernize-scan SINCE=<previous toolchain>` lists candidates to judge rather than a mandatory gate. A declared minimum version is not proof of compatibility. Review advisories, licensing, and build scripts or procedural macros in proportion to the changed trust surface.

All-features success does not prove the default path, compilation does not prove runtime semantics, and neither a benchmark, a dependency-wide audit, nor a new research artifact is a universal completion gate.
