# Represent the relationship

Use this example when several fields describe one fact or the validity of a
field depends on another. The question from **Contracts** is which combinations
the caller is allowed to observe.

An observation either exists with its value and completion time, or does not
exist. Independent optional fields allow half an observation:

```rust
use std::time::Instant;

struct Observation<T> {
    value: T,
    completed_at: Instant,
}

struct Cache<T> {
    latest: Option<Observation<T>>,
    draining: bool,
}
```

The pair belongs in one value because it describes one observation. `draining`
is independent and remains a boolean. This representation prevents missing
half of the pair; it does not prove that the timestamp matches the value, that
the value is fresh, or that concurrent publication is coherent. Those rules
still belong to the producer and publication boundary. When working in this
service, its [readiness state and Check](../../../../crates/health/src/lib.rs) locate an
implementation to examine with its readers, staleness, and drain behavior.
Check the intended publication contract; the existing layout is not itself
that contract.

An existing record or a local tuple may already carry the pair; the example
does not require a generic cache abstraction. Use an enum when the alternatives
really exclude one another. A current refresh attempt and the last successful
publication may coexist, so one phase flag must not erase either fact.

Before storing a second representation of a fact, identify what it preserves.
A derived value may justify storage for a current cost or snapshot requirement.
A historical count may remain necessary after the underlying records expire.
Without that reason, deriving the value removes a synchronization obligation.
This is a semantic decision, not a rule to remove every counter or cache.

Likewise, distinguish not supplied from supplied but empty only when the caller
does: a plain collection is enough otherwise. The selecting condition, rather
than the presence of an `Option` or enum, determines the representation.

Source: [Effective Rust, Item 1](https://www.lurklurk.org/effective-rust/use-types.html).
