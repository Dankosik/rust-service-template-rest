# Move, borrow, or share for a reason

Use this example when an operation actually needs to move an owned payload
between representations. **Contracts** first asks whether anything must leave
its current owner. If the operation only inspects state or updates retained
fields, borrow them in place; extracting a value just to inspect it adds a
restoration obligation without transferring responsibility.

When a caller gives up the previous state, a consuming transition can move the
payload directly:

```rust
enum Batch {
    Collecting { items: Vec<String>, missing: usize },
    Ready(Vec<String>),
}

fn seal(batch: Batch) -> Batch {
    match batch {
        Batch::Collecting { items, missing: 0 } => Batch::Ready(items),
        unchanged => unchanged,
    }
}
```

Here sealing consumes the previous batch and is allowed only when no items
are missing. It performs no I/O or validation beyond that condition. The vector
moves into the ready representation; no temporary empty state is needed.

If an established API lends only `&mut Batch`, preserve that API. `mem::take`,
`mem::replace`, or `Option::take` can move a payload when the replacement is
valid on every observable exit. Check failure, unwind, callbacks, and suspension
between extraction and replacement. A get-and-reset operation may intentionally
leave the owner empty; a transition promising to preserve input on failure
needs that stronger guarantee. `take` itself promises neither rollback nor
delivery. Do not change a settled borrowing contract just to copy this example.

For read-only inspection, borrow a slice instead. For an independent snapshot,
copying may be the required semantics. For immutable data genuinely shared
after a lock is released, cloning an `Arc<T>` or `Bytes` handle can make the
ownership simpler. It retains the allocation and does not freeze interior
mutability; a snapshot must have the promised independence. Do not replace an
intentional clone merely because a move example is available.

Sources: [Rust Design Patterns on moving between variants](https://rust-unofficial.github.io/patterns/idioms/mem-replace.html),
[`mem::take`](https://doc.rust-lang.org/std/mem/fn.take.html), and
[Mini-Redis's owned value returned by get](https://github.com/tokio-rs/mini-redis/blob/master/src/db.rs).
The transferable Mini-Redis choice is the short lock plus shared immutable
bytes; its teaching code's panic and process-lifecycle policies are not this
service's policies.
