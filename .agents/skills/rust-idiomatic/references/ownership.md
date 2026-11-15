# Move, borrow, or share for a reason

Use this example when an operation needs to move an owned payload between
representations. **Contracts** first asks whether anything must leave its
current owner: if the operation only inspects state or updates retained fields,
borrow them in place rather than extracting a value just to inspect it.

When an established API lends `&mut T`, `mem::take`, `mem::replace`, or
`Option::take` can move a payload out if the replacement is valid on every
observable exit. Check failure, unwind, callbacks, and suspension between
extraction and replacement. A get-and-reset operation may intentionally leave
the owner empty; a transition promising to preserve input on failure needs
that stronger guarantee. `take` itself promises neither rollback nor delivery.
Do not change a settled borrowing contract just to avoid a clone.

For read-only inspection, borrow a slice instead. For an independent snapshot,
copying may be the required semantics. For immutable data genuinely shared
after a lock is released, cloning an `Arc<T>` or `Bytes` handle can make
ownership simpler; it retains the allocation and does not freeze interior
mutability, and a snapshot must still have the promised independence. Do not
replace an intentional clone merely because a move is available.

Sources: [Rust Design Patterns on moving between variants](https://rust-unofficial.github.io/patterns/idioms/mem-replace.html),
[`mem::take`](https://doc.rust-lang.org/std/mem/fn.take.html), and
[Mini-Redis's owned value returned by get](https://github.com/tokio-rs/mini-redis/blob/master/src/db.rs).
The transferable Mini-Redis choice is the short lock plus shared immutable
bytes; its teaching code's panic and process-lifecycle policies are not this
service's policies.
