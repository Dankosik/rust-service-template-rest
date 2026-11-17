use std::time::Duration;

pub const MAX_TIMEOUT_DIGITS: usize = 8;
pub const WIRE_TIMEOUT_MAX: Duration = Duration::from_hours(99_999_999);

#[must_use]
pub fn split_method_path(path: &str) -> Option<(&str, &str)> {
    let (service, method) = path.strip_prefix('/')?.split_once('/')?;
    let valid = !service.is_empty() && !method.is_empty() && !method.contains('/');
    valid.then_some((service, method))
}

#[must_use]
pub fn parse_timeout(value: &[u8]) -> Option<Duration> {
    if value.len() > MAX_TIMEOUT_DIGITS + 1 {
        return None;
    }
    let (&unit, digits) = value.split_last()?;
    if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let amount: u64 = std::str::from_utf8(digits).ok()?.parse().ok()?;
    match unit {
        b'H' => Some(Duration::from_hours(amount)),
        b'M' => Some(Duration::from_mins(amount)),
        b'S' => Some(Duration::from_secs(amount)),
        b'm' => Some(Duration::from_millis(amount)),
        b'u' => Some(Duration::from_micros(amount)),
        b'n' => Some(Duration::from_nanos(amount)),
        _ => None,
    }
}

#[must_use]
pub fn encode_timeout(remaining: Duration, wire: &mut [u8; MAX_TIMEOUT_DIGITS + 1]) -> &[u8] {
    const NANOS_PER_UNIT: [(u8, u128); 5] = [
        (b'n', 1),
        (b'u', 1_000),
        (b'm', 1_000_000),
        (b'S', 1_000_000_000),
        (b'M', 60_000_000_000),
    ];
    const MAX_VALUE: u128 = 99_999_999;
    let nanos = remaining.min(WIRE_TIMEOUT_MAX).as_nanos();
    let (unit, amount) = NANOS_PER_UNIT
        .into_iter()
        .map(|(unit, per_unit)| (unit, nanos / per_unit))
        .find(|&(_, amount)| amount <= MAX_VALUE)
        .unwrap_or((b'H', nanos / 3_600_000_000_000));
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
            ("1H", Some(Duration::from_hours(1))),
            ("2M", Some(Duration::from_mins(2))),
            ("5S", Some(Duration::from_secs(5))),
            ("250m", Some(Duration::from_millis(250))),
            ("7u", Some(Duration::from_micros(7))),
            ("99999999n", Some(Duration::from_nanos(99_999_999))),
            ("0n", Some(Duration::ZERO)),
            ("123456789S", None),
            ("+5S", None),
            ("-5S", None),
            (" 5S", None),
            ("S", None),
            ("1x", None),
            ("", None),
        ];
        for (value, expected) in cases {
            assert_eq!(parse_timeout(value.as_bytes()), expected, "{value:?}");
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
            WIRE_TIMEOUT_MAX,
        ] {
            let encoded = encode_timeout(duration, &mut wire);
            assert_eq!(parse_timeout(encoded), Some(duration), "{duration:?}");
        }
        let encoded = encode_timeout(Duration::MAX, &mut wire);
        assert_eq!(encoded, b"99999999H");
    }

    #[test]
    fn split_method_path_requires_service_and_method_segments() {
        assert_eq!(
            split_method_path("/example.v1.EchoService/Unary"),
            Some(("example.v1.EchoService", "Unary"))
        );
        for invalid in [
            "",
            "/",
            "/Service",
            "/Service/",
            "//Method",
            "/a/b/c",
            "Service/Method",
        ] {
            assert_eq!(split_method_path(invalid), None, "{invalid:?}");
        }
    }
}
