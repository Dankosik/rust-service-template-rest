//! JSON Web Token, RFC 7519.

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
    use super::numeric_date_seconds;

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
