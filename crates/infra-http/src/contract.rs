//! Final security policy compiled from the assembled upstream OpenAPI document.

use std::collections::BTreeMap;
// template:begin authn:http-contract-scope-import
use std::sync::Arc;
// template:end authn:http-contract-scope-import

use axum::Router;
use axum::extract::{Request, State};
use axum::http::Method;
use axum::middleware::{self, Next};
use axum::response::Response;
use utoipa::openapi::{OpenApi, path::Operation};
use utoipa_axum::router::OpenApiRouter;

#[cfg(test)]
use crate::problem::SANITIZED_DETAIL;
use crate::problem::sanitized_internal_error;

/// A closed finalization error; no partially served contract is returned.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("HTTP contract finalization failed")]
pub enum FinalizeError {
    /// A protected operation requires enabled authentication.
    NonPublicOperation,
    /// An operation's effective security policy is unsupported or ambiguous.
    InvalidPolicy,
}

/// Finalize a document whose operations are all public.
///
/// # Errors
/// Returns an error for protected operations or unsupported effective security.
pub fn finalize_public<S>(contract: OpenApiRouter<S>) -> Result<Router<S>, FinalizeError>
where
    S: Clone + Send + Sync + 'static,
{
    let policy = Policy::compile(contract.get_openapi())?;
    if policy.0.values().any(|methods| {
        methods
            .values()
            .any(|access| matches!(access, Access::Protected(..)))
    }) {
        return Err(FinalizeError::NonPublicOperation);
    }
    let (router, _) = contract.split_for_parts();
    if !router.has_routes() {
        return Ok(router);
    }
    Ok(router.route_layer(middleware::from_fn_with_state(policy, enforce_public)))
}

/// One operation's compiled access: open to any caller, or gated behind at
/// least one alternative of required scopes (OpenAPI 3.1 §4.8.30: OR across
/// alternatives, AND within one; an empty list is any authenticated caller).
/// Scope alternatives are retained only when authentication is enabled.
#[derive(Clone)]
pub(crate) enum Access {
    Public,
    Protected(
        // template:begin authn:http-contract-protected-scopes
        Arc<[Box<[String]>]>,
        // template:end authn:http-contract-protected-scopes
    ),
}

#[derive(Clone)]
pub(crate) struct Policy(BTreeMap<String, BTreeMap<&'static str, Access>>);

impl Policy {
    pub(crate) fn compile(document: &OpenApi) -> Result<Self, FinalizeError> {
        let mut table = BTreeMap::new();
        for (path, item) in &document.paths.paths {
            let mut methods = BTreeMap::new();
            for (method, operation) in [
                ("GET", &item.get),
                ("PUT", &item.put),
                ("POST", &item.post),
                ("DELETE", &item.delete),
                ("OPTIONS", &item.options),
                ("HEAD", &item.head),
                ("PATCH", &item.patch),
                ("TRACE", &item.trace),
            ] {
                if let Some(operation) = operation {
                    methods.insert(method, access(document, operation)?);
                }
            }
            table.insert(
                if path.is_empty() {
                    "/".to_owned()
                } else {
                    path.clone()
                },
                methods,
            );
        }
        Ok(Self(table))
    }

    pub(crate) fn access(&self, path: &str, method: &Method) -> Option<&Access> {
        let methods = self.0.get(path)?;
        methods.get(method.as_str()).or_else(|| {
            if method == Method::HEAD {
                methods.get("GET")
            } else {
                None
            }
        })
    }
}

async fn enforce_public(State(policy): State<Policy>, request: Request, next: Next) -> Response {
    let public = contract_path(request.extensions())
        .and_then(|path| policy.access(path, request.method()))
        .is_some_and(|access| matches!(access, Access::Public));
    if public {
        next.run(request).await
    } else {
        sanitized_internal_error()
    }
}

/// Normalize a finalized router mounted beneath an outer axum nest.
pub(crate) fn contract_path(extensions: &axum::http::Extensions) -> Option<&str> {
    let matched = extensions.get::<axum::extract::MatchedPath>()?.as_str();
    let inner = extensions
        .get::<axum::extract::NestedPath>()
        .and_then(|nested| matched.strip_prefix(nested.as_str()))
        .map_or(matched, |inner| if inner.is_empty() { "/" } else { inner });
    Some(inner)
}

/// An operation's effective security as one of `Access`. A requirement object
/// must have exactly one `bearerAuth` key whose value is an array of scope
/// strings (possibly empty); any other scheme, an anonymous `{}`, a
/// multi-key (AND-of-schemes) requirement, or a non-string scope is
/// unsupported and fails finalization instead of silently narrowing access.
fn access(document: &OpenApi, operation: &Operation) -> Result<Access, FinalizeError> {
    let security = operation.security.as_ref().or(document.security.as_ref());
    let Some(requirements) = security.filter(|requirements| !requirements.is_empty()) else {
        return Ok(Access::Public);
    };
    let value = serde_json::to_value(requirements).map_err(|_| FinalizeError::InvalidPolicy)?;
    let scheme = document
        .components
        .as_ref()
        .and_then(|components| components.security_schemes.get("bearerAuth"))
        .and_then(|scheme| serde_json::to_value(scheme).ok());
    let bearer_scheme = scheme
        .as_ref()
        .is_some_and(|scheme| scheme["type"] == "http" && scheme["scheme"] == "bearer");
    if !bearer_scheme {
        return Err(FinalizeError::InvalidPolicy);
    }
    let requirements = value.as_array().ok_or(FinalizeError::InvalidPolicy)?;
    // template:begin authn:http-contract-scope-alternatives
    let mut alternatives = Vec::with_capacity(requirements.len());
    // template:end authn:http-contract-scope-alternatives
    for requirement in requirements {
        let requirement = requirement
            .as_object()
            .ok_or(FinalizeError::InvalidPolicy)?;
        if requirement.len() != 1 {
            return Err(FinalizeError::InvalidPolicy);
        }
        let scopes = requirement
            .get("bearerAuth")
            .and_then(serde_json::Value::as_array)
            .ok_or(FinalizeError::InvalidPolicy)?;
        if scopes.iter().any(|scope| !scope.is_string()) {
            return Err(FinalizeError::InvalidPolicy);
        }
        // template:begin authn:http-contract-retain-scopes
        alternatives.push(
            scopes
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Box<[String]>>(),
        );
        // template:end authn:http-contract-retain-scopes
    }
    Ok(Access::Protected(
        // template:begin authn:http-contract-protected-value
        alternatives.into(),
        // template:end authn:http-contract-protected-value
    ))
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::header::{ALLOW, CONTENT_TYPE};
    use axum::http::{Method, Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use utoipa::OpenApi;

    use super::*;
    use crate::{HardenOptions, harden};

    #[derive(OpenApi)]
    struct Api;

    #[utoipa::path(
        get,
        path = "/_test/method",
        operation_id = "infraHttpContractDocumentedGet",
        security(),
        extensions(("x-security-decision" = json!({
            "exposure": "public",
            "rationale": "contract provenance test route"
        }))),
        responses((status = 200, description = "ok", content_type = "text/plain", body = String))
    )]
    async fn documented_get() -> &'static str {
        "get"
    }

    fn app(contract: OpenApiRouter) -> axum::Router {
        harden(
            finalize_public(contract).expect("the documented public contract finalizes"),
            &HardenOptions {
                max_body_bytes: 1024,
                request_timeout: Duration::from_secs(1),
                max_in_flight: NonZeroU32::new(2),
                log_health_probes: false,
            },
        )
    }

    #[test]
    fn public_finalization_checks_security_but_leaves_documentation_to_the_gate() {
        for (root, security, exposure, expected) in [
            (None, None, None, Ok(())),
            (None, Some(serde_json::json!([])), None, Ok(())),
            (
                Some(serde_json::json!([{"bearerAuth": []}])),
                Some(serde_json::json!([])),
                None,
                Ok(()),
            ),
            (
                Some(serde_json::json!([{"bearerAuth": []}])),
                None,
                None,
                Err(FinalizeError::NonPublicOperation),
            ),
            (None, Some(serde_json::json!([])), Some("protected"), Ok(())),
            (None, Some(serde_json::json!([])), Some("public"), Ok(())),
            (
                None,
                Some(serde_json::json!([{}])),
                None,
                Err(FinalizeError::InvalidPolicy),
            ),
            (
                None,
                Some(serde_json::json!([{"bearerAuth": ["write"]}])),
                None,
                Err(FinalizeError::NonPublicOperation),
            ),
            (
                None,
                Some(serde_json::json!([{"unknown": []}])),
                None,
                Err(FinalizeError::InvalidPolicy),
            ),
            (
                None,
                Some(serde_json::json!([{"bearerAuth": []}, {}])),
                None,
                Err(FinalizeError::InvalidPolicy),
            ),
        ] {
            let mut document = serde_json::json!({
                "openapi": "3.1.0",
                "info": {"title": "policy fixture", "version": "1"},
                "components": {"securitySchemes": {"bearerAuth": {"type": "http", "scheme": "bearer"}}},
                "paths": {"/_test/policy": {"get": {"responses": {"200": {"description": "ok"}}}}}
            });
            if let Some(root) = root {
                document["security"] = root;
            }
            if let Some(security) = security {
                document["paths"]["/_test/policy"]["get"]["security"] = security;
            }
            if let Some(exposure) = exposure {
                document["paths"]["/_test/policy"]["get"]["x-security-decision"] =
                    serde_json::json!({"exposure": exposure});
            }
            let contract = OpenApiRouter::<()>::with_openapi(
                serde_json::from_value(document.clone()).expect("valid OpenAPI fixture"),
            );
            assert_eq!(
                finalize_public(contract).map(|_| ()),
                expected,
                "{document}"
            );
        }
    }

    #[tokio::test]
    async fn implicit_head_inherits_get_but_native_method_fallback_remains() {
        let app = app(OpenApiRouter::with_openapi(Api::openapi())
            .routes(utoipa_axum::routes!(documented_get)));
        let head = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/_test/method")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .expect("the HEAD response");
        assert_eq!(head.status(), StatusCode::OK);
        assert!(
            head.into_body()
                .collect()
                .await
                .expect("a complete body")
                .to_bytes()
                .is_empty()
        );

        let post = app
            .oneshot(Request::post("/_test/method").body(Body::empty()).unwrap())
            .await
            .expect("the method fallback response");
        assert_eq!(post.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(
            post.headers()
                .get(ALLOW)
                .and_then(|value| value.to_str().ok()),
            Some("GET,HEAD")
        );
        assert_eq!(
            post.headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/problem+json")
        );
    }

    #[tokio::test]
    async fn finalized_contract_mounted_with_nest_keeps_its_policy() {
        let routes = finalize_public(
            OpenApiRouter::with_openapi(Api::openapi())
                .routes(utoipa_axum::routes!(documented_get)),
        )
        .expect("the documented public contract finalizes");
        let app = harden(
            axum::Router::new().nest("/mounted", routes),
            &HardenOptions {
                max_body_bytes: 1024,
                request_timeout: Duration::from_secs(1),
                max_in_flight: NonZeroU32::new(2),
                log_health_probes: false,
            },
        );
        let response = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/mounted/_test/method")
                    .body(Body::empty())
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn served_operation_missing_policy_is_sanitized() {
        let (router, document) = OpenApiRouter::with_openapi(Api::openapi())
            .routes(utoipa_axum::routes!(documented_get))
            .split_for_parts();
        let handler_1 = || async { "must not run" };
        #[allow(
            clippy::disallowed_methods,
            reason = "this concrete fixture builder is outside the application contract; handlers retain runtime checks"
        )]
        let mut contract =
            OpenApiRouter::from(router.route("/undocumented", axum::routing::post(handler_1)));
        *contract.get_openapi_mut() = document;
        let response = app(contract)
            .oneshot(
                Request::post("/undocumented")
                    .header("x-request-id", "missing-policy")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.headers()[CONTENT_TYPE], "application/problem+json");
        assert_eq!(response.headers()["x-request-id"], "missing-policy");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let problem: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(problem["code"], "internal_error");
        assert_eq!(problem["detail"], SANITIZED_DETAIL);
    }
}
