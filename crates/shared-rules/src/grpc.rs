use std::time::Duration;

pub const TIMEOUT_HEADER: &str = "grpc-timeout";
pub const DIAGNOSTIC_BYTES: usize = 1024;
pub const HANDLING_SECONDS_BUCKETS: &[f64] = &[
    0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
];
pub const FALLBACK_SPAN_NAME: &str = "grpc";
pub const RPC_SYSTEM_NAME_ATTR: &str = "rpc.system.name";
pub const RPC_SYSTEM_GRPC: &str = "grpc";
pub const RPC_METHOD_ATTR: &str = "rpc.method";
pub const RPC_METHOD_OTHER: &str = "_OTHER";
pub const RPC_METHOD_ORIGINAL_ATTR: &str = "rpc.method_original";
pub const RPC_STATUS_CODE_ATTR: &str = "rpc.status_code";
pub const ERROR_TYPE_ATTR: &str = "error.type";

pub const MAX_TIMEOUT_DIGITS: usize = 8;
const MAX_TIMEOUT_VALUE: u64 = 99_999_999;
const WIRE_TIMEOUT_MAX: Duration = Duration::from_hours(MAX_TIMEOUT_VALUE);

const TIMEOUT_UNITS: [(u8, u128); 6] = [
    (b'n', Duration::from_nanos(1).as_nanos()),
    (b'u', Duration::from_micros(1).as_nanos()),
    (b'm', Duration::from_millis(1).as_nanos()),
    (b'S', Duration::from_secs(1).as_nanos()),
    (b'M', Duration::from_mins(1).as_nanos()),
    (b'H', Duration::from_hours(1).as_nanos()),
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatusNames {
    pub span: &'static str,
    pub metric: &'static str,
}

const UNKNOWN_STATUS: StatusNames = StatusNames {
    span: "UNKNOWN",
    metric: "Unknown",
};

pub const STATUS_CODE_COUNT: usize = STATUS_CODE_NAMES.len();

const STATUS_CODE_NAMES: &[StatusNames] = &[
    StatusNames {
        span: "OK",
        metric: "OK",
    },
    StatusNames {
        span: "CANCELLED",
        metric: "Canceled",
    },
    UNKNOWN_STATUS,
    StatusNames {
        span: "INVALID_ARGUMENT",
        metric: "InvalidArgument",
    },
    StatusNames {
        span: "DEADLINE_EXCEEDED",
        metric: "DeadlineExceeded",
    },
    StatusNames {
        span: "NOT_FOUND",
        metric: "NotFound",
    },
    StatusNames {
        span: "ALREADY_EXISTS",
        metric: "AlreadyExists",
    },
    StatusNames {
        span: "PERMISSION_DENIED",
        metric: "PermissionDenied",
    },
    StatusNames {
        span: "RESOURCE_EXHAUSTED",
        metric: "ResourceExhausted",
    },
    StatusNames {
        span: "FAILED_PRECONDITION",
        metric: "FailedPrecondition",
    },
    StatusNames {
        span: "ABORTED",
        metric: "Aborted",
    },
    StatusNames {
        span: "OUT_OF_RANGE",
        metric: "OutOfRange",
    },
    StatusNames {
        span: "UNIMPLEMENTED",
        metric: "Unimplemented",
    },
    StatusNames {
        span: "INTERNAL",
        metric: "Internal",
    },
    StatusNames {
        span: "UNAVAILABLE",
        metric: "Unavailable",
    },
    StatusNames {
        span: "DATA_LOSS",
        metric: "DataLoss",
    },
    StatusNames {
        span: "UNAUTHENTICATED",
        metric: "Unauthenticated",
    },
];

#[must_use]
pub const fn status_code_names(code: usize) -> StatusNames {
    if code < STATUS_CODE_NAMES.len() {
        STATUS_CODE_NAMES[code]
    } else {
        UNKNOWN_STATUS
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RpcName<'a> {
    pub service: &'a str,
    pub method: &'a str,
}

impl std::fmt::Display for RpcName<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.service, self.method)
    }
}

#[must_use]
pub fn split_method_path(path: &str) -> Option<RpcName<'_>> {
    let (service, method) = path.strip_prefix('/')?.split_once('/')?;
    let valid = !service.is_empty() && !method.is_empty() && !method.contains('/');
    valid.then_some(RpcName { service, method })
}

#[must_use]
pub fn diagnostic_rpc_method_original(path: &str) -> Option<&str> {
    let trimmed = path.trim_start_matches('/');
    (!trimmed.is_empty() && trimmed.len() <= DIAGNOSTIC_BYTES).then_some(trimmed)
}

#[must_use]
pub fn parse_timeout(value: &[u8]) -> Option<Duration> {
    if value.len() > MAX_TIMEOUT_DIGITS + 1 {
        return None;
    }
    let (&unit, digits) = value.split_last()?;
    if !digits.first()?.is_ascii_digit() {
        return None;
    }
    let amount: u32 = std::str::from_utf8(digits).ok()?.parse().ok()?;
    let (_, nanos_per_unit) = TIMEOUT_UNITS.into_iter().find(|&(code, _)| code == unit)?;
    Some(Duration::from_nanos_u128(
        u128::from(amount) * nanos_per_unit,
    ))
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
            u128::from(remaining.min(WIRE_TIMEOUT_MAX).as_secs() / 3600),
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
            ("1H", Some(Duration::from_hours(1))),
            ("2M", Some(Duration::from_mins(2))),
            ("5S", Some(Duration::from_secs(5))),
            ("250m", Some(Duration::from_millis(250))),
            ("7u", Some(Duration::from_micros(7))),
            ("99999999n", Some(Duration::from_nanos(99_999_999))),
            ("0n", Some(Duration::ZERO)),
            ("123456789S", None),
            ("+5S", None),
            ("1+5S", None),
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
        assert_eq!(
            encode_timeout(Duration::from_nanos(99_999_999_999), &mut wire),
            b"99999999u"
        );
    }

    #[test]
    fn split_method_path_requires_service_and_method_segments() {
        assert_eq!(
            split_method_path("/example.v1.EchoService/Unary"),
            Some(RpcName {
                service: "example.v1.EchoService",
                method: "Unary",
            })
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
