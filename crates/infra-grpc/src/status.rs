use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::Duration;

use bytes::Bytes;
use enum_map::EnumMap;
use http::{HeaderMap, HeaderValue};
use infra_grpc_core::protocol::STATUS_HEADERS;
pub use service_failure::Code;
use service_failure::{AT_CAPACITY_DETAIL, AT_CAPACITY_RETRY_AFTER, SANITIZED_DETAIL};
use tonic::Status;
use tonic_types::{BadRequest, ErrorDetail, ErrorInfo, FieldViolation, RetryInfo, StatusExt as _};

pub const ERROR_DOMAIN: &str = "service";

#[derive(Clone, Debug)]
pub struct Failure {
    code: Code,
    retry_after: Option<Duration>,
    field_violations: Vec<FieldViolation>,
}

impl Failure {
    #[must_use]
    pub const fn new(code: Code) -> Self {
        Self {
            code,
            retry_after: None,
            field_violations: Vec::new(),
        }
    }

    #[must_use]
    pub const fn retry_after(mut self, after: Duration) -> Self {
        self.retry_after = Some(after);
        self
    }

    #[must_use]
    pub fn field_violation(
        mut self,
        field: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        self.field_violations
            .push(FieldViolation::new(field, description));
        self
    }

    const fn is_plain(&self) -> bool {
        self.retry_after.is_none() && self.field_violations.is_empty()
    }

    pub(crate) fn into_status_as(self, grpc: tonic::Code, message: &'static str) -> Status {
        let details: [Option<ErrorDetail>; 3] = [
            self.retry_after
                .map(|after| RetryInfo::new(Some(after)).into()),
            Some(
                ErrorInfo::new(
                    self.code.as_str().to_ascii_uppercase(),
                    ERROR_DOMAIN,
                    HashMap::new(),
                )
                .into(),
            ),
            (!self.field_violations.is_empty())
                .then(|| BadRequest::new(self.field_violations).into()),
        ];
        Status::with_error_details_vec(grpc, message, details.into_iter().flatten())
    }

    pub(crate) fn into_classified_as(
        self,
        grpc: tonic::Code,
        message: &'static str,
    ) -> ClassifiedStatus {
        let code = self.code;
        ClassifiedStatus {
            kind: ClassifiedKind::Dynamic(self.into_status_as(grpc, message)),
            failure: Some(code),
        }
    }
}

impl From<Failure> for Status {
    fn from(failure: Failure) -> Self {
        if failure.is_plain() {
            return catalog_template(failure.code).status();
        }
        let (grpc, message) = projection(failure.code);
        failure.into_status_as(grpc, message)
    }
}

#[derive(Debug)]
struct StatusTemplate {
    status: Status,
    header_values: [Option<HeaderValue>; 3],
}

impl StatusTemplate {
    fn build(status: Status) -> Self {
        let mut encoded = HeaderMap::with_capacity(STATUS_HEADERS.len());
        let added = status.add_header(&mut encoded);
        debug_assert!(added.is_ok(), "catalog status headers must encode");
        let header_values = STATUS_HEADERS.map(|name| encoded.remove(&name));
        debug_assert!(encoded.is_empty());
        Self {
            status,
            header_values,
        }
    }

    fn status(&'static self) -> Status {
        Status::with_details(
            self.status.code(),
            self.status.message(),
            Bytes::from_static(self.status.details()),
        )
    }

    fn write_headers(&self, headers: &mut HeaderMap) {
        for (name, value) in STATUS_HEADERS.into_iter().zip(&self.header_values) {
            if let Some(value) = value {
                headers.insert(name, value.clone());
            }
        }
    }

    fn http_response<B: Default>(&'static self) -> http::Response<B> {
        let mut response = http::Response::new(B::default());
        let headers = response.headers_mut();
        headers.insert(
            http::header::CONTENT_TYPE,
            tonic::metadata::GRPC_CONTENT_TYPE,
        );
        self.write_headers(headers);
        response.extensions_mut().insert(self.status());
        response
    }
}

fn catalog_template(code: Code) -> &'static StatusTemplate {
    static TEMPLATES: EnumMap<Code, OnceLock<StatusTemplate>> =
        EnumMap::from_array([const { OnceLock::new() }; enum_map::enum_len::<Code>()]);
    TEMPLATES[code].get_or_init(|| {
        let (grpc, message) = projection(code);
        StatusTemplate::build(Failure::new(code).into_status_as(grpc, message))
    })
}

#[derive(Debug)]
enum ClassifiedKind {
    Cached(&'static StatusTemplate),
    Dynamic(Status),
}

#[derive(Debug)]
pub(crate) struct ClassifiedStatus {
    kind: ClassifiedKind,
    failure: Option<Code>,
}

impl ClassifiedStatus {
    pub(crate) fn fixed(failure: FixedFailure) -> Self {
        Self {
            kind: ClassifiedKind::Cached(failure.template()),
            failure: Some(failure.catalog_code()),
        }
    }

    pub(crate) fn plain(status: Status) -> Self {
        debug_assert!(
            status.details().is_empty(),
            "plain statuses cannot carry rich catalog details"
        );
        Self {
            kind: ClassifiedKind::Dynamic(status),
            failure: None,
        }
    }

    pub(crate) fn code(&self) -> tonic::Code {
        match &self.kind {
            ClassifiedKind::Cached(template) => template.status.code(),
            ClassifiedKind::Dynamic(status) => status.code(),
        }
    }

    pub(crate) const fn failure(&self) -> Option<Code> {
        self.failure
    }

    pub(crate) fn into_http<B: Default>(self) -> http::Response<B> {
        match self.kind {
            ClassifiedKind::Cached(template) => template.http_response(),
            ClassifiedKind::Dynamic(status) => status.into_http(),
        }
    }

    #[cfg(test)]
    pub(crate) fn status(&self) -> &Status {
        match &self.kind {
            ClassifiedKind::Cached(template) => &template.status,
            ClassifiedKind::Dynamic(status) => status,
        }
    }
}

impl From<Failure> for ClassifiedStatus {
    fn from(failure: Failure) -> Self {
        if failure.is_plain() {
            return Self {
                kind: ClassifiedKind::Cached(catalog_template(failure.code)),
                failure: Some(failure.code),
            };
        }
        let (grpc, message) = projection(failure.code);
        failure.into_classified_as(grpc, message)
    }
}

pub(crate) fn internal_status() -> Status {
    FixedFailure::Internal.status()
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum FixedFailure {
    Deadline,
    Internal,
    AtCapacity,
}

impl FixedFailure {
    pub(crate) const fn catalog_code(self) -> Code {
        match self {
            Self::Deadline => Code::GatewayTimeout,
            Self::Internal => Code::InternalServerError,
            Self::AtCapacity => Code::ServiceUnavailable,
        }
    }

    pub(crate) const fn grpc_code(self) -> tonic::Code {
        match self {
            Self::AtCapacity => tonic::Code::ResourceExhausted,
            Self::Deadline | Self::Internal => projection(self.catalog_code()).0,
        }
    }

    fn template(self) -> &'static StatusTemplate {
        static AT_CAPACITY: OnceLock<StatusTemplate> = OnceLock::new();
        match self {
            Self::Deadline | Self::Internal => catalog_template(self.catalog_code()),
            Self::AtCapacity => AT_CAPACITY.get_or_init(|| {
                StatusTemplate::build(
                    Failure::new(self.catalog_code())
                        .retry_after(AT_CAPACITY_RETRY_AFTER)
                        .into_status_as(self.grpc_code(), AT_CAPACITY_DETAIL),
                )
            }),
        }
    }

    pub(crate) fn status(self) -> Status {
        self.template().status()
    }

    pub(crate) fn write_headers(self, headers: &mut HeaderMap) {
        self.template().write_headers(headers);
    }

    pub(crate) fn into_http<B: Default>(self) -> http::Response<B> {
        self.template().http_response()
    }
}

pub(crate) fn catalog_code(status: &Status) -> Option<Code> {
    let info = status.get_details_error_info()?;
    if info.domain != ERROR_DOMAIN {
        return None;
    }
    Code::ALL
        .iter()
        .copied()
        .find(|code| code.as_str().eq_ignore_ascii_case(&info.reason))
}

#[allow(
    clippy::match_same_arms,
    reason = "Optional profiles remove complete match arms independently."
)]
const fn projection(code: Code) -> (tonic::Code, &'static str) {
    const INVALID: (tonic::Code, &str) = (tonic::Code::InvalidArgument, "request is invalid");
    const UNAUTHENTICATED: (tonic::Code, &str) =
        (tonic::Code::Unauthenticated, "authentication failed");
    const CONFLICT: (tonic::Code, &str) = (tonic::Code::Aborted, "request conflict");
    const EXHAUSTED: (tonic::Code, &str) =
        (tonic::Code::ResourceExhausted, "resource limit exceeded");
    const UNAVAILABLE: (tonic::Code, &str) = (tonic::Code::Unavailable, "service is unavailable");
    match code {
        Code::BadRequest | Code::UnsupportedMediaType | Code::UnprocessableContent => INVALID,
        // template:begin http-idempotency:grpc-idempotency-invalid-status
        Code::IdempotencyKeyMismatch => INVALID,
        // template:end http-idempotency:grpc-idempotency-invalid-status
        // template:begin inbound-webhooks:grpc-webhook-status
        Code::WebhookRejected => INVALID,
        // template:end inbound-webhooks:grpc-webhook-status
        Code::Unauthorized => UNAUTHENTICATED,
        // template:begin authn:grpc-authentication-status
        Code::AuthenticationRequired
        | Code::AuthenticationMalformed
        | Code::AuthenticationInvalid => UNAUTHENTICATED,
        Code::AuthenticationUnavailable => {
            (tonic::Code::Unavailable, "authentication is unavailable")
        }
        // template:end authn:grpc-authentication-status
        Code::Forbidden => (tonic::Code::PermissionDenied, "permission denied"),
        Code::NotFound => (tonic::Code::NotFound, "not found"),
        Code::AlreadyExists => (tonic::Code::AlreadyExists, "already exists"),
        Code::Conflict => CONFLICT,
        // template:begin http-idempotency:grpc-idempotency-conflict-status
        Code::IdempotencyRequestInProgress => CONFLICT,
        // template:end http-idempotency:grpc-idempotency-conflict-status
        Code::MethodNotAllowed => (tonic::Code::Unimplemented, "method is not implemented"),
        Code::RequestEntityTooLarge | Code::TooManyRequests => EXHAUSTED,
        Code::ServiceUnavailable => UNAVAILABLE,
        // template:begin http-idempotency:grpc-idempotency-unavailable-status
        Code::IdempotencyUnavailable => UNAVAILABLE,
        // template:end http-idempotency:grpc-idempotency-unavailable-status
        Code::RequestTimeout | Code::GatewayTimeout => {
            (tonic::Code::DeadlineExceeded, "request deadline exceeded")
        }
        Code::InternalServerError => (tonic::Code::Internal, SANITIZED_DETAIL),
    }
}

#[cfg(test)]
mod tests {
    use tonic_types::ErrorDetails;

    use super::*;

    fn assert_matches_tonic_http(actual: &http::Response<()>, expected_status: Status) {
        let expected = expected_status.into_http::<()>();
        assert_eq!(actual.status(), expected.status());
        assert_eq!(actual.headers(), expected.headers());
        let actual_ext = actual
            .extensions()
            .get::<Status>()
            .expect("tonic status extension");
        let expected_ext = expected
            .extensions()
            .get::<Status>()
            .expect("expected tonic status extension");
        assert_eq!(actual_ext.code(), expected_ext.code());
        assert_eq!(actual_ext.message(), expected_ext.message());
        assert_eq!(actual_ext.details(), expected_ext.details());
    }

    #[test]
    fn every_code_is_a_status_with_a_valid_error_info_reason() {
        for &code in Code::ALL {
            let status = Status::from(Failure::new(code));
            assert_ne!(status.code(), tonic::Code::Ok, "{code}");
            assert_ne!(status.code(), tonic::Code::Unknown, "{code}");
            assert!(!status.message().is_empty(), "{code}");
            let details = status.get_error_details();
            let info = details.error_info().expect("error info");
            assert_eq!(info.domain, ERROR_DOMAIN);
            assert!(info.metadata.is_empty());
            let reason = info.reason.as_bytes();
            assert_eq!(info.reason, code.as_str().to_ascii_uppercase());
            assert!((3..=63).contains(&reason.len()), "{code}");
            assert!(reason[0].is_ascii_uppercase(), "{code}");
            assert!(reason[reason.len() - 1] != b'_', "{code}");
            assert!(
                reason.iter().all(|byte| byte.is_ascii_uppercase()
                    || byte.is_ascii_digit()
                    || *byte == b'_'),
                "{code}"
            );
            assert!(details.retry_info().is_none());
            assert!(details.bad_request().is_none());
            assert_eq!(catalog_code(&status), Some(code));
            let classified = ClassifiedStatus::from(Failure::new(code));
            assert_eq!(classified.code(), status.code());
            assert_eq!(classified.failure(), Some(code));
            assert_matches_tonic_http(&classified.into_http::<()>(), status);
        }

        let custom = Failure::new(Code::Forbidden)
            .into_classified_as(tonic::Code::PermissionDenied, "custom refusal");
        assert_eq!(custom.code(), tonic::Code::PermissionDenied);
        assert_eq!(custom.status().message(), "custom refusal");
        assert_eq!(custom.failure(), Some(Code::Forbidden));
        assert_eq!(catalog_code(custom.status()), Some(Code::Forbidden));
        let custom_response = custom.into_http::<()>();
        assert_eq!(
            Status::from_header_map(custom_response.headers())
                .expect("custom status headers")
                .message(),
            "custom refusal"
        );

        for fixed in [
            FixedFailure::Deadline,
            FixedFailure::Internal,
            FixedFailure::AtCapacity,
        ] {
            let classified = ClassifiedStatus::fixed(fixed);
            assert_eq!(classified.code(), fixed.grpc_code());
            assert_eq!(classified.code(), fixed.status().code());
            assert_eq!(classified.failure(), Some(fixed.catalog_code()));
            assert_eq!(
                catalog_code(classified.status()),
                Some(fixed.catalog_code())
            );
            assert_matches_tonic_http(&fixed.into_http::<()>(), fixed.status());
            assert_matches_tonic_http(&classified.into_http::<()>(), fixed.status());

            let mut overwritten = HeaderMap::new();
            overwritten.insert(Status::GRPC_STATUS, HeaderValue::from_static("0"));
            overwritten.insert(Status::GRPC_MESSAGE, HeaderValue::from_static("stale"));
            overwritten.insert(
                Status::GRPC_STATUS_DETAILS,
                HeaderValue::from_static("AAAA"),
            );
            fixed.write_headers(&mut overwritten);
            let mut expected = HeaderMap::new();
            fixed.status().add_header(&mut expected).unwrap();
            assert_eq!(overwritten, expected);
        }
    }

    #[test]
    fn a_status_outside_the_catalog_has_no_catalog_code() {
        assert_eq!(catalog_code(&Status::not_found("no details")), None);
        for (reason, domain) in [
            ("NOT_FOUND", "another.service"),
            ("QUOTA_SPENT", ERROR_DOMAIN),
        ] {
            let details = ErrorDetails::with_error_info(reason, domain, HashMap::new());
            let status = Status::with_error_details(tonic::Code::NotFound, "foreign", details);
            assert_eq!(catalog_code(&status), None, "{reason} in {domain}");
        }
    }

    #[test]
    fn catalog_codes_keep_their_status_code_and_message() {
        for (code, grpc, message) in [
            (
                Code::BadRequest,
                tonic::Code::InvalidArgument,
                "request is invalid",
            ),
            (
                Code::UnsupportedMediaType,
                tonic::Code::InvalidArgument,
                "request is invalid",
            ),
            (
                Code::UnprocessableContent,
                tonic::Code::InvalidArgument,
                "request is invalid",
            ),
            (
                Code::Unauthorized,
                tonic::Code::Unauthenticated,
                "authentication failed",
            ),
            (
                Code::Forbidden,
                tonic::Code::PermissionDenied,
                "permission denied",
            ),
            (Code::NotFound, tonic::Code::NotFound, "not found"),
            (
                Code::AlreadyExists,
                tonic::Code::AlreadyExists,
                "already exists",
            ),
            (Code::Conflict, tonic::Code::Aborted, "request conflict"),
            (
                Code::MethodNotAllowed,
                tonic::Code::Unimplemented,
                "method is not implemented",
            ),
            (
                Code::RequestEntityTooLarge,
                tonic::Code::ResourceExhausted,
                "resource limit exceeded",
            ),
            (
                Code::TooManyRequests,
                tonic::Code::ResourceExhausted,
                "resource limit exceeded",
            ),
            (
                Code::ServiceUnavailable,
                tonic::Code::Unavailable,
                "service is unavailable",
            ),
            (
                Code::RequestTimeout,
                tonic::Code::DeadlineExceeded,
                "request deadline exceeded",
            ),
            (
                Code::GatewayTimeout,
                tonic::Code::DeadlineExceeded,
                "request deadline exceeded",
            ),
            (
                Code::InternalServerError,
                tonic::Code::Internal,
                "request failed",
            ),
        ] {
            let status = Status::from(Failure::new(code));
            assert_eq!(status.code(), grpc, "{code}");
            assert_eq!(status.message(), message, "{code}");
        }
    }

    #[test]
    fn retry_delay_and_field_violations_travel_as_standard_details() {
        let status = Status::from(
            Failure::new(Code::BadRequest)
                .retry_after(Duration::from_secs(2))
                .field_violation("message", "must be 1 to 1024 bytes")
                .field_violation("items[0].name", "must not be empty"),
        );
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        let details = status.get_error_details();
        assert_eq!(
            details.retry_info().and_then(|info| info.retry_delay),
            Some(Duration::from_secs(2))
        );
        let violations = &details.bad_request().expect("bad request").field_violations;
        let violations: Vec<_> = violations
            .iter()
            .map(|violation| (violation.field.as_str(), violation.description.as_str()))
            .collect();
        assert_eq!(
            violations,
            [
                ("message", "must be 1 to 1024 bytes"),
                ("items[0].name", "must not be empty")
            ]
        );

        let mut expected_details =
            ErrorDetails::with_error_info("BAD_REQUEST", ERROR_DOMAIN, HashMap::new());
        expected_details.set_retry_info(Some(Duration::from_secs(2)));
        expected_details.set_bad_request(vec![
            FieldViolation::new("message", "must be 1 to 1024 bytes"),
            FieldViolation::new("items[0].name", "must not be empty"),
        ]);
        let expected = Status::with_error_details(
            tonic::Code::InvalidArgument,
            "request is invalid",
            expected_details,
        );
        assert_eq!(status.details(), expected.details());

        let zero_retry =
            Status::from(Failure::new(Code::ServiceUnavailable).retry_after(Duration::ZERO));
        assert_eq!(
            zero_retry
                .get_error_details()
                .retry_info()
                .and_then(|info| info.retry_delay),
            Some(Duration::ZERO)
        );
        let zero_retry_classified = ClassifiedStatus::from(
            Failure::new(Code::ServiceUnavailable).retry_after(Duration::ZERO),
        );
        assert_eq!(
            zero_retry_classified
                .status()
                .get_error_details()
                .retry_info()
                .and_then(|info| info.retry_delay),
            Some(Duration::ZERO)
        );

        let clamped_retry =
            Status::from(Failure::new(Code::ServiceUnavailable).retry_after(Duration::MAX));
        assert_eq!(
            clamped_retry
                .get_error_details()
                .retry_info()
                .and_then(|info| info.retry_delay),
            Some(RetryInfo::MAX_RETRY_DELAY)
        );
    }
}
