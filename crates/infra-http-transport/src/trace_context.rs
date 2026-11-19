use axum::http::header::{Entry, HeaderName};
use axum::http::{HeaderMap, HeaderValue};

const TRACEPARENT_V0_LEN: usize = 55;
const TRACEPARENT: HeaderName = HeaderName::from_static("traceparent");
const TRACESTATE: HeaderName = HeaderName::from_static("tracestate");

pub fn admit_trace_context(headers: &mut HeaderMap, trusted: bool) -> bool {
    let accepted = trusted && admit_traceparent(headers);
    if accepted {
        normalize_tracestate(headers);
    } else {
        headers.remove(&TRACEPARENT);
        headers.remove(&TRACESTATE);
    }
    accepted
}

fn admit_traceparent(headers: &mut HeaderMap) -> bool {
    let Entry::Occupied(mut entry) = headers.entry(&TRACEPARENT) else {
        return false;
    };
    if entry.iter().nth(1).is_some() {
        return false;
    }
    let wire = entry.get().as_bytes();
    let Some(prefix) = validate_traceparent_prefix(wire.trim_ascii()) else {
        return false;
    };
    if prefix.len() != wire.len() {
        let Ok(value) = HeaderValue::from_bytes(prefix) else {
            return false;
        };
        entry.insert(value);
    }
    true
}

// W3C Trace Context §3.2.4.
fn validate_traceparent_prefix(wire: &[u8]) -> Option<&[u8]> {
    let prefix = wire.get(..TRACEPARENT_V0_LEN)?;
    let version = &prefix[..2];
    let trace_id = &prefix[3..35];
    let parent_id = &prefix[36..52];
    let flags = &prefix[53..];
    let lower_hex = |value: &[u8]| value.iter().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    let valid_id = |value: &[u8]| lower_hex(value) && value.iter().any(|&b| b != b'0');
    (lower_hex(version)
        && version != b"ff"
        && prefix[2] == b'-'
        && valid_id(trace_id)
        && prefix[35] == b'-'
        && valid_id(parent_id)
        && prefix[52] == b'-'
        && lower_hex(flags)
        && wire
            .get(TRACEPARENT_V0_LEN)
            .is_none_or(|&next| version != b"00" && next == b'-'))
    .then_some(prefix)
}

fn normalize_tracestate(headers: &mut HeaderMap) {
    let Entry::Occupied(mut entry) = headers.entry(&TRACESTATE) else {
        return;
    };
    if entry.iter().nth(1).is_none() {
        return;
    }
    let first = entry.get();
    let additional_bytes = entry
        .iter()
        .skip(1)
        .map(|value| 1 + value.len())
        .sum::<usize>();
    let mut combined = Vec::with_capacity(first.len() + additional_bytes);
    combined.extend_from_slice(first.as_bytes());
    for value in entry.iter().skip(1) {
        combined.push(b',');
        combined.extend_from_slice(value.as_bytes());
    }
    match HeaderValue::try_from(combined) {
        Ok(value) => {
            entry.insert(value);
        }
        Err(_) => {
            entry.remove();
        }
    }
}
