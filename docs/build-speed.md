# Build Speed

Read this when compile times slow iteration or workstation/container caching is
configured. [AGENTS.md](../AGENTS.md#validation-budget) owns completion criteria,
and `Cargo.toml` documents `[profile.dev]` and `[profile.release]` settings.

## Local iteration and worktrees

- Run one failing test directly via `cargo test -p <crate> --locked <test>` and
  avoid `--release` unless measuring or building final images (fat single-CGU
  LTO takes minutes). For local variable inspection in a debugger, point
  `CARGO_TARGET_DIR` at a separate directory with `CARGO_PROFILE_DEV_DEBUG=full`
  and `--config 'profile.dev.package."*".debug="full"'`.
- Keep each Git worktree's `target/` separate to avoid Cargo lock serialization
  and artifact corruption ([cargo#16804][cargo-16804]).
- Add new integration cases as submodules under `tests/<area>/main.rs` rather
  than separate `tests/*.rs` binaries that re-link the dependency graph; profile
  compile bottlenecks with `cargo build --timings --locked`.

## Workstation setup

1. **macOS XProtect:** Add terminal and agent apps running Cargo under System
   Settings → Privacy & Security → Developer Tools and restart them to skip
   per-binary scans ([Nethercote][xprotect]).
2. **Optional `sccache`:** Set `rustc-wrapper = "sccache"` under `[build]` in
   `~/.cargo/config.toml` to share compiled dependency artifacts across
   worktrees (`sccache --show-stats`).

## Deployment builds

- **Railway builders vs CLI upload:** Railway compiles `build/docker/Dockerfile`
  natively [at no build charge](https://docs.railway.com/pricing/plans). For
  authorized `railway up` uploads, set `ARG VCS_REF` to the exact commit SHA.
- **Private crates without build secrets:** Because BuildKit logs `ARG` values
  and Railway has no secret mounts, vendor private crates only via
  `cargo vendor --locked --versioned-dirs vendor-private`, wire
  `vendored-sources` in `.cargo/config.toml`, admit both in `.dockerignore` and
  `build.watchPatterns`, and copy them into the `cooked` stage.
- **Local `linux/amd64` builds on Apple Silicon:** Pass
  `RUNTIME_IMAGE_CACHE_FROM` / `RUNTIME_IMAGE_CACHE_TO` (`type=local`) to reuse
  the `cooked` layer, or cross-compile from `$BUILDPLATFORM` with
  `x86_64-unknown-linux-gnu` cross toolchains in a workstation-only Dockerfile.

## Rejected options

- **Cranelift codegen:** Requires nightly; toolchain is pinned stable.
- **`mold` / `wild` / macOS `lld`:** Linux-only or no measured gain over Xcode's parallel linker.
- **Shared `CARGO_TARGET_DIR`:** Lock contention and cache corruption ([cargo#16804][cargo-16804]).
- **`cargo-hakari`:** Feature unification not measured as a bottleneck.
- **Remote `sccache` in image builds / external remote builders:** Image builds hold no credentials, and Railway already builds natively for free.

[cargo-16804]: https://github.com/rust-lang/cargo/issues/16804
[xprotect]: https://nnethercote.github.io/2025/09/04/faster-rust-builds-on-mac.html
