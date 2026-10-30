# Representation examples

## Raw input and a promised value

Premise: input carries a percentage as an integer, but operations accepting
`Percent` rely on the inclusive range 0–100. A raw DTO may carry an out-of-range
value until admission; the validated type protects later consumers from
rechecking that same range. The cost is a conversion boundary, private storage
and fallible mutation. Keep only the DTO if no consumer benefits from an
already-valid value. If validity later depends on a changing external limit,
this static type alone cannot establish it.

This std-only example models decoding as `RawPercent`; it makes no claim about
a serialization library. A deserializer for `Percent` would decode that raw
input and call `TryFrom`, rather than directly assign its private field. The
public paths here are `new`, `TryFrom`, `Default`, `set`, and copying an already
valid value; there is no mutable getter or unchecked conversion.

```rust
mod percentage {
    #[derive(Debug, PartialEq, Eq)]
    pub struct OutOfRange;

    pub struct RawPercent {
        pub value: u16,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Percent(u8);

    impl Percent {
        pub fn new(value: u16) -> Result<Self, OutOfRange> {
            if value <= 100 {
                Ok(Self(value as u8))
            } else {
                Err(OutOfRange)
            }
        }

        pub fn get(self) -> u8 {
            self.0
        }

        pub fn set(&mut self, value: u16) -> Result<(), OutOfRange> {
            let replacement = Self::new(value)?;
            *self = replacement;
            Ok(())
        }
    }

    impl TryFrom<RawPercent> for Percent {
        type Error = OutOfRange;

        fn try_from(raw: RawPercent) -> Result<Self, Self::Error> {
            Self::new(raw.value)
        }
    }

    impl Default for Percent {
        fn default() -> Self {
            Self(0)
        }
    }
}

use percentage::{OutOfRange, Percent, RawPercent};

let raw = RawPercent { value: 101 };
assert_eq!(raw.value, 101);
assert_eq!(Percent::try_from(raw), Err(OutOfRange));
assert_eq!(Percent::new(0).unwrap().get(), 0);
assert_eq!(Percent::new(100).unwrap().get(), 100);
assert_eq!(Percent::new(u16::MAX), Err(OutOfRange));
assert_eq!(Percent::default().get(), 0);
let mut value = Percent::try_from(RawPercent { value: 30 }).unwrap();
assert_eq!(value.set(101), Err(OutOfRange));
assert_eq!(value.get(), 30);
value.set(100).unwrap();
let copied = value;
assert_eq!(copied.get(), 100);
```

## One meaningful key for equality, order and hash

Premise: records with the same ID represent one entity even when their observed
timestamps differ. Equality, total order and hashing all use that ID, so ordered
and hashed collections agree about identity. The cost is explicit trait
implementations and responsibility for keeping them aligned. If a timestamp
instead distinguishes values, derive all traits over those fields; if only one
view needs chronological order, sort that view by timestamp without changing
record identity. If callers need no record ordering or hashing, omit those
implementations entirely.

The contrasting `Snapshot` below keeps the cheap all-fields derived semantics:
its ID and timestamp together identify a value. No wrapper is needed merely to
avoid plain fields. The incorrect mix “ID equality, timestamp `Ord`” would make
equal records compare unequal and lets collection choice change deduplication.

```rust
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashSet, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};

#[derive(Debug, Clone)]
struct Record {
    id: u64,
    observed_at: u64,
}

impl PartialEq for Record {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for Record {}
impl PartialOrd for Record {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Record {
    fn cmp(&self, other: &Self) -> Ordering {
        self.id.cmp(&other.id)
    }
}
impl Hash for Record {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

fn hash(value: &impl Hash) -> u64 {
    let mut state = DefaultHasher::new();
    value.hash(&mut state);
    state.finish()
}

let older = Record { id: 7, observed_at: 10 };
let newer = Record { id: 7, observed_at: 20 };
let next = Record { id: 8, observed_at: 0 };
assert_eq!(older, newer);
assert_eq!(older.cmp(&newer), Ordering::Equal);
assert_eq!(older.partial_cmp(&newer), Some(Ordering::Equal));
assert_eq!(hash(&older), hash(&newer));
assert!(older < next);
assert_eq!(BTreeSet::from([older.clone(), newer.clone(), next.clone()]).len(), 2);
assert_eq!(HashSet::from([older.clone(), newer.clone(), next.clone()]).len(), 2);
let mut chronological = vec![newer, older, next];
chronological.sort_by_key(|record| record.observed_at);
assert_eq!(chronological.iter().map(|record| record.observed_at).collect::<Vec<_>>(),
           vec![0, 10, 20]);

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct Snapshot {
    id: u64,
    observed_at: u64,
}
let first = Snapshot { id: 7, observed_at: 10 };
let second = Snapshot { id: 7, observed_at: 20 };
assert_ne!(first, second);
assert!(first < second);
assert_eq!(HashSet::from([first, second]).len(), 2);
```
