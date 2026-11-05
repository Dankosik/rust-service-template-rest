# Capacity belongs to the execution it bounds

Use this example when a waiter can stop before its work does. **Ownership**
distinguishes the caller awaiting a result from the owner of actual execution.

This fragment requires an execution owner that retains and observes completion.
When working in this service, inspect its
[lifecycle path](../../../../docs/architecture/runtime-lifecycle.md#business-work-admission-and-lifetime)
against that requirement rather than assuming its current code meets it.
Elsewhere use the program's existing owner; the fragment does not require that
service architecture.
Here `permit` is an `OwnedSemaphorePermit` acquired under the
existing admission policy, `input` is owned, and `compute` is synchronous work
that eventually finishes. The existing supervisor receives `handle` and owns
observing completion and panic, including after a request stops waiting:

```rust
let handle = tokio::task::spawn_blocking(move || {
    let _capacity = permit;
    compute(input)
});
```

Moving the permit into the closure holds capacity for queued and running work
until the closure is dropped or exits, including unwind. Keeping it in the
request future instead would release capacity on request cancellation while
started blocking work continues. Use a named binding for the retained guard;
a wildcard pattern is not an ownership-retention mechanism.

The fragment establishes only this resource-lifetime relation. It is not a
complete spawning or shutdown recipe: the supervisor must retain and observe
the handle before applying waiter cancellation, and the existing admission
owner must bound queued work or refusal. Started `spawn_blocking` work cannot
be stopped by aborting its handle. A shutdown timeout stops waiting, not the
running closure. Cooperative cancellation, effect fencing, and durable handoff
remain separate requirements when the operation needs them. Cheap bounded
computation can stay inline; this example does not justify another executor.

Source: [Tokio 1.54.1 blocking-task contract](https://github.com/tokio-rs/tokio/blob/tokio-1.54.1/tokio/src/task/blocking.rs).
Recheck the resolved Tokio version if these task-lifetime assumptions change.
