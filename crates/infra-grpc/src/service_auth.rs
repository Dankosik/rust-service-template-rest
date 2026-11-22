use std::sync::Arc;

use axum::extract::Request;
use foldhash::HashSet;
use http::header::AUTHORIZATION;
use infra_serviceauth::{Caller, ServiceName, Verifier};
use operation_context::OperationContext;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;

use crate::Failure;
use crate::call::check_open;
use crate::router::reject_with_budget;
use crate::status::ClassifiedStatus;

const NOT_ADMITTED_DETAIL: &str = "the verified caller is not admitted to this method";

pub(crate) fn strip_caller(extensions: &mut http::Extensions) {
    extensions.remove::<Caller>();
}

pub(crate) async fn admit_caller(
    verifier: &Verifier,
    allowed_callers: Option<&HashSet<ServiceName>>,
    context: &OperationContext,
    mut request: Request,
) -> Result<Request, ClassifiedStatus> {
    check_open(context)?;
    let result = {
        let authorization = request
            .headers()
            .get_all(AUTHORIZATION)
            .iter()
            .map(http::HeaderValue::as_bytes);
        verifier.authenticate(authorization)
    };
    check_open(context)?;
    let caller = match result {
        Ok(caller) => caller,
        Err(failure) => {
            return Err(reject_with_budget(context, request, verification_status(failure)).await);
        }
    };
    if !allowed_callers.is_some_and(|callers| callers.contains(caller.name())) {
        let method = if allowed_callers.is_some() {
            request.uri().path()
        } else {
            "unknown"
        };
        tracing::warn!(
            caller = %caller.name(),
            method,
            "service_auth_caller_not_admitted"
        );
        let status = Failure::new(service_failure::Code::Forbidden)
            .into_classified_as(tonic::Code::PermissionDenied, NOT_ADMITTED_DETAIL);
        return Err(reject_with_budget(context, request, status).await);
    }
    tracing::Span::current().set_attribute("peer.service", Arc::<str>::from(caller.name().clone()));
    request.headers_mut().remove(AUTHORIZATION);
    request.extensions_mut().insert::<Caller>(caller);
    Ok(request)
}

fn verification_status(failure: infra_serviceauth::Failure) -> ClassifiedStatus {
    Failure::new(match failure {
        infra_serviceauth::Failure::Missing
        | infra_serviceauth::Failure::Malformed
        | infra_serviceauth::Failure::Invalid => service_failure::Code::Unauthorized,
        infra_serviceauth::Failure::Unavailable => service_failure::Code::ServiceUnavailable,
    })
    .into()
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use bytes::Bytes;
    use futures_util::StreamExt as _;
    use infra_serviceauth::{
        JwtType, TrustRegistry,
        test_support::{TestKey, now_secs, registry},
    };

    use super::*;

    const METHOD: &str = "/example.v1.EchoService/Unary";
    const TYPE: &str = "example-svc+jwt";

    fn orders_only_admission() -> (Verifier, HashSet<ServiceName>, String, String) {
        let orders = TestKey::new("orders/2026-10");
        let billing = TestKey::new("billing/2026-10");
        let trust = TrustRegistry::parse(&registry(&[
            ("orders", &[&orders]),
            ("billing", &[&billing]),
        ]))
        .unwrap();
        let verifier = Verifier::new(
            ServiceName::parse("catalog").unwrap(),
            JwtType::parse(TYPE).unwrap(),
            trust,
        );
        let allowed = HashSet::from_iter([ServiceName::parse("orders").unwrap()]);
        let now = now_secs();
        let sign = |key: &TestKey, name: &str| {
            let token = key.sign(
                &serde_json::json!({"alg": "ES256", "typ": TYPE, "kid": key.kid()}),
                &serde_json::json!({
                    "iss": name,
                    "sub": name,
                    "aud": "catalog",
                    "iat": now,
                    "exp": now + 60,
                }),
            );
            format!("Bearer {token}")
        };
        (
            verifier,
            allowed,
            sign(&billing, "billing"),
            sign(&orders, "orders"),
        )
    }

    #[tokio::test]
    async fn stopped_context_answers_before_caller_admission() {
        let (verifier, allowed, billing_bearer, orders_bearer) = orders_only_admission();
        let body_polls = Arc::new(AtomicUsize::new(0));
        let make_request = |authorization: Option<&str>| {
            let polls = Arc::clone(&body_polls);
            let stream = futures_util::stream::iter(0..2).map(move |_| {
                polls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, Infallible>(Bytes::from_static(b"frame"))
            });
            let mut builder = http::Request::post(METHOD);
            if let Some(value) = authorization {
                builder = builder.header(AUTHORIZATION, value);
            }
            builder.body(axum::body::Body::from_stream(stream)).unwrap()
        };

        for authorization in [Some(billing_bearer.as_str()), None] {
            let context = OperationContext::with_timeout(Duration::ZERO);
            let rejected = admit_caller(
                &verifier,
                Some(&allowed),
                &context,
                make_request(authorization),
            )
            .await
            .unwrap_err();
            assert_eq!(rejected.code(), tonic::Code::DeadlineExceeded);
        }

        let cancelled_context = OperationContext::unbounded();
        cancelled_context.cancellation().cancel();
        let cancelled = admit_caller(
            &verifier,
            Some(&allowed),
            &cancelled_context,
            make_request(Some(&billing_bearer)),
        )
        .await
        .unwrap_err();
        assert_eq!(cancelled.code(), tonic::Code::Cancelled);
        assert_eq!(body_polls.load(Ordering::SeqCst), 0);

        let live = OperationContext::with_timeout(Duration::from_secs(5));
        let denied = admit_caller(
            &verifier,
            Some(&allowed),
            &live,
            make_request(Some(&billing_bearer)),
        )
        .await
        .unwrap_err();
        assert_eq!(denied.code(), tonic::Code::PermissionDenied);
        assert_eq!(denied.failure(), Some(service_failure::Code::Forbidden));

        let unknown = admit_caller(&verifier, None, &live, make_request(Some(&orders_bearer)))
            .await
            .unwrap_err();
        assert_eq!(unknown.code(), tonic::Code::PermissionDenied);
        assert_eq!(unknown.failure(), Some(service_failure::Code::Forbidden));

        let admitted = admit_caller(
            &verifier,
            Some(&allowed),
            &live,
            make_request(Some(&orders_bearer)),
        )
        .await
        .unwrap();
        assert_eq!(
            admitted
                .extensions()
                .get::<Caller>()
                .unwrap()
                .name()
                .as_str(),
            "orders"
        );
        assert!(!admitted.headers().contains_key(AUTHORIZATION));
    }
}
