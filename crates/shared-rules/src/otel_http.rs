//! OpenTelemetry HTTP semantic conventions.

/// The `http.request.method` value for a method no convention names.
pub const OTHER_METHOD: &str = "_OTHER";

/// The `http.request.method` value for `method`: the method itself when the
/// conventions name it (RFC 9110, `PATCH`, and `QUERY`), otherwise
/// [`OTHER_METHOD`], so a caller-chosen method cannot add a telemetry series.
/// Methods are case-sensitive, so `get` is not `GET`.
#[must_use]
pub fn request_method(method: &str) -> &'static str {
    match method {
        "CONNECT" => "CONNECT",
        "DELETE" => "DELETE",
        "GET" => "GET",
        "HEAD" => "HEAD",
        "OPTIONS" => "OPTIONS",
        "PATCH" => "PATCH",
        "POST" => "POST",
        "PUT" => "PUT",
        "QUERY" => "QUERY",
        "TRACE" => "TRACE",
        _ => OTHER_METHOD,
    }
}

#[cfg(test)]
mod tests {
    use super::{OTHER_METHOD, request_method};

    #[test]
    fn a_named_method_is_its_own_value() {
        for method in [
            "CONNECT", "DELETE", "GET", "HEAD", "OPTIONS", "PATCH", "POST", "PUT", "QUERY", "TRACE",
        ] {
            assert_eq!(request_method(method), method);
        }
    }

    #[test]
    fn any_other_method_shares_one_value() {
        for method in ["get", "PROPFIND", "", "GET "] {
            assert_eq!(request_method(method), OTHER_METHOD, "{method:?}");
        }
    }
}
