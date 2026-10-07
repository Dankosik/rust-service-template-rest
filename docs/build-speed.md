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
  dependency graph. The optional task-local cache described below can reuse
  compiler results without sharing `target/` directories.
- Every Cargo build already uses every core. Concurrent builds beyond what
  memory holds push the machine into swap and slow all of them. Keep one heavy
  Cargo command per checkout; `make check` and `make verify` already take the
  repository's shared validation queue. Wrap independently selected heavy
  commands with `bash scripts/ci/validation-lock.sh -- <command>` so other
  worktrees share the same exclusion. Live registrations wait in FIFO order;
  cancellation removes the waiting ticket, and retrying joins at the tail.
  Across repositories, start from two concurrent builds on a 16 GB workstation
  and adjust from observed swap.
- A finished worktree's `target/` holds 10–20 GB. Removing the worktree
  directory (`wt remove`, `git worktree remove`) deletes it.

Inspect a wait with `bash scripts/ci/validation-lock.sh --status`. The default
wait budget is 900 seconds; `VALIDATION_LOCK_TIMEOUT_SECONDS` accepts a finite
nonnegative override and does not shorten an admitted command's runtime.
Registered descendants and external resources retain the gate until terminal
evidence is available. A quarantine names the unresolved lifetime; restore
that observation capability, then use `--reconcile`. Deleting the gate or
forcing an unlock can overlap live work. Legacy directory clients retain their
old crash/cancel limitation and need a confirmed drain and upgrade for the
new custody guarantee. See the [queue command guide](build-test-and-development-commands.md#shared-validation-queue)
for status, nesting, compatibility and recovery details.

## Workstation setup

These settings change the developer machine, not the repository; the machine's
owner applies them once.

1. **macOS: exempt developer tools from XProtect.** macOS scans every new
   executable on first launch, and each build script and test binary is new
   after a rebuild. Add the terminal and every agent application that runs
   Cargo in System Settings → Privacy & Security → Developer Tools, then
   restart them. This turns the scan off for programs those applications
   launch ([Nethercote][xprotect]).
2. **Shared compiler cache (owner-applied opt-in).** A workstation owner may
   install [sccache][sccache] and add to `~/.cargo/config.toml`:

   ```toml
   [build]
   rustc-wrapper = "sccache"
   ```

   It caches non-incremental units, which are the dependencies; workspace
   crates keep compiling incrementally. `sccache --show-stats` shows the hit
   rate. Repository commands never install sccache or write this setting. An
   inherited wrapper remains caller-owned and is preserved by the default
   build-context mode.
3. **Line-table debug info.** Add to `~/.cargo/config.toml`:

   ```toml
   [profile.dev]
   debug = "line-tables-only"

   [profile.test]
   debug = "line-tables-only"
   ```

   Backtraces keep file and line; a debugger loses local variables. Full
   debug info makes `target/` two to three times larger and slows linking.

## Command-scoped compiler cache

Cargo remains the compiler and configuration authority. `BUILD_CACHE=inherit`
(the default) preserves the caller's direct Cargo executable and supported
wrapper/output configuration; it neither installs nor starts a cache. If that
context cannot be projected safely, the command still runs as inherited but its
context is unknown, so an exact-context verification receipt cannot be reused
or published.

The helper covers the direct Make Cargo build, test, run, Clippy and OpenAPI
leaves. Existing integration/container runners retain their own commands and
custody. Cache statistics establish only the native invocations recorded in the
attempt, never a claim that every selected verification step used the cache.

Set `BUILD_CACHE=sccache` only for an explicit task-local cache run. It requires
the pinned `sccache` executable at `BUILD_CACHE_BIN` or on `PATH`, a task-owned
`BUILD_CACHE_DIR`, and a usable private local server. The helper refuses an
incompatible caller wrapper rather than replacing it. `BUILD_CACHE_SIZE` is an
optional cache cap and defaults to `1G`; it caps that private cache only.
`BUILD_MIN_FREE_BYTES` is optional explicit free-space policy in bytes; the
repository does not guess a reserve. The cache helper owns a foreground local
server for the command, reads bounded native statistics, then shuts it down and
joins it. It does not select a daemon, remote backend, shared cache, global
Cargo configuration, or wrapper replacement.

For a binary provisioned explicitly in this checkout's ignored `.artifacts/`:

```sh
BUILD_CACHE=sccache \
BUILD_CACHE_BIN="$PWD/.artifacts/tools/sccache" \
BUILD_CACHE_DIR="$PWD/.artifacts/compiler-cache" \
make build
```

The cache directory must be new/empty or carry the helper's matching checkout
ownership record; a nonempty unclaimed or other-checkout cache is refused. It
must be separate from Cargo output and Cargo's package cache. For exact context,
output paths must resolve inside the current checkout and cannot alias another
Git worktree. External output ownership and Cargo's `{workspace-path-hash}`
template are currently unprojectable: inherit preserves them with unknown
context, while explicit cache mode refuses them. Ordinary TOML lookup,
recursive `include` files, direct `--config` overrides and supported wrapper/
output environment keys are fingerprinted. Ambiguous special-environment/CLI
overrides and shell snippets remain outside that projection.

Provisioning is separate delivery work, never a side effect of `make build`,
`make test`, or a missing-tool preflight. Use the accepted task-local archive
and digest evidence before placing the executable in the task-owned tool
directory. Re-observe capacity before download and build. Neither the cache cap
nor a previous free-space reading establishes that the build will fit. The
optional pin is `SCCACHE_VERSION` in `tools/versions.env`; tools-check and CI
do not install or require a cache.

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
