---
name: rust-tokio
description: "Ownership. Use when async tasks, select arms, lock scopes, channels, spawn_blocking, or startup/shutdown need cancellation or lifetime guarantees."
---

# Rust Tokio

**Ownership.** Identify who admits work, owns its execution, observes failure, requests cancellation, and awaits completion across future, task, thread, and resource boundaries; a caller's wait and the operation's lifetime can diverge.

**Supervision.** Observe meaningful task outcomes while their parent serves traffic as well as during shutdown, separating unexpected completion from a requested drain (a join winning `select!` can reflect normal drain). Retain spawn handles so tasks stay attached and results stay observable. `crates/service/src/bootstrap` owns admission, `Background` registration, root cancellation, and joining. Treat cancellation propagation as cooperative rather than proof that child work or external effects have stopped.

**Cancellation safety.** Because suspension points allow dropping a future, identify which progress, queue slot, or owned payload may be discarded. `select!` drops owned losing futures; check operation-level cancellation safety (`read` preserves stream state while `read_exact` and `write_all` leave partial progress, and cancelling a pending send drops the unsent value while leaving already-delivered messages intact). Use `biased;` only for polling priority, paired with explicit admission control where required.

**Locks and poison.** Prefer a synchronous mutex for short, low-contention critical sections that complete before any `.await`; switch to an async mutex, message passing, or task-owned state when contention or suspension across `.await` is required. Check reentry, lock ordering, and caller-held guard lifetime before invoking callbacks or returning guards. Use non-poisoning locks (`parking_lot::Mutex`) or explicit `.unwrap_or_else(PoisonError::into_inner)` under workspace `unwrap_used` and `expect_used` lints only when a critical section preserves invariants on unwind; when a panic inside a multi-step update could leave shared state partial, re-establish or quarantine that invariant before exposing the state, and preserve justified `SeqCst` orderings.

**Blocking boundary.** Move complete blocking operations (including synchronous transactions and lock scopes) to the blocking owner or a supported async adapter (`clippy.toml` bans direct std blocking I/O), while keeping cheap bounded computation inline. Bound per-poll CPU work and tight ready-future loops, and register a wakeup before returning `Poll::Pending`.

**Admission lifetime.** Acquire admission before submitting blocking or CPU work and move `let _capacity = permit;` (`OwnedSemaphorePermit`, keeping a named binding so the permit lives through closure execution) inside `spawn_blocking`, since handle abort or waiter timeout leaves a started closure running; see the [execution-lifetime example](references/execution-lifetime.md). Supervise termination and panics, request cooperative cancellation where supported (using `JoinSet::shutdown` when aborting and draining an owned task set whose individual results go uninspected), and bound queued memory alongside task count.

**Timers and lifecycle.** Prefer state-change notifications over polling: carry shared state and its wakeup together in `watch` (`Receiver::wait_for`), reserving `Notify` for stateless edge or permit wakeups with a race-free state check, and bound external polling with an owner, cadence, and terminal condition. Pair `timeout` with resource reclamation that works even when consumers stop polling, use Tokio time for paused-clock tests, keep signal streams alive across their full lifetime, join all test-owned tasks on exit, and synchronize tests on observable state changes within the existing lifecycle owner.
