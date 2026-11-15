# Build Speed

Read this when builds or tests slow iteration (including deployment images), a
worktree starts building, or a workstation is set up.
[AGENTS.md](../AGENTS.md#validation-budget) owns completion rules; this guide
reduces wait times without weakening them.

## Iteration loop

- Type-check while editing with `make lint-changed PKGS="<crates>"` or
  `cargo check -p <crate> --all-targets --locked` (skips codegen/linking).
- Run completion commands once after type-checking, scoped by
  `scripts/ci/affected-crates.sh` unless the planner falls back to `make test`.
- Debug one failing test directly: `cargo test -p <crate> --locked <test name>`.
- Build `--release` only when needed; `[profile.release]` uses single-CGU fat
  LTO and takes minutes. Run long Cargo builds in the background to avoid
  discarding partial work; use `cargo clean` only for proven corruption.
- `[profile.dev]` keeps workspace line tables without dependency debuginfo
  (~30% less cold CPU, ~50% smaller `target/`). For a debugger, use a separate
  `CARGO_TARGET_DIR` with `CARGO_PROFILE_DEV_DEBUG=full` and
  `--config 'profile.dev.package."*".debug="full"'`.

## Parallel sessions and worktrees

- Keep each worktree's own `target/`; sharing `CARGO_TARGET_DIR` serializes
  Cargo's lock and risks artifact corruption ([cargo#16804][cargo-16804]).
  Removing a worktree deletes its 10–20 GB `target/`.
- Keep one heavy Cargo/container command per checkout (start from at most two
  concurrent builds on 16 GB RAM). Wrap independent heavy commands with
  `bash scripts/ci/validation-lock.sh -- <command>`.
- Check lock ownership with `bash scripts/ci/validation-lock.sh --status`
  (override 900 s timeout via `VALIDATION_LOCK_TIMEOUT_SECONDS`;
  [`scripts/ci/validation-lock.py`](../scripts/ci/validation-lock.py)).

## Workstation setup

1. **macOS XProtect:** Add terminal and agent apps running Cargo to System
   Settings → Privacy & Security → Developer Tools and restart them to stop
   per-binary scans ([Nethercote][xprotect]).
2. **Shared compiler cache (optional):** Install [sccache][sccache] and set
   `rustc-wrapper = "sccache"` under `[build]` in `~/.cargo/config.toml` to
   cache dependency crates (`sccache --show-stats`).

## Deployment builds

- **Build on Railway:** Railway's builders compile `build/docker/Dockerfile`
  [at no build charge](https://docs.railway.com/pricing/plans) (3.1–5.9 min
  warm vs 5.5–8.2 min cold without `IMAGE_TESTS=unit`).
- **`railway up` deploys what it builds:** [Authority](../AGENTS.md#authority)
  applies. Write the commit into both `ARG VCS_REF` defaults (`railway up` sets
  no `RAILWAY_GIT_COMMIT_SHA`) and record the commit and directory digest;
  unchanged watched paths can `SKIP`.
- **Keep credentials out of builds:** Railway lacks secret mounts and BuildKit
  logs `ARG` values in `RUN`. Vendor private deps with
  `cargo vendor --locked --versioned-dirs vendor-private`, keep only private
  packages, add the printed git-source/`vendored-sources` stanzas to
  `.cargo/config.toml`, allow `.cargo/config.toml` and `vendor-private/` in
  `.dockerignore`, copy both into `cooked`, and add both to `build.watchPatterns`.
- **Cross-build on arm64 without emulation:** For `linux/amd64`, use a
  workstation-only Dockerfile variant with `FROM --platform=$BUILDPLATFORM`,
  `rustup target add x86_64-unknown-linux-gnu`,
  `gcc-x86-64-linux-gnu`/`g++-x86-64-linux-gnu`/`libc6-dev-amd64-cross`, and
  `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER` / `CC_*` / `CXX_*` / `AR_*`
  (16.5 vs 31 emulated CPU-minutes) via
  `bash scripts/ci/validation-lock.sh -- docker buildx build --platform linux/amd64 -f <variant> .`.
- **Keep the cooked layer locally:** Pass `RUNTIME_IMAGE_CACHE_FROM` and
  `RUNTIME_IMAGE_CACHE_TO` (e.g., `type=local` under `.artifacts/`)
  (31 vs 175 emulated CPU-minutes on one-line rebuilds).
- **Fat-LTO floor and lock:** Each binary ends with single-threaded fat LTO;
  image builds share the validation lock. Compare in-container CPU time rather
  than wall time on busy hosts.

## Code structure

- Add tests as modules under `tests/<area>/main.rs` (`mod <case>;`) rather than
  extra `tests/*.rs` binaries that re-link the dependency graph.
- Profile compile critical paths with `cargo build --timings --locked`
  (`target/cargo-timings/cargo-timing.html`) before restructuring crates under
  [Repository Architecture](repo-architecture.md).

## Not adopted

| Option | Reason |
| --- | --- |
| Cranelift backend | Requires nightly; toolchain is pinned stable |
| mold / wild | Linux only |
| lld on macOS | Xcode linker is already parallel; no measured need |
| Shared `CARGO_TARGET_DIR` | Lock serialization and artifact corruption |
| cargo-hakari | Feature unification not measured as bottleneck |
| Remote sccache in image builds | Railway builds hold no credentials ([Deployment builds](#deployment-builds)) |
| Hosted remote builder | Railway builders already compile natively at no build charge |

Upstream compiler improvements ([fast-builds goals][fast-builds]) arrive via
reviewed toolchain bumps.

[cargo-16804]: https://github.com/rust-lang/cargo/issues/16804
[xprotect]: https://nnethercote.github.io/2025/09/04/faster-rust-builds-on-mac.html
[sccache]: https://github.com/mozilla/sccache
[fast-builds]: https://goals.rust-lang.org/2026/roadmap-fast-builds.html
