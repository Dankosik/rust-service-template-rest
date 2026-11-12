//! Spreading timers that replicas would otherwise fire together.

use std::time::Duration;

/// The share `sample / u16::MAX` of `window`, rounded down to a nanosecond:
/// zero for a sample of 0 and the whole window for `u16::MAX`.
#[must_use]
pub fn share_of_window(window: Duration, sample: u16) -> Duration {
    // At most `window`, so the result always fits a `Duration`.
    Duration::from_nanos_u128(window.as_nanos() * u128::from(sample) / u128::from(u16::MAX))
}

#[cfg(test)]
mod tests {
    use super::share_of_window;
    use std::time::Duration;

    #[test]
    fn the_extreme_samples_give_nothing_and_the_whole_window() {
        for window in [Duration::ZERO, Duration::from_secs(90), Duration::MAX] {
            assert_eq!(share_of_window(window, 0), Duration::ZERO);
            assert_eq!(share_of_window(window, u16::MAX), window);
        }
    }

    #[test]
    fn a_share_rounds_down_to_whole_nanoseconds() {
        assert_eq!(
            share_of_window(Duration::from_nanos(1), 32_768),
            Duration::ZERO
        );
        // 21845 is a third of u16::MAX.
        assert_eq!(
            share_of_window(Duration::from_secs(3), 21_845),
            Duration::from_secs(1)
        );
        assert_eq!(
            share_of_window(Duration::from_nanos(10), 21_845),
            Duration::from_nanos(3)
        );
    }

    #[test]
    fn a_larger_sample_never_gives_a_smaller_share() {
        let window = Duration::from_secs(30);
        let mut previous = Duration::ZERO;
        for sample in 0..=u16::MAX {
            let share = share_of_window(window, sample);
            assert!(share >= previous && share <= window, "{sample}");
            previous = share;
        }
    }
}
