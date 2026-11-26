# Trait bounds, sealing, and static assertions

Use this reference when generic type definitions over-constrain callers, when deciding whether to seal a trait, or when pinning `Send`/layout/lifetime contracts at compile time.

## Bounds on `impl`, not on `struct`

Weak — forces `Ord` on construction, storage, and every unrelated method:

```rust
struct BoundedCache<K: Ord, V> {
    entries: Vec<(K, V)>,
}
```

Strong — keeps the type unconstrained and bounds only operations that require `Ord` (use `derive-where` when derived traits add unneeded bounds on type parameters):

```rust
struct BoundedCache<K, V> {
    entries: Vec<(K, V)>,
}

impl<K, V> BoundedCache<K, V> {
    const fn new() -> Self {
        Self { entries: Vec::new() }
    }
}

impl<K: Ord, V> BoundedCache<K, V> {
    fn sort_keys(&mut self) {
        self.entries.sort_by(|left, right| left.0.cmp(&right.0));
    }
}
```

**When to bound the type:** put a bound on a `struct` only when the definition itself requires an associated type (such as `T: Iterator` storing `T::Item`) or a drop invariant requires it.

## Sealed traits, `const` assertions, and Edition 2024 `use<..>`

Seal a trait only to protect a closed protocol invariant (for example transaction isolation modes). Prefer `std::borrow::Borrow<T>`, `AsRef<T>`, or an inherent method whenever standard conversion semantics suffice. Pin critical `Send`/`Sync` and size guarantees with `const` assertions, and use Edition 2024 `use<..>` precise capturing when an `impl Iterator` return does not borrow a second parameter:

```rust
mod sealed {
    pub trait Sealed {}
}

pub trait TxMode: sealed::Sealed {
    const READ_ONLY: bool;
}

pub struct ReadOnly;
impl sealed::Sealed for ReadOnly {}
impl TxMode for ReadOnly {
    const READ_ONLY: bool = true;
}

struct ClientHandle {
    endpoint: String,
}

const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ClientHandle>();
    assert!(size_of::<ClientHandle>() <= 64);
};

fn matching_lines<'a>(
    lines: &'a [&'a str],
    prefix: &str,
) -> impl Iterator<Item = &'a str> + use<'a> {
    let owned_prefix = prefix.to_owned();
    lines
        .iter()
        .copied()
        .filter(move |line| line.starts_with(&owned_prefix))
}
```

**When to stay plain:** omit sealed traits for internal-only modules where a `pub(crate)` enum already closes the set of variants, and omit `const` size assertions on types whose layout is not performance- or ABI-sensitive.

Sources: [Rust API Guidelines (`C-STRUCT-BOUNDS`, `C-SEALED`)](https://rust-lang.github.io/api-guidelines/), [Jon Gjengset, *Rust for Rustaceans* (Ch. 2–3)](https://nostarch.com/rust-rustaceans), and [Rust Edition 2024 Precise Capturing](https://doc.rust-lang.org/edition-guide/rust-2024/rpit-lifetime-capture.html).
