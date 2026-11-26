# Hot-path idioms without premature complexity

Use this reference when reviewing or optimizing allocation-heavy paths, generic entry points, large error variants, or collection scans.

## Outlining generic entry points and zero-alloc lookups

Weak — monomorphizes the entire function body for every `AsRef<Path>` caller and allocates an owned `String` key even when the entry already exists:

```rust
use std::collections::BTreeMap;
use std::path::Path;

fn count_extension<P: AsRef<Path>>(counts: &mut BTreeMap<String, usize>, path: P) {
    let key = path
        .as_ref()
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_owned();
    *counts.entry(key).or_insert(0) += 1;
}
```

Strong — delegates immediately to a non-generic inner function and updates existing keys via borrowed `&str` before allocating on misses:

```rust
use std::collections::BTreeMap;
use std::path::Path;

fn count_extension(counts: &mut BTreeMap<String, usize>, path: impl AsRef<Path>) {
    count_extension_inner(counts, path.as_ref());
}

fn count_extension_inner(counts: &mut BTreeMap<String, usize>, path: &Path) {
    let ext = path
        .extension()
        .and_then(|raw| raw.to_str())
        .unwrap_or("");
    if let Some(slot) = counts.get_mut(ext) {
        *slot += 1;
        return;
    }
    counts.insert(ext.to_owned(), 1);
}
```

## Boxing rare large variants and avoiding intermediate `Vec` allocations

Box cold error details when they inflate every `Result<T, E>` on a hot path, and prefer `fmt::from_fn` or slice iterators (`chunks_exact`, `windows`, `chunk_by`) over building temporary `Vec<String>` values:

```rust
use std::fmt;

struct Diagnostics {
    header: [u8; 256],
}

enum DecodeError {
    Empty,
    Corrupt(Box<Diagnostics>),
}

fn join_csv<'a>(items: &'a [&'a str]) -> impl fmt::Display + use<'a> {
    fmt::from_fn(move |f| {
        for (index, item) in items.iter().enumerate() {
            if index > 0 {
                f.write_str(",")?;
            }
            f.write_str(item)?;
        }
        Ok(())
    })
}
```

Reuse caller buffers with `dst.clone_from(&src)` and shrink long-lived immutable collections to `Box<[T]>` or `Arc<str>`. Always call `.flush()` (or `.flush().await` and `.shutdown().await` on async writers) explicitly on `BufWriter` so I/O failures are never lost on drop.

**When to stay plain:** skip outlining, boxing, and `Box<[T]>` on cold setup paths or small enums (`<= 64` bytes) where the direct representation is clearer and measurements show no pressure.

Sources: [Nicholas Nethercote, *The Rust Performance Book*](https://nnethercote.github.io/perf-book/), [Clippy `large_enum_variant`](https://rust-lang.github.io/rust-clippy/master/index.html#large_enum_variant), and [`std::fmt::from_fn`](https://doc.rust-lang.org/std/fmt/fn.from_fn.html).
