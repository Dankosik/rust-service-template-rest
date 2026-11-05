# Let the operation carry its meaning

Use this example when a collection update or iterator rewrite can change
replacement, ordering, failure, or effects. **Contracts** asks what happens to
the existing value, not just what the return value says.

Suppose registration must reject a duplicate without replacing its original
payload. This local API also returns the rejected payload to its owner:

```rust
use std::collections::{HashMap, hash_map::Entry};

fn register<T>(
    entries: &mut HashMap<String, T>,
    key: String,
    value: T,
) -> Result<(), T> {
    match entries.entry(key) {
        Entry::Vacant(slot) => {
            slot.insert(value);
            Ok(())
        }
        Entry::Occupied(_) => Err(value),
    }
}
```

`insert(key, value).is_some()` discovers a duplicate after replacing the old
value. `Entry` expresses the decision while holding exclusive access to that
place. If replacement is the contract, `insert` is the simpler correct choice.
If callers do not recover the rejected value, an existing domain error may
be appropriate instead; this example's `Result<(), T>` is not a general error
policy. This is one in-memory mutation, not database or distributed atomicity.

Collection choice also carries ordering and retention. A `HashMap` does not
promise output order. `BTreeMap` supplies key order, not insertion order; sorting
at the output boundary can preserve a useful existing owner. Use an ordered
collection only for an accepted observable order. Adding a second index adds
an obligation to keep insert, replacement, expiry, and removal coherent.

A short iterator chain suits a transformation with one result. An explicit
loop can expose evolving state, several effects, and early termination more
clearly. Compare the semantic steps: which items are visited, in what order,
when failure stops work, and whether results accumulate or stream. Replacing
fallible collection with `filter_map(Result::ok)` silently changes failure to
discard; moving `take` before `filter` changes which items qualify. These are
contract changes even when the resulting code is shorter.

Sources: [`Entry`](https://doc.rust-lang.org/std/collections/hash_map/enum.Entry.html),
[Effective Rust, Item 9](https://www.lurklurk.org/effective-rust/iterators.html),
and [Mini-Redis's entries and expiration index](https://github.com/tokio-rs/mini-redis/blob/master/src/db.rs).
Do not infer a measured speedup from fewer lookups or fewer lines.
