# Move, borrow, or share for a reason

Use this reference when deciding between owned fields, borrowed call views, `mem::replace`, or lock/drop boundaries. **Contracts** asks whether data outlives the call and what happens on every failure or unwind exit.

## Stored state versus transient call views

Keep owned fields (`String`, `Vec<T>`, `Arc<T>`) on stored state or values crossing `'static` `tokio::spawn` boundaries. Give per-call descriptors—queries, options, or write rows consumed within one call—borrowed views (`Cow<'a, str>`, `&'a [T]`, or `Borrow<T>`) so callers avoid cloning on every invocation.

Weak — forces every caller to allocate `String` keys and values:

```rust
struct OwnedQuery {
    sql: String,
    params: Vec<(String, String)>,
}
```

Strong — borrows literals with zero allocation while accepting dynamic `String` values, and uses `std::borrow::Borrow` rather than private sealed adapter traits:

```rust
use std::borrow::{Borrow, Cow};

#[derive(Debug)]
struct Query<'a> {
    sql: Cow<'a, str>,
    params: Vec<(Cow<'a, str>, Cow<'a, str>)>,
}

fn write_all<R, B>(rows: &[R])
where
    R: Borrow<B>,
    B: ?Sized,
{
    let _ = rows;
}
```

## Exclusive `&mut self`, single lock scope, and zero-waste insert

Drop unnecessary `Rc<RefCell<T>>` or split `Mutex` wrappers to plain `&mut self` (or one `Mutex<Inner>` when concurrent `&self` access is required) so coupled state and logs never race. On fallible inserts, avoid double-cloning keys and return the rejected value inside `Err`, naming read-only accessors without a `get_` prefix:

Weak — splits coupled state across two locks, double-clones the key on insert, drops the caller's `Session` on error, and uses a `get_` prefix:

```rust
use std::collections::HashMap;
use std::sync::Mutex;

struct LegacySession {
    id: String,
    ttl_secs: u32,
}

struct SplitRegistry {
    sessions: Mutex<HashMap<String, LegacySession>>,
    audit_log: Mutex<Vec<String>>,
}

impl SplitRegistry {
    fn register(&self, session: LegacySession) -> Result<(), String> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let id = session.id.clone();
        if sessions.contains_key(&id) {
            return Err(id);
        }
        sessions.insert(id.clone(), session);
        drop(sessions);
        self.audit_log
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(id);
        Ok(())
    }

    fn get_session(&self, id: &str) -> Option<u32> {
        self.sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .map(|s| s.ttl_secs)
    }
}
```

Strong — uses plain `&mut self` (keeping state and log consistent without locking), clones the key once only on success, returns the rejected `Session` in `Err`, and borrows `&str` via `session()`:

```rust
use std::collections::{BTreeMap, VecDeque};

struct Session {
    id: String,
    ttl_secs: u32,
}

struct DuplicateSession(Session);

struct SessionRegistry {
    sessions: BTreeMap<String, Session>,
    audit_log: VecDeque<String>,
}

impl SessionRegistry {
    fn register(&mut self, session: Session) -> Result<(), DuplicateSession> {
        if self.sessions.contains_key(&session.id) {
            return Err(DuplicateSession(session));
        }
        self.audit_log.push_back(format!("registered:{}", session.id));
        self.sessions.insert(session.id.clone(), session);
        Ok(())
    }

    fn session(&self, id: &str) -> Option<&Session> {
        self.sessions.get(id)
    }
}
```

## Prepare-then-commit and dropping outside locks

Borrow retained fields in place for inspection rather than extracting them. Reserve `mem::take` or `mem::replace` for true ownership transfer whose replacement state remains valid on every exit—prepare fallible work first, then commit atomically, and move freed collections or notifications out of the mutex scope before dropping them.

Bind cancel-safe rollback of a slot, leader claim, or permit to `std::mem::DropGuard` so early returns, panics, and dropped futures release automatically while the happy path dismisses the guard cleanly (in this repository, inspect [`PrimeLease`](../../../../crates/infra-clickhouse/src/metadata.rs) and [`AfterUnlock`](../../../../crates/infra-postgres/src/slots.rs)):

```rust
use std::mem::DropGuard;
use std::sync::atomic::{AtomicUsize, Ordering};

struct ActiveSlot<'a>(&'a AtomicUsize);

impl ActiveSlot<'_> {
    fn abort(self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }

    fn finish(self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

struct OperationLease<'a>(DropGuard<ActiveSlot<'a>, fn(ActiveSlot<'a>)>);

impl<'a> OperationLease<'a> {
    fn start(slot: ActiveSlot<'a>) -> Self {
        Self(DropGuard::new(slot, ActiveSlot::abort))
    }

    fn complete(self) {
        let slot = DropGuard::dismiss(self.0);
        slot.finish();
    }
}
```

**When to keep owned or explicit:** switch back to owned handles when state crosses `'static` spawns or needs an independent snapshot, do not change a settled borrowing contract merely to avoid a cheap clone, and add an explicit async `close` alongside `Drop` whenever teardown can fail or suspend.

Sources: [Jon Gjengset, *Rust for Rustaceans* (Ch. 3)](https://nostarch.com/rust-rustaceans), [Mara Bos, *Rust Atomics and Locks* (Ch. 1)](https://marabos.nl/atomics/), [Rust Design Patterns on `mem::replace`](https://rust-unofficial.github.io/patterns/idioms/mem-replace.html), and [Oxide RFD 400](https://rfd.shared.oxide.computer/rfd/400).
