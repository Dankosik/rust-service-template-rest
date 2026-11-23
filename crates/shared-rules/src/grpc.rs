use std::time::Duration;

pub const TIMEOUT_HEADER: &str = "grpc-timeout";

pub const MAX_TIMEOUT_DIGITS: usize = 8;
const MAX_TIMEOUT_VALUE: u64 = 99_999_999;
const MAX_TIMEOUT: Duration = Duration::from_hours(MAX_TIMEOUT_VALUE);

#[must_use]
pub fn parse_timeout(value: &[u8]) -> Option<Duration> {
    let (&unit, digits) = value.split_last()?;
    if digits.is_empty() || digits.len() > MAX_TIMEOUT_DIGITS {
        return None;
    }
    let amount = digits.iter().try_fold(0_u32, |amount, &byte| {
        let digit = byte.checked_sub(b'0').filter(|&digit| digit < 10)?;
        Some(amount * 10 + u32::from(digit))
    })?;
    let amount = u64::from(amount);
    match unit {
        b'n' => Some(Duration::from_nanos(amount)),
        b'u' => Some(Duration::from_micros(amount)),
        b'm' => Some(Duration::from_millis(amount)),
        b'S' => Some(Duration::from_secs(amount)),
        b'M' => Some(Duration::from_mins(amount)),
        b'H' => Some(Duration::from_hours(amount)),
        _ => None,
    }
}

#[must_use]
pub fn encode_timeout(remaining: Duration, wire: &mut [u8; MAX_TIMEOUT_DIGITS + 1]) -> &[u8] {
    const LIMIT: u64 = MAX_TIMEOUT_VALUE + 1;
    let (unit, amount) = if remaining < Duration::from_nanos(LIMIT) {
        (b'n', remaining.as_nanos())
    } else if remaining < Duration::from_micros(LIMIT) {
        (b'u', remaining.as_micros())
    } else if remaining < Duration::from_millis(LIMIT) {
        (b'm', remaining.as_millis())
    } else if remaining < Duration::from_secs(LIMIT) {
        (b'S', u128::from(remaining.as_secs()))
    } else if remaining < Duration::from_mins(LIMIT) {
        (b'M', u128::from(remaining.as_secs() / 60))
    } else {
        (
            b'H',
            u128::from(remaining.min(MAX_TIMEOUT).as_secs() / 3600),
        )
    };
    let mut buffer = std::fmt::NumBuffer::new();
    let digits = amount.format_into(&mut buffer);
    let len = digits.len();
    wire[..len].copy_from_slice(digits.as_bytes());
    wire[len] = unit;
    &wire[..=len]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grpc_timeout_follows_the_protocol_grammar() {
        let cases = [
            (b"1H".as_slice(), Some(Duration::from_hours(1))),
            (b"2M", Some(Duration::from_mins(2))),
            (b"5S", Some(Duration::from_secs(5))),
            (b"00005S", Some(Duration::from_secs(5))),
            (b"00000000S", Some(Duration::ZERO)),
            (b"250m", Some(Duration::from_millis(250))),
            (b"7u", Some(Duration::from_micros(7))),
            (b"99999999n", Some(Duration::from_nanos(99_999_999))),
            (b"99999999H", Some(MAX_TIMEOUT)),
            (b"0n", Some(Duration::ZERO)),
            (b"123456789S", None),
            (b"/S", None),
            (b":S", None),
            (b"1:S", None),
            (b"1aS", None),
            (b"+5S", None),
            (b"1+5S", None),
            (b"-5S", None),
            (b" 5S", None),
            (b"5 S", None),
            (b"5S ", None),
            (b"\xffS", None),
            (b"1\xffS", None),
            (b"S", None),
            (b"1x", None),
            (b"", None),
        ];
        for (value, expected) in cases {
            assert_eq!(parse_timeout(value), expected, "{value:?}");
        }
    }

    #[test]
    fn encode_timeout_round_trips_and_caps_at_wire_maximum() {
        let mut wire = [0_u8; MAX_TIMEOUT_DIGITS + 1];
        for duration in [
            Duration::ZERO,
            Duration::from_nanos(1),
            Duration::from_nanos(99_999_999),
            Duration::from_millis(100),
            Duration::from_micros(99_999_999),
            Duration::from_millis(250),
            Duration::from_millis(99_999_999),
            Duration::from_secs(5),
            Duration::from_secs(99_999_999),
            Duration::from_mins(99_999_999),
            Duration::from_hours(2),
            MAX_TIMEOUT,
        ] {
            let encoded = encode_timeout(duration, &mut wire);
            assert_eq!(parse_timeout(encoded), Some(duration), "{duration:?}");
        }
        let encoded = encode_timeout(Duration::MAX, &mut wire);
        assert_eq!(encoded, b"99999999H");
        assert_eq!(
            encode_timeout(Duration::from_nanos(99_999_999_999), &mut wire),
            b"99999999u"
        );
    }
}
