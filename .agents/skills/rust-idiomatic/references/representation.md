# Represent the relationship

Use this guidance when several fields describe one fact or the validity of a
field depends on another. The question from **Contracts** is which combinations
the caller is allowed to observe.

Bundle coupled fields (such as a value and its `completed_at` timestamp) into one
optional value while keeping genuinely independent flags (such as `draining`)
separate, preventing half-populated states. Grouping them does not prove that
the timestamp matches the value, that the value is fresh, or that concurrent
publication is coherent; those rules remain with the producer and publication
boundary. In this service, [readiness state and Check](../../../../crates/health/src/lib.rs)
locate an implementation to examine with its readers, staleness, and drain
behavior. Check the intended publication contract; existing layout is not itself
that contract.

An existing record or tuple may already carry the pair without a generic wrapper
abstraction. Use an enum when alternatives mutually exclude one another; when a
current refresh attempt and the last successful publication can coexist, one
phase flag must not erase either fact.

Before storing a second representation of a fact, identify what it preserves.
A derived value may justify storage for a current cost or snapshot requirement.
A historical count may remain necessary after the underlying records expire.
Without that reason, deriving the value removes a synchronization obligation.
This is a semantic decision, not a rule to remove every counter or cache.

Likewise, distinguish not supplied from supplied but empty only when the caller
does: a plain collection is enough otherwise. The selecting condition, rather
than the presence of an `Option` or enum, determines the representation.

Source: [Effective Rust, Item 1](https://www.lurklurk.org/effective-rust/use-types.html).
