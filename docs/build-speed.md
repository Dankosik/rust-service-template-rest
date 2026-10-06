# Build Speed

Read this when a build or test wait slows iteration, when a new worktree or a
parallel session starts building, or when a workstation is set up for this
repository. [AGENTS.md](../AGENTS.md#validation-budget) owns what completion
requires; this file owns reaching it with less waiting and never weakens that
route.

## Iteration loop

- Type-check while editing with `make lint-changed PKGS="<crates>"` or
  `cargo check -p <crate> --all-targets --locked`. Both stop before code
  generation and linking, typically two to three times faster than a build
  ([corrode][corrode]).
- Run the completion commands once, after the change type-checks, scoped by
  the crates `scripts/ci/affected-crates.sh` prints. The workspace `make test`
  is for the fallback the planner names.
- While repairing one failing test, run only it:
  `cargo test -p <crate> --locked <test name>`.
- Build `--release` only when the task needs a release artifact. The release
  profile uses fat LTO over one codegen unit and costs minutes per build; the
  measurement is beside `[profile.release]` in `Cargo.toml`.
- Run a Cargo command that can outlast the tool timeout in the background and
  wait for its exit. A killed build discards its work and the retry repeats it.
- Use `cargo clean` only for proven artifact corruption; it forces every
  dependency to compile again.

## Parallel sessions and worktrees

- Each worktree keeps its own `target/`, the Cargo default. Do not point
  concurrently building checkouts at one `CARGO_TARGET_DIR`: Cargo's lock
  serializes them and overlapping writes corrupt artifacts
  ([cargo#16804][cargo-16804]).
- A new worktree starts with an empty `target/` and compiles the whole
  dependency graph. The shared compiler cache under
  [Workstation setup](#workstation-setup) turns that into cache hits.
- Every Cargo build already uses every core. Concurrent builds beyond what
  memory holds push the machine into swap and slow all of them. Keep one heavy
  Cargo command per checkout; `make check` and `make verify` already take the
  repository's validation lock. Across repositories, start from two concurrent
  builds on a 16 GB workstation and adjust from observed swap.
- A finished worktree's `target/` holds 10–20 GB. Removing the worktree
  directory (`wt remove`, `git worktree remove`) deletes it.

## Workstation setup

These settings change the developer machine, not the repository; the machine's
owner applies them once.

1. **macOS: exempt developer tools from XProtect.** macOS scans every new
   executable on first launch, and each build script and test binary is new
   after a rebuild. Add the terminal and every agent application that runs
   Cargo in System Settings → Privacy & Security → Developer Tools, then
   restart them. This turns the scan off for programs those applications
   launch ([Nethercote][xprotect]).
2. **Shared compiler cache.** Install [sccache][sccache] and add to
   `~/.cargo/config.toml`:

   ```toml
   [build]
   rustc-wrapper = "sccache"
   ```

   It caches non-incremental units, which are the dependencies; workspace
   crates keep compiling incrementally. `sccache --show-stats` shows the hit
   rate.
3. **Line-table debug info.** Add to `~/.cargo/config.toml`:

   ```toml
   [profile.dev]
   debug = "line-tables-only"

   [profile.test]
   debug = "line-tables-only"
   ```

   Backtraces keep file and line; a debugger loses local variables. Full
   debug info makes `target/` two to three times larger and slows linking.

## Code structure

- Add a test for an existing area as a module of that area's test binary
  (`tests/<area>/main.rs` with `mod <case>;`). Each top-level `tests/*.rs` file
  is a separate binary linked against the whole dependency graph; add one only
  for a separately selected area or harness.
- Measure before restructuring for build time: `cargo build --timings --locked`
  writes `target/cargo-timings/cargo-timing.html` with each crate's duration
  and the critical path. A crate boundary change follows
  [Repository Architecture](repo-architecture.md).

## Not adopted

| Option | Reason |
| --- | --- |
| Cranelift codegen backend | Requires nightly; the toolchain is pinned stable |
| mold or wild linker | Linux only |
| lld on macOS | Xcode's default linker is already parallel; no measured need |
| One `CARGO_TARGET_DIR` for all worktrees | Lock serialization and artifact corruption under parallel sessions |
| cargo-nextest, cargo-hakari | No measurement shows the test runner or feature unification as the bottleneck |

Upstream compiler work, such as the parallel frontend and relink-don't-rebuild
in the Rust project's [fast-builds goals][fast-builds], arrives through the
reviewed toolchain bump.

[corrode]: https://corrode.dev/blog/tips-for-faster-rust-compile-times/
[cargo-16804]: https://github.com/rust-lang/cargo/issues/16804
[xprotect]: https://nnethercote.github.io/2025/09/04/faster-rust-builds-on-mac.html
[sccache]: https://github.com/mozilla/sccache
[fast-builds]: https://goals.rust-lang.org/2026/roadmap-fast-builds.html
