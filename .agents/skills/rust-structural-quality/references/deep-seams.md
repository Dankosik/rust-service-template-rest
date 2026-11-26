# Deep seams, pushing branches up, and Sans-I/O cores

Use this reference when callers must coordinate multi-step ceremony, when helpers grow `Option`/mode branches deep inside the call tree, or when testing timeouts and retries tempts introducing a `Clock` trait or `MockCtrl`.

## Push `if`s up and `for`s down

Weak — pushes mode checks and optional inputs down into a leaf helper, multiplying branches and forcing callers to guess preconditions:

```rust
struct DiscountInput {
    price_cents: u64,
    percent: Option<u8>,
    apply_discount: bool,
}

fn maybe_apply_discount(input: &DiscountInput) -> u64 {
    if !input.apply_discount {
        return input.price_cents;
    }
    let Some(percent) = input.percent else {
        return input.price_cents;
    };
    let clamped = u64::from(percent.min(100));
    input.price_cents - (input.price_cents * clamped / 100)
}
```

Strong — branches once at the boundary and passes a validated value to a straight-line batch operation:

```rust
#[derive(Clone, Copy)]
struct DiscountPercent(u8);

impl DiscountPercent {
    const fn try_new(percent: u8) -> Option<Self> {
        if percent <= 100 {
            Some(Self(percent))
        } else {
            None
        }
    }

    fn apply_all(self, prices_cents: &mut [u64]) {
        let pct = u64::from(self.0);
        for price in prices_cents {
            *price -= *price * pct / 100;
        }
    }
}
```

## Sans-I/O pure core instead of `Clock` traits or `MockCtrl`

Keep transition rules synchronous and pure by passing `now: Instant` (or elapsed `Duration`) directly as an argument, letting the thin I/O shell drive real sockets and timers:

```rust
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
enum RetryDecision {
    AttemptNow,
    WaitUntil(Instant),
    Exhausted,
}

struct RetryBudget {
    remaining: u8,
    backoff: Duration,
    next_allowed: Instant,
}

impl RetryBudget {
    const fn new(max_attempts: u8, backoff: Duration, now: Instant) -> Self {
        Self {
            remaining: max_attempts,
            backoff,
            next_allowed: now,
        }
    }

    fn poll(&mut self, now: Instant) -> RetryDecision {
        if self.remaining == 0 {
            return RetryDecision::Exhausted;
        }
        if now < self.next_allowed {
            return RetryDecision::WaitUntil(self.next_allowed);
        }
        self.remaining -= 1;
        self.next_allowed = now + self.backoff;
        RetryDecision::AttemptNow
    }
}
```

**When to stay plain:** keep I/O and control flow together in a short single-purpose handler when there is no branching state machine to test independently; never introduce a trait or wrapper merely to mimic Sans-I/O ceremony.

Sources: [John Ousterhout, *A Philosophy of Software Design*](https://web.stanford.edu/~ouster/cgi-bin/book.php), [matklad, *Push Ifs Up and Fors Down*](https://matklad.github.io/2023/11/15/push-ifs-up-and-fors-down.html), and [Thomas Eizinger, *sans-IO in Rust*](https://www.firezone.dev/blog/sans-io).
