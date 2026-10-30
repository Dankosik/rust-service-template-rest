---
name: rust-design
description: "Representation and responsibility. Use when a Rust change leaves data validity, ownership, mutation authority, responsibility placement, or the need for an abstraction unresolved."
---

# Rust Design

Start from the promise a caller can rely on, then choose the representation and
owner that can preserve it. Distinguish what is already true on entry from what
this operation must establish, including failures, ordering and resource
lifetime. The earliest owner with the required information should enforce the
rule; moving it earlier without that information merely duplicates a partial
check. A settled local edit needs no representation redesign.

## Make validity a truthful promise

A raw DTO can expose fields and deserialize directly because it promises only
to carry input. A type that promises a valid value must close every bypass:
constructors, conversions, deserialization, defaults and mutation. Route decoded
input through validation before producing that type; a validating `new` beside
an unrestricted deserializer or mutable field is insufficient. Private fields
help only if the owning module also preserves the invariant. Keep the DTO when
validation belongs to a later boundary; add a newtype when preventing actual
misuse repays conversion and API cost. Cross-resource or time-dependent
conditions still need a runtime owner, even when each stored value is valid.

Use an enum when alternatives exclude combinations that a bag of flags permits.
Keep ordinary fields when combinations are legitimate and independently mutable.
Separate representation from recovery meaning: a type can express absence, but
whether absence is an error depends on the operation's contract.

For identity and ordering, choose the meaningful key before implementing traits.
`cmp` returning `Equal` must agree with `Eq`, `PartialOrd` with `Ord`, and equal
values must hash equally. Ordering records by timestamp while equality uses
only ID violates that contract. Either use the same key throughout or sort by a
separate projection without claiming a different total order for the record.
Derive the traits when all fields already have the intended semantics; implement
only those callers need. Load [representation examples](references/representation-examples.md)
when deciding how to close construction paths or make identity, ordering and
hashing agree; they show both the stronger representation and the cheaper case
that does not need it.

## Give data and effects an owner

Borrow when the caller retains responsibility and the operation need not retain
the data. Move when responsibility transfers. A clone can buy independent
mutation or a bounded snapshot; weigh its size, frequency and retained lifetime
against a more complicated borrowing API. `Arc` shares allocation ownership,
not synchronized mutation. Before adding shared mutable state, name the
cross-owner invariant and the atomic operation that preserves it; partitioning
independent state often removes coordination. Shortening a borrow may solve a
conflict without introducing shared ownership. Task execution custody and
cancellation are separate questions from who owns its input.

Keep decisions over available data deterministic when that makes their rules
visible, leaving acquisition and effects with their existing owners. This can
be a local function, not a new layer. Follow a proposed boundary through its
callers: who can mutate the result, match an outcome, retain an allocation or
delay resource release? A locally simpler signature can move cost and
uncertainty to every caller. Preserve numeric overflow, byte-versus-text and
ordering semantics when replacing loops or iterator chains; collecting also
changes memory retention.

## Pay for structure with a present constraint

Try removing the proposed trait, helper, module or parallel path in the design:
which current constraint becomes harder to satisfy? A single-implementation
trait can enforce a needed dependency direction or dynamic dispatch; a concrete
type is cheaper when neither matters. A one-use helper can isolate an ownership
rule or a cohesive decision; an extra name that merely forwards arguments adds
indirection. Judge cohesion by the responsibility and authority that change
together, not by a name, line count or number of callers.

Reuse an existing owner before splitting it. Where placement remains open,
compare the call paths and actual plausible change cost of both options: fewer
lines can still demand coordinated edits across more owners. Do not invent a
future variation to justify today's interface. Retain a compatibility or
parallel path only for its accepted current requirement and removal condition.
For crate direction and composition, load [Component Boundaries](../../../docs/architecture/boundaries.md);
for file placement and local naming rules, load [Project Structure](../../../docs/project-structure-and-module-organization.md).
Those owners determine this service's permitted structure rather than an
example's layout.
