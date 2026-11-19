use http::StatusCode;

pub(crate) use infra_grpc_core::protocol::{
    GRPC_TIMEOUT, STATUS_HEADERS, grpc_timeout, header_status_code, set_grpc_timeout,
};

pub(crate) fn unbracket_host(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host)
}

pub(crate) fn grpc_code_from_http_status(status: StatusCode) -> Option<tonic::Code> {
    match status {
        StatusCode::OK => None,
        StatusCode::BAD_REQUEST => Some(tonic::Code::Internal),
        StatusCode::UNAUTHORIZED => Some(tonic::Code::Unauthenticated),
        StatusCode::FORBIDDEN => Some(tonic::Code::PermissionDenied),
        StatusCode::NOT_FOUND => Some(tonic::Code::Unimplemented),
        StatusCode::TOO_MANY_REQUESTS
        | StatusCode::BAD_GATEWAY
        | StatusCode::SERVICE_UNAVAILABLE
        | StatusCode::GATEWAY_TIMEOUT => Some(tonic::Code::Unavailable),
        _ => Some(tonic::Code::Unknown),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_status_codes_follow_grpc_mappings() {
        assert_eq!(grpc_code_from_http_status(StatusCode::OK), None);
        assert_eq!(
            grpc_code_from_http_status(StatusCode::UNAUTHORIZED),
            Some(tonic::Code::Unauthenticated)
        );
        assert_eq!(
            grpc_code_from_http_status(StatusCode::SERVICE_UNAVAILABLE),
            Some(tonic::Code::Unavailable)
        );
    }
}
