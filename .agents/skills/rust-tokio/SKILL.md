---
name: rust-tokio
description: "Ownership. Use when async tasks, select arms, lock scopes, channels, spawn_blocking, or startup/shutdown need cancellation or lifetime guarantees."
---

# Rust Tokio

**Ownership.** Identify who admits work, owns its actual execution, observes failure, requests cancellation, and waits for completion across future, task, thread, and resource boundaries; the caller's wait and the operation's lifetime need not coincide.

Observe meaningful task outcomes while their parent is serving, not only during shutdown; distinguish unexpected completion from a requested drain (a join winning a `select!` is not automatically a crash). Dropping a spawn handle detaches its task and loses its result. `crates/service/src/bootstrap` owns admission, `Background` registration, root cancellation, and joining. Cancellation propagation follows the selected primitive's contract and does not by itself establish that child work or effects have stopped.

At a suspension point, a future may be dropped; identify which progress, queue position, or owned payload may be discarded and whether that is permitted. `select!` drops losing futures it owns (a borrowed future can remain with its owner, and pinning alone does not preserve it after that owner is dropped). Check cancellation safety per operation: `read` can be cancellation-safe while `read_exact` and `write_all` leave partial progress, and a cancelled pending send may lose its value or queue position without retracting a delivered message. Biased polling establishes polling order, not an atomic admission barrier.

Prefer a synchronous mutex for short critical sections with low contention and no await while guarded; when contention or required suspension would block executor progress, use an async mutex, reduced sharing, or an existing task-owned resource. Before calling a callback or returning guarded access, check reentry, lock ordering, blocking, and caller-controlled lock lifetime; moving a callback out of a lock is valid only if required atomicity survives. Poison recovery needs project permission and invariant re-establishment; this service retains its adopted no-recovery policy. Scrutinize `SeqCst` without weakening it merely to remove the spelling.

An async wrapper or mutex does not make synchronous I/O nonblocking; move complete blocking operations (including transactions and lock scopes) behind the blocking owner or a supported async adapter while keeping cheap bounded computation inline. Distinguish sustained CPU work from occasional blocking calls and loops over immediately-ready futures (`await` does not guarantee yielding); bound work per poll and arrange a wakeup before returning `Pending`.

Bound admission before submitting blocking or CPU work, moving `let _capacity = permit;` (`OwnedSemaphorePermit`, never `let _ = permit;`) inside `spawn_blocking` so waiter cancellation cannot release capacity early while the started closure outlives handle abort or timeout. Keep an owner observing termination and panic, request cooperative cancellation when supported, and account for residual work at shutdown, using `JoinSet::shutdown` when aborting and draining an owned task set whose individual results are not needed; see the [execution-lifetime example](references/execution-lifetime.md). Bound queued work through the queue or a finite admission bound counting retained payloads as well as task count.

Prefer completion or change notifications over sleep loops: carry shared state and its wakeup together in `watch` (using `Receiver::wait_for` for predicate waits, including late subscribers) rather than pairing a separate flag or mutex with `Notify`, reserving `Notify` for stateless edge or permit wakeups with a race-free state check; where external state lacks notifications, polling needs an owner, bounded cadence, and terminal condition. `timeout` cannot preempt non-yielding work, and expiry does not prove non-execution of an external effect; hard resource-lifetime promises need an owner able to reclaim resources even when the consumer stops polling.

Use Tokio time for code controlled by a paused clock, keep signal streams alive for their intended lifetime, and follow the lifecycle owner's deadlines and cleanup order rather than adding an independent shutdown path. Prefer `async fn` when its contract suffices; explicit `Future` returns remain useful for required setup or interface behavior.

Leave no test-owned task running after completion, synchronize on observable state rather than sleep, and do not add a new actor, executor, timer, or lifecycle audit solely because this method names them.
