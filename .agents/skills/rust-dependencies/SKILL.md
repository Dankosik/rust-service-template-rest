---
name: rust-dependencies
description: "Resolution. Use when choosing a Rust library versus local code, or changing a crate, feature, toolchain or edition with consequences for compatibility, maintenance or trust."
---

# Rust Dependencies

Choose the mechanism that meets the current requirement at the lowest justified
implementation and maintenance cost. Reuse the existing seam, standard library
or declared dependency when adequate. A maintained library often beats owning a
general mechanism; a small local policy does not become better merely because
it can be wrapped in a dependency.

## Research the decision-changing uncertainty

Compare alternatives on the behavior the service actually needs: supported
extension points, failure/cancellation behavior, runtime integration and costs
of adapting its API. Verify decisive claims against the resolved version's
official documentation or source. Popularity or a recent release alone does
not establish fitness, and adopting the latest release is not a reason to
upgrade a settled dependency.

Keep investigation proportional to the open choice. Existing accepted research
may already answer it; a feature adjustment need not create a new synthesis or
scratch project. Record a consequential new choice in the service's accepted
decision artifact, including the material limitation and what would reopen it.
When a library meets the requirements only after custom wrappers, compare the
whole ownership burden with the simpler alternative, including future upgrades.

## Inspect the graph Cargo actually builds

A manifest request is not the resolved graph. Feature unification can enable a
capability through another edge, and duplicate versions can split type identity
or process-global state. Inspect the relevant feature and version paths, not
just the line being edited. Disabling defaults can reduce unwanted mechanisms
but can also remove a required transport or runtime capability; preserve the
configurations the service supports rather than proving only all-features.

A minimum Rust version is a declared compatibility promise, not proof that the
resolved dependencies or generated code build there. Toolchain and edition
changes can alter diagnostics, dependency resolution and language behavior.
Preserve observable semantics and assess the supported compiler/configuration
boundary before treating a modernization as mechanical.

Build scripts and procedural macros execute during the build; source, release,
license and advisory provenance therefore matter alongside runtime behavior.
Assess them in proportion to the change and keep integrity checks effective.
A download failure is not a reason to hand-edit a lockfile or bypass verification.

Before adding a dependency or general mechanism, load [Contributing](../../../CONTRIBUTING.md#library-selection)
and [Backend Library Selection](../../../docs/backend-library-selection.md) for
adopted mechanisms and current decisions. [Workspace policy](../../../CONTRIBUTING.md#code-and-workspace)
owns version placement and default features; [AGENTS](../../../AGENTS.md#validation-budget)
owns locked commands and the local validation budget. A dependency choice does
not create its own full-repository completion gate or weaken existing CI gates.
