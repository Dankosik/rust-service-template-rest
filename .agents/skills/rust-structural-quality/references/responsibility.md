# Structure that carries a current responsibility

Use this example when a borrow conflict, helper, or trait raises an unresolved
placement question. The **Deletion test** asks what becomes harder to preserve
if the proposed structure is removed.

In this fragment, `Item` is the input record and `Report::observe(&mut self,
&Item)` owns report accumulation. The two fields have independent access:

```rust
struct Batch {
    items: Vec<Item>,
    report: Report,
}

impl Batch {
    fn inspect(&mut self) {
        for item in &self.items {
            self.report.observe(item);
        }
    }
}
```

Calling a method requiring all of `&mut self` inside that loop would hide this
independence. A report owner is justified if its fields and operations belong
together and multiple decisions must preserve that relationship. If it merely
packages two nearby assignments, direct field access or a free function taking
the required fields may be clearer. Check the next accepted change: does the
structure let its rule change in one place, or make the reader visit more
places for the same operation? Do not redesign the entire object for a single
borrow-checker diagnostic.

For traits, establish the required dependency direction and variation before
using the consumer as evidence. In this service, if health must accept provider-defined checks
without depending on their crates, the existing
[Probe and collection](../../../../crates/health/src/lib.rs) are an
implementation to examine against that requirement. When PostgreSQL is
retained, `infra_postgres::PostgresProbe` is one adapter to inspect; an absent
profile does not require it. Neither existing `dyn` usage nor the number of
implementations proves the abstraction necessary. One implementation can
protect a required dependency direction; several similar functions may need
no new interface.

Prefer an existing standard trait when it expresses the variation, such as
`std::io::Write` for a synchronous output destination. A concrete type is
enough when no variation or dependency boundary exists. A callback may carry
one varying operation without a new trait family. Choose generics for an
actual statically known consumer and `dyn Trait` for an actual erased or
heterogeneous consumer; neither spelling is a quality ranking. Do not add a
trait, wrapper, builder, or crate solely to reproduce this example or mock
internal choreography.

Sources: [Niko Matsakis on interprocedural conflicts and factoring costs](https://smallcultfollowing.com/babysteps/blog/2018/11/01/after-nll-interprocedural-conflicts/),
[Rust Reference on independent fields](https://doc.rust-lang.org/reference/expressions/field-expr.html#borrowing),
and [Effective Rust on generics and trait objects](https://www.lurklurk.org/effective-rust/generics.html).
