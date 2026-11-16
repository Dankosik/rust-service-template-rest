/// RFC 7519 §4.1.4 makes expiration exclusive. All arguments are in whole seconds.
#[must_use]
pub const fn is_expired(expiry: u64, now: u64, leeway: u64) -> bool {
    match now.checked_sub(leeway) {
        Some(earliest_now) => earliest_now >= expiry,
        None => false,
    }
}

/// RFC 7519 §4.1.5 makes `nbf` inclusive: a token is usable from that second
/// on, `leeway` seconds earlier for clock skew. All arguments are in whole seconds.
#[must_use]
pub const fn is_not_yet_valid(not_before: u64, now: u64, leeway: u64) -> bool {
    not_before > now.saturating_add(leeway)
}

/// A `NumericDate` (RFC 7519 §2) given as a JSON number with a fraction, in
/// whole seconds: finite, not negative, and below `2^64`, rounded to the
/// nearest second as `jsonwebtoken` reads `exp` and `nbf`. `None` for any
/// other number.
#[must_use]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "only a finite value inside the u64 range reaches the cast"
)]
pub fn numeric_date_seconds(value: f64) -> Option<u64> {
    (value.is_finite() && value >= 0.0 && value < u64::MAX as f64).then(|| value.round() as u64)
}

#[cfg(test)]
mod tests {
    use super::{is_expired, is_not_yet_valid, numeric_date_seconds};

    #[test]
    fn the_lifetime_window_bounds_are_widened_by_the_leeway() {
        assert!(!is_expired(100, 129, 30));
        assert!(is_expired(100, 130, 30));
        assert!(!is_expired(100, 10, 30));
        assert!(!is_not_yet_valid(130, 100, 30));
        assert!(is_not_yet_valid(131, 100, 30));
        assert!(!is_not_yet_valid(u64::MAX, u64::MAX - 1, 30));
    }

    #[test]
    fn a_fractional_date_rounds_to_the_nearest_second() {
        assert_eq!(numeric_date_seconds(0.0), Some(0));
        assert_eq!(numeric_date_seconds(-0.0), Some(0));
        assert_eq!(numeric_date_seconds(1_700_000_000.4), Some(1_700_000_000));
        assert_eq!(numeric_date_seconds(1_700_000_000.5), Some(1_700_000_001));
        // The largest double below 2^64.
        assert_eq!(
            numeric_date_seconds(18_446_744_073_709_549_568.0),
            Some(18_446_744_073_709_549_568)
        );
    }

    #[test]
    fn a_negative_infinite_or_oversized_number_is_no_date() {
        for value in [
            -1.0,
            -0.6,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            18_446_744_073_709_551_616.0,
        ] {
            assert_eq!(numeric_date_seconds(value), None, "{value}");
        }
    }
}
