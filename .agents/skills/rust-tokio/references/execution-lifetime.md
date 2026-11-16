# Capacity belongs to the execution it bounds

Use this example when a waiter can stop before its work does. **Ownership**
distinguishes the caller awaiting a result from the owner of actual execution.

This fragment requires an execution owner that retains and observes completion;
inspect the lifecycle path in `crates/service/src/bootstrap` against that
requirement rather than assuming existing code meets it.
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

Source: [Tokio `spawn_blocking` contract](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html).
