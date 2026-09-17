# Lane report: process lifecycle, readiness, build metadata, process tests

Raw research lane output, 2026-09-17. Versions were read from the crates.io
and GitHub APIs on that date. Decisions are consolidated in
[synthesis.md](synthesis.md); this file is the evidence behind them.

## Executive summary

| Area | Recommendation | Why |
|---|---|---|
| Shutdown | `tokio::signal::unix` + `tokio_util::sync::CancellationToken` + `tokio_util::task::TaskTracker` + `axum::serve(..).with_graceful_shutdown(..)` wrapped in `tokio::time::timeout`, plus a manually owned `Runtime` for `shutdown_timeout` | Zero extra crates, matches the tokio.rs shutdown topic; axum already implements per-connection graceful shutdown, only the bound is missing |
| Readiness | Hand-written: background refresher publishing a snapshot through `tokio::sync::watch`; handler reads the snapshot and applies a staleness guard | No maintained axum/tower crate does background refresh + failure threshold + staleness; the only semantic match (`health`) is unmaintained since 2022 |
| Build metadata | `env!("CARGO_PKG_VERSION")` for the version; `vergen-gitcl` 10 in `build.rs` for the SHA, with `VERGEN_GIT_SHA` set from the Docker `ARG` when `.git` is absent | Verified env override and `default_on_error` fallback; shells out to `git`, no libgit2 build |
| Runtime limits | Nothing to do for CPU: `std::thread::available_parallelism` honors cgroup v2 (Rust 1.61) and v1 (Rust 1.64) quotas and Tokio 1.53.1 derives its worker count from it; no GOMEMLIMIT analogue exists | Verified against std docs, RELEASES.md and Tokio source |
| Process tests | `Command::new(env!("CARGO_BIN_EXE_service"))`, port discovery via `:0` + a stdout line, readiness poll with `ureq` (`default-features = false`), `nix::sys::signal::kill(.., SIGTERM)`, assert exit code and drain time | `Child::kill` is SIGKILL on both std and tokio; `nix` gives a safe `kill(2)` wrapper |
| Errors | `thiserror` in library crates; `anyhow` at the binary boundary; `fn main() -> ExitCode` | `process::exit` skips destructors; `ExitCode` runs them |

Budget note raised by the lane: the brief listed stage budgets that sum
past the grace period. In the Go source the readiness propagation delay is
counted *inside* `http.shutdown_timeout` (25 s includes the 15 s delay), so
the actual worst case is 25 + 2 + 5 + 5 + 5 = 42 s inside 45 s. Keep that
arithmetic and its validation.

## A. Graceful shutdown

| Crate / API | Latest (date) | Maintenance | Fit |
|---|---|---|---|
| `tokio` 1.53.1 (`signal::unix`, `time::timeout`, `Runtime::shutdown_timeout`) | 1.53.1 (2026-07-20) | Core | Signal source, per-stage bounds, final force-close |
| `tokio-util` 0.7.19 (`CancellationToken`, `TaskTracker`) | 0.7.19 (2026-07-21) | Core | The tokio.rs-recommended pair; `wait()` needs `close()` |
| `axum::serve(..).with_graceful_shutdown(f)` | axum 0.8.9 | Core | Stops accepting immediately when `f` completes, calls hyper `graceful_shutdown()` on every live connection, then awaits all connection tasks with no bound |
| `hyper_util::server::graceful::GracefulShutdown` | hyper-util 0.1.20 | Core | Same mechanism, exposes `count()` and a consumable `shutdown(self)` future; requires a hand-written accept loop |
| `tokio-graceful-shutdown` 0.20.0 | 0.20.0 (2026-07-30) | Active | Subsystem tree with one global timeout, not an ordered multi-stage pipeline; pulls `miette` |
| `tokio-graceful` 0.2.2 | 2024-09-30 | Stale | Skip |
| `signal-hook` / `signal-hook-tokio` | 0.4.4 / 0.4.0 | Active | Redundant under Tokio |
| `axum-server` 0.8.0 | 2025-12-06 | Third-party | Own `Handle`-based graceful shutdown; only needed with its TLS acceptors |

What `with_graceful_shutdown` does (axum 0.8.9 `serve/mod.rs`): the accept
loop selects between `accept()` and the signal; on signal it drops the
listener; each connection task calls `conn.graceful_shutdown()` (hyper 1.11:
idle HTTP/1 keep-alive connections close immediately, in-flight ones finish
the current response; HTTP/2 sends GOAWAY); the serve future then waits for
every connection task with no timeout. Connection tasks are spawned, so
dropping the serve future does not cancel them; `Runtime::shutdown_timeout`
or process exit does. `timeout(drain, serve_future)` → `Ok` means every
connection closed; `Err(Elapsed)` means the budget expired.

Gotchas: `TaskTracker::wait()` resolves only when closed and empty;
`CancellationToken::child_token()` is one-directional; `JoinSet` aborts on
drop and retains results, `TaskTracker` does neither; `tokio::signal::unix::signal`
must be installed before the signal can arrive and the libc handler is never
uninstalled, so keep the stream alive for the process lifetime; plain
`Runtime::drop` waits forever for `spawn_blocking` work.

## B. Readiness aggregation

| Crate | Latest (date) | Maintenance | Background refresh | Staleness | Verdict |
|---|---|---|---|---|---|
| `axum-health` | 0.2.1 (2026-05-25) | 1 star, one maintainer | No: probes inline per request | No | Exactly the model to avoid |
| `health` (neoeinstein) | 0.2.0 (2022-05-13) | Unmaintained | Yes (`PeriodicChecker`, `min_failures` default 3) | Yes (`leeway`) | Matching semantics, too stale to adopt; reference only |
| `tower-health`, `healthchecker`, `axum-healthcheck`, `tokio-health`, `readiness` | — | Do not exist | — | — | — |

State holder: `tokio::sync::watch` (readers clone a small snapshot; writer
`send_modify`; tests await `changed()`), over `arc-swap` (read-hot data only),
`std::sync::RwLock` (poisoning noise), `parking_lot` (extra dependency),
`AtomicPtr` (hand-rolled reclamation).

## C. Build metadata

| Option | Latest (date) | Notes |
|---|---|---|
| `env!("CARGO_PKG_VERSION")` | Cargo | Version, zero cost |
| Hand-rolled `build.rs` + `option_env!("VCS_REF")` | std | Smallest, but you own the edge cases |
| `vergen-gitcl` 10.0.3 | 2026-08-24; MSRV 1.96 | Shells out to `git`; `VERGEN_GIT_SHA` env override emitted verbatim; `default_on_error()` keeps `env!` compiling without a repo; `rerun-if-changed=.git/HEAD`; honors `VERGEN_IDEMPOTENT`, `SOURCE_DATE_EPOCH` |
| `vergen-git2` / `vergen-gix` 10.0.3 | same | Heavier backends, unnecessary when `git` is in the builder image |
| `built` 0.8.1 | 2026-05-21 | Build timestamp by default (reproducibility), heavier |
| `shadow-rs` 2.0.0 | 2026-04-23 | libgit2 default backend, many moving parts |
| `git-version` 0.3.9 | 2023-12-13 | Low activity |

## D. Runtime limits

`std::thread::available_parallelism()` reads the affinity mask and cgroup
CPU quotas: cgroup v2 since Rust 1.61 (rust-lang/rust#92697), cgroup v1 since
1.64 (#97925). Tokio 1.53.1 derives its default worker count from it
(`TOKIO_WORKER_THREADS` overrides). No GC and no `GOMEMLIMIT` equivalent;
memory awareness is an allocator and admission-limit decision, deferred.

## E. Process-level tests

| Crate / API | Latest (date) | Fit |
|---|---|---|
| `env!("CARGO_BIN_EXE_<name>")` | Cargo | Set for integration tests; Cargo builds the binary. Use this |
| `assert_cmd` 2.2.2 | 2026-05-11 | Runs to completion eagerly; no API to signal a running child. Fine for `--help`, wrong for SIGTERM |
| `escargot` 0.5.15 | 2025-08-11 | Rebuilds through cargo; unnecessary |
| `std::process::Child::kill` / `tokio::process::Child::kill` | — | SIGKILL on Unix |
| `nix` 0.31.3 (`signal`) | 2026-05-11 | Safe `kill(2)`; recommended |
| `libc` 0.2.189 | 2026-07-21 | One `unsafe` line; `1.0.0-alpha` exists, stay on 0.2 |
| `duct` 1.1.2 | 2026-09-03 | Overkill for one child |
| `wait-timeout` 0.2.1 | 2025-02-03 | Bounded `wait()` for std `Child` |
| `ureq` 3.4.2 | 2026-09-13 | Blocking, `forbid(unsafe)`; `default-features = false` drops TLS/gzip |
| `reqwest` 0.13.5 | 2026-09-08 | Heavy for a lifecycle test |
| `portpicker` 0.1.1 | 2021 | Stale and racy; bind `:0` instead |

Gotchas: send SIGTERM only after readiness is observed (otherwise the handler
may not be installed yet and the child dies with signal 15); always `wait()`
after signalling; consider `process_group(0)` so a Ctrl-C to `cargo test`
does not reach the child.

## F. Errors at the composition root

`thiserror` 2.0.20 in libraries; `anyhow` 1.0.104 for setup failures in the
binary; `fn main() -> ExitCode` with distinct codes for graceful (0), budget
expired (small non-zero), startup failure (1). Never `process::exit` once the
runtime and telemetry exist. `eyre`/`color-eyre` only for CLI-style reports.

## Unverified

Code sketches were not compiled; `axum-server` graceful API details,
`tokio-graceful-shutdown` per-subsystem timeouts, Kubernetes default grace
period, and allocator crates were not inspected.

## Sources

crates.io API for every crate above; GitHub API for push dates;
tokio.rs/tokio/topics/shutdown; docs.rs pages for `tokio-util` TaskTracker
and CancellationToken, `axum::serve::WithGracefulShutdown`, `hyper-util`
graceful, `hyper` http1 Connection, `tokio::signal::unix`, `tokio::runtime`,
`tokio::sync::watch`, `tokio::process`, `tokio-graceful-shutdown`,
`axum-health`, `health`, `arc-swap`, `vergen-gitcl`, `built`, `shadow-rs`,
`assert_cmd`, `duct`, `nix`, `ureq`, `eyre`; axum 0.8.9 `serve/mod.rs` and
graceful-shutdown example; hyper-util 0.1.20 `graceful.rs`; hyper 1.11.1
`proto/h1/dispatch.rs`; tokio 1.53.1 `loom/std/mod.rs`; vergen sources;
rust-lang/rust RELEASES.md 1.61 and 1.64; std docs for
`available_parallelism`, `process::exit`, `ExitCode`, `Child`; Cargo
environment-variables reference.
