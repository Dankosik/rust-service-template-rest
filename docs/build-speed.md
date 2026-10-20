# Build Speed

Read this when a build or test wait slows iteration, including an image or
release build for a deployment, when a new worktree or a parallel session
starts building, or when a workstation is set up for this repository.
[AGENTS.md](../AGENTS.md#validation-budget) owns what completion requires; this
file owns reaching it with less waiting and never weakens that route.

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

Wait diagnostics appear immediately, when the owner or queue head changes, and
at ten-second intervals while the owner is unchanged. They include elapsed and
configured wait time, the owner's guardian PID, checkout and safe command
identity; arbitrary arguments and environment values are never printed. Older
owner records without these fields are reported as unknown. Acquisition,
cancellation and timeout have distinct messages.

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

## Deployment builds

- **Build where the service deploys.** For a service deployed on Railway,
  Railway's builders compile `build/docker/Dockerfile` [at no build
  charge](https://docs.railway.com/pricing/plans), from the connected branch or
  from a directory uploaded with `railway up <dir> --path-as-root` and explicit
  project, environment and service. On three derived services (about 400–500
  crates, fat LTO, three binaries; October 2026) the per-service medians from
  build start to a passing health check were 3.1–5.9 minutes with the cooked
  layer cached and 5.5–8.2 minutes without it; a quarter to a half of builds
  missed that cache. Those builds ran without the `IMAGE_TESTS=unit` gate,
  which also compiles and runs the unit tests in a debug stage of the same
  build. The same three binaries, built one after another after a one-line
  change, took about 10 minutes of wall time on a shared laptop with four
  performance cores, cross-compiled over a warm cache.
- **An upload is a deployment.** `railway up` deploys what it builds, so
  [Authority](../AGENTS.md#authority) applies to the named target. It sets no
  [`RAILWAY_GIT_COMMIT_SHA`](https://docs.railway.com/variables/reference#git-variables):
  write the commit into both `ARG VCS_REF` defaults of the uploaded Dockerfile,
  which makes `app.commit` the uploader's claim rather than a Git-verified
  identity, and record the commit and the prepared directory's digest beside
  the deployment id. An upload
  whose watched paths did not change was observed to end `SKIPPED`; a file
  under `build/docker/` that differs per upload keeps it building.
- **Keep credentials out of the build.** BuildKit prints the value of an `ARG`
  that a `RUN` line references inside the step name the build log keeps, and
  Railway's Dockerfile builds have no secret mounts ([Railway staff,
  2026-06-07](https://station.railway.com/questions/how-to-properly-handle-secrets-inside-do-cf0a110b);
  reopen when Railway documents them). A private dependency enters as its
  locked release instead: `cargo vendor --locked --versioned-dirs
  vendor-private` writes that directory and prints the source configuration;
  keep only the private package, append the printed git-source and
  `vendored-sources` stanzas (not the `crates-io` one) to `.cargo/config.toml`,
  admit `.cargo/config.toml` and `vendor-private/` in `.dockerignore`, copy
  both into the `cooked` stage before its first cook, and add
  `vendor-private/**` and `.cargo/config.toml` to both `build.watchPatterns`
  forms in the service's deployment profile. `vendor/` stays the template's
  patched crates; public crates still come from crates.io under the
  `Cargo.lock` checksums.
- **On a workstation, compile for the target without emulation.** Use a
  workstation-only Dockerfile variant, because the shared
  `build/docker/Dockerfile` stays arch-neutral ([CI/CD Production
  Readiness](ci-cd-production-ready.md), deferred multi-platform image),
  and build it with
  `bash scripts/ci/validation-lock.sh -- docker buildx build --platform linux/amd64 -f <variant> .`.
  On an arm64 machine building `linux/amd64`, its compiling stages start
  `FROM --platform=$BUILDPLATFORM`, run
  `rustup target add x86_64-unknown-linux-gnu`, install Debian's
  `gcc-x86-64-linux-gnu`, `g++-x86-64-linux-gnu` and `libc6-dev-amd64-cross`,
  and set `CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER`,
  `CC_x86_64_unknown_linux_gnu`, `CXX_x86_64_unknown_linux_gnu` and
  `AR_x86_64_unknown_linux_gnu` to those tools. Emulating the same build took
  about twice the CPU time (31 against 16.5 CPU-minutes); the cross-built image
  passed `runtime-image-check` and Trivy with its auditable section intact.
- **Keep the cooked layer between builds.** `make runtime-image-build` creates
  a fresh builder per run; keep the cooked layer with `RUNTIME_IMAGE_CACHE_FROM`
  and `RUNTIME_IMAGE_CACHE_TO` (for example a `type=local` cache under
  `.artifacts/`), or with a task-owned `docker-container` builder named in
  `VALIDATION_BUILDER_NAME` with `VALIDATION_BUILDER_EXCLUSIVE=1`. A one-line
  source change then cost about a sixth of a cold build's CPU time (31 against
  175 CPU-minutes, both emulated). A workstation-only variant may keep
  `target/` in a `RUN --mount=type=cache` instead; `make docs-check` refuses
  mounts in the shared Dockerfile.
- **The release profile sets a floor.** Each binary ends with a
  single-threaded fat-LTO step: built alone, the service binary took about 5
  minutes on that laptop and about 3 on Railway's builders. The profile is a
  runtime decision (`[profile.release]` in `Cargo.toml`), and a performance
  comparison builds both sides with it.
- **An image build is a heavy command.** The one-heavy-build rule above covers
  it, together with any lock a service keeps for production measurements.
  Other builds on the same machine inflated wall time two to three times in
  these measurements, so on a shared machine compare CPU time measured inside
  the build step, for example
  `bash -c 'TIMEFORMAT="%R real %U user %S sys"; time cargo …'` in the
  variant's `RUN`.

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
| sccache with a remote backend in image builds | A Railway build holds no credential ([Deployment builds](#deployment-builds)) |
| A hosted remote builder | For a Railway-deployed service, Railway's builders already compile natively for the deployment target at no build charge; a hosted builder adds a vendor with source access |

Upstream compiler work, such as the parallel frontend and relink-don't-rebuild
in the Rust project's [fast-builds goals][fast-builds], arrives through the
reviewed toolchain bump.

[corrode]: https://corrode.dev/blog/tips-for-faster-rust-compile-times/
[cargo-16804]: https://github.com/rust-lang/cargo/issues/16804
[xprotect]: https://nnethercote.github.io/2025/09/04/faster-rust-builds-on-mac.html
[sccache]: https://github.com/mozilla/sccache
[fast-builds]: https://goals.rust-lang.org/2026/roadmap-fast-builds.html
