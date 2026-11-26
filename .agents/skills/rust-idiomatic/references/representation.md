# Represent the relationship

Use this reference when a struct accumulates `Option`/`bool` fields, repeated `is_valid_*` checks, raw integers with distinct meanings, or derived caches. **Contracts** asks which combinations callers may construct and whether an invariant is proven once or re-checked on every call.

## Mode-coupled fields and positive bounds

Weak — allows plaintext with a CA path, unauthenticated with a password, and zero limits:

```rust
use std::path::PathBuf;

struct RawOptions {
    password: Option<String>,
    allow_unauthenticated: bool,
    root_ca_path: Option<PathBuf>,
    allow_plaintext: bool,
    max_rows: u64,
}
```

Strong — illegal combinations and zero bounds cannot be constructed, and `Auth` redacts credentials in `Debug`:

```rust
use std::fmt;
use std::num::NonZeroU64;
use std::path::PathBuf;

#[derive(PartialEq, Eq)]
enum Auth {
    Unauthenticated,
    Password(String),
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unauthenticated => f.write_str("Unauthenticated"),
            Self::Password(_) => f.write_str("Password(<redacted>)"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Transport {
    Plaintext,
    Tls { root_ca_path: Option<PathBuf> },
}

#[derive(Debug, PartialEq, Eq)]
struct Options {
    auth: Auth,
    transport: Transport,
    max_rows: NonZeroU64,
}
```

The strong form removes cross-field rejection branches downstream and proves `max_rows >= 1` at construction. External facts—file readability, peer reachability, or publication freshness—still belong to the I/O or publication boundary; in this repository, inspect [readiness state and Check](../../../../crates/health/src/lib.rs) alongside its staleness and drain readers.

**When to stay plain:** keep fields direct on passive wire/DTO records, raw deserialization inputs before admission, or when every field combination is genuinely independent (for example optional metadata `version` and `commit`). Distinguish absent from empty only when callers do.

## Parse once instead of re-validating

Weak — accepts an out-of-range wire scale, forcing recursive validation on every call:

```rust
struct UncheckedWireScale {
    unscaled: i128,
    scale: u8,
}
```

Strong — private fields and a fallible `const` constructor establish the wire bound once (use `rust_decimal` from `workspace-crates.md` for general arithmetic):

```rust
const MAX_WIRE_SCALE: u8 = 38;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WireDecimal {
    unscaled: i128,
    scale: u8,
}

impl WireDecimal {
    #[must_use]
    const fn try_new(unscaled: i128, scale: u8) -> Option<Self> {
        if scale <= MAX_WIRE_SCALE {
            Some(Self { unscaled, scale })
        } else {
            None
        }
    }
}
```

Wrap distinct counters or identifiers sharing a primitive (such as `SchemaGen(u64)` and `LeaderId(u64)`) in single-field newtypes whenever swapping arguments would compile silently. Bind a justified live projection or cache to its source owner by handle or borrow so callers cannot pass mismatched pairs; derive the value directly when no cost, retention, or snapshot need exists.

**When to stay plain:** skip newtypes for local loop indices or single-helper values, and omit `Default` when no neutral value exists.

Sources: [Alexis King, *Parse, Don't Validate*](https://lexi-lambda.github.io/blog/2019/11/05/parse-don-t-validate/), [Effective Rust, Item 1](https://www.lurklurk.org/effective-rust/use-types.html), and [matklad, *Push Ifs Up and Fors Down*](https://matklad.github.io/2023/11/15/push-ifs-up-and-fors-down.html).
