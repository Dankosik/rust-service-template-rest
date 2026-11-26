# Error chains without double rendering or lost ownership

Use this reference when designing typed error enums/structs, chaining upstream `Error::source()` causes, or returning rejected owned payloads from fallible operations.

## Do not interpolate `#[source]` into `Display`

When a boundary renderer walks `Error::source()` (such as service logs, Problem mappers, or `anyhow` debug formatting), interpolating the inner error into the outer `Display` (for example `#[error("read config {path}: {source}")]`) duplicates the root cause on every layer. Keep each layer's `Display` limited to its own context, and never implement `#[from]` on a wide `std::io::Error` where `?` drops the path or operation name.

Weak — embeds the underlying `io::Error` text inside `Display` while also returning it from `source()`:

```rust
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

#[derive(Debug)]
struct DuplicateReadError {
    path: PathBuf,
    source: std::io::Error,
}

impl fmt::Display for DuplicateReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "failed to read {}: {}", self.path.display(), self.source)
    }
}

impl Error for DuplicateReadError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}
```

Strong — attaches operation context in `Display`, exposes the cause solely through `source()`, and scopes the error to the specific operation:

```rust
use std::error::Error;
use std::fmt;
use std::path::PathBuf;

#[derive(Debug)]
struct ReadConfigError {
    path: PathBuf,
    source: std::io::Error,
}

impl fmt::Display for ReadConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "read config file {}", self.path.display())
    }
}

impl Error for ReadConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}
```

## Return expensive consumed payloads on `Err`

When a fallible send or encode takes ownership of an expensive payload (`Vec<u8>`, batch, or message), return the unconsumed payload inside the error so callers can retry or inspect without re-allocating:

```rust
#[derive(Debug, PartialEq, Eq)]
struct QueueFullError<T> {
    payload: T,
    capacity: usize,
}

impl<T> QueueFullError<T> {
    fn into_payload(self) -> T {
        self.payload
    }
}
```

**When to stay plain:** if a standalone CLI prints only `{err}` without walking `source()`, format the cause explicitly at the printing boundary rather than duplicating it inside library `Display` impls; skip payload recovery on cheap `Copy` or borrowed inputs.

Sources: [Sabrina Jewson, *Modular Errors in Rust*](https://sabrinajewson.org/blog/errors), [BurntSushi, *Error Handling in Rust*](https://blog.burntsushi.net/rust-error-handling/), and [Rust API Guidelines (`C-INTERMEDIATE`)](https://rust-lang.github.io/api-guidelines/interoperability.html).
