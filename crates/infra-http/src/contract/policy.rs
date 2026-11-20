use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, header::ALLOW};
use axum::middleware::Next;
use axum::response::Response;
use enum_map::EnumMap;
use foldhash::{HashMap, HashMapExt as _};
use itertools::Itertools as _;
use utoipa::openapi::path::Operation;
use utoipa::openapi::security::{HttpAuthScheme, SecurityScheme};
use utoipa::openapi::{OpenApi, RefOr};

use super::document::{StandardMethod, documented_methods};
use super::{FinalizeError, FinalizeKind};
// template:begin http-idempotency:http-policy-idempotency-import
use super::IdempotentOperations;
use super::document::resolve;
// template:end http-idempotency:http-policy-idempotency-import
use crate::problem::sanitized_internal_error;

// OpenAPI 3.1 §4.8.30
pub(crate) enum Access {
    Public,
    Protected(
        // template:begin authn:http-contract-protected-scopes
        crate::authn::ProtectedAccess,
        // template:end authn:http-contract-protected-scopes
    ),
}

#[derive(Clone)]
pub(crate) struct Policy(Arc<CompiledPolicy>);

struct RoutePolicy {
    standard: EnumMap<StandardMethod, Option<Access>>,
    additional: Box<[(Box<str>, Access)]>,
    extension_allow: Option<HeaderValue>,
}

impl RoutePolicy {
    fn get(&self, method: &str) -> Option<&Access> {
        match StandardMethod::parse(method) {
            Some(standard) => self.standard[standard].as_ref(),
            None => self
                .additional
                .iter()
                .find(|(name, _)| name.as_ref() == method)
                .map(|(_, access)| access),
        }
    }

    fn iter(&self) -> impl Iterator<Item = (&str, &Access)> {
        self.standard
            .iter()
            .filter_map(|(method, access)| access.as_ref().map(|access| (method.as_str(), access)))
            .chain(
                self.additional
                    .iter()
                    .map(|(method, access)| (method.as_ref(), access)),
            )
    }
}

struct CompiledPolicy {
    routes: HashMap<Box<str>, RoutePolicy>,
}

impl Policy {
    pub(super) fn compile(
        document: &OpenApi,
        // template:begin http-idempotency:http-compile-mounts-parameter
        idempotent: &IdempotentOperations,
        // template:end http-idempotency:http-compile-mounts-parameter
    ) -> Result<Self, FinalizeError> {
        let mut routes = HashMap::with_capacity(document.paths.paths.len());
        for (path, item) in &document.paths.paths {
            let path = path.as_str();
            let extension_allow = if item.query.is_some() || !item.additional_operations.is_empty()
            {
                let methods = documented_methods(item)
                    .chain((item.get.is_some() && item.head.is_none()).then_some("HEAD"))
                    .join(",");
                Some(
                    HeaderValue::from_str(&methods)
                        .map_err(|_| FinalizeError::new(FinalizeKind::InvalidRegistration))?,
                )
            } else {
                None
            };
            let mut standard = EnumMap::<StandardMethod, Option<Access>>::default();
            for method in StandardMethod::ALL {
                let Some(operation) = method.operation(item) else {
                    continue;
                };
                standard[method] = Some(compile_operation(
                    document,
                    path,
                    method.as_str(),
                    operation,
                    // template:begin http-idempotency:http-fixed-operation-mounts
                    idempotent,
                    // template:end http-idempotency:http-fixed-operation-mounts
                )?);
            }
            let mut additional = Vec::with_capacity(item.additional_operations.len());
            for (method, operation) in &item.additional_operations {
                if StandardMethod::parse(method).is_some() {
                    return Err(FinalizeError::new(FinalizeKind::InvalidPolicy {
                        path: path.to_owned(),
                        method: method.clone(),
                        fault: PolicyFault::FixedMethodInAdditionalOperations,
                    }));
                }
                additional.push((
                    method.as_str().into(),
                    compile_operation(
                        document, path, method, operation,
                        // template:begin http-idempotency:http-additional-operation-mounts
                        idempotent,
                        // template:end http-idempotency:http-additional-operation-mounts
                    )?,
                ));
            }
            routes.insert(
                path.into(),
                RoutePolicy {
                    standard,
                    additional: additional.into_boxed_slice(),
                    extension_allow,
                },
            );
        }
        Ok(Self(Arc::new(CompiledPolicy { routes })))
    }

    pub(super) fn first_protected(&self) -> Option<(&str, &str)> {
        self.0
            .routes
            .iter()
            .flat_map(|(path, route)| {
                route
                    .iter()
                    .filter(|(_, access)| matches!(access, Access::Protected(..)))
                    .map(move |(method, _)| (&**path, method))
            })
            .min()
    }

    // template:begin authn:http-policy-is-protected
    pub(super) fn is_protected(&self, path: &str, method: &str) -> bool {
        matches!(
            self.0.routes.get(path).and_then(|route| route.get(method)),
            Some(Access::Protected(..))
        )
    }
    // template:end authn:http-policy-is-protected

    pub(crate) fn unregistered_response(&self, path: Option<&str>, method: &Method) -> Response {
        if let Some(allow) = path.and_then(|path| self.0.routes.get(path)?.extension_allow.as_ref())
        {
            let mut response = crate::harden::method_not_allowed_response();
            response.headers_mut().insert(ALLOW, allow.clone());
            return response;
        }
        tracing::error!(route = path, %method, failure = "policy_absent", "http_contract_wiring_failed");
        sanitized_internal_error()
    }

    pub(crate) fn access(&self, path: &str, method: &Method) -> Option<&Access> {
        let route = self.0.routes.get(path)?;
        route.get(method.as_str()).or_else(|| {
            if method == Method::HEAD {
                route.standard[StandardMethod::Get].as_ref()
            } else {
                None
            }
        })
    }
}

pub(super) async fn enforce_public(
    State(policy): State<Policy>,
    request: Request,
    next: Next,
) -> Response {
    // template:begin authn:http-public-strip-identity
    let mut request = request;
    crate::authn::strip_identity(request.extensions_mut());
    // template:end authn:http-public-strip-identity
    let route = contract_path(request.extensions());
    let public = route
        .and_then(|path| policy.access(path, request.method()))
        .is_some_and(|access| matches!(access, Access::Public));
    if public {
        next.run(request).await
    } else {
        policy.unregistered_response(route, request.method())
    }
}

pub(crate) fn contract_path(extensions: &axum::http::Extensions) -> Option<&str> {
    Some(extensions.get::<axum::extract::MatchedPath>()?.as_str())
}

fn compile_operation(
    document: &OpenApi,
    path: &str,
    method: &str,
    operation: &Operation,
    // template:begin http-idempotency:http-compile-operation-mounts
    idempotent: &IdempotentOperations,
    // template:end http-idempotency:http-compile-operation-mounts
) -> Result<Access, FinalizeError> {
    let invalid = |fault| {
        FinalizeError::new(FinalizeKind::InvalidPolicy {
            path: path.to_owned(),
            method: method.to_owned(),
            fault,
        })
    };
    let access = Access::from_operation(document, operation).map_err(invalid)?;
    // template:begin http-idempotency:http-contract-idempotency-policy
    let is_idempotent = idempotent.contains(path, method);
    if is_idempotent && matches!(access, Access::Public) {
        return Err(invalid(PolicyFault::IdempotencyRequiresAuthentication));
    }
    if !is_idempotent && declares_idempotency(document, path, operation) {
        return Err(invalid(PolicyFault::IdempotencyRequiresRegistration));
    }
    // template:end http-idempotency:http-contract-idempotency-policy
    Ok(access)
}

// template:begin http-idempotency:http-unmounted-reserved-declarations
fn declares_idempotency(document: &OpenApi, path: &str, operation: &Operation) -> bool {
    use crate::idempotency::{KEY_HEADER, REPLAYED_HEADER};
    use utoipa::openapi::path::ParameterIn;
    let components = document.components.as_ref();
    let empty_parameters = BTreeMap::new();
    let parameters = components.map_or(&empty_parameters, |components| &components.parameters);
    let empty_responses = BTreeMap::new();
    let responses = components.map_or(&empty_responses, |components| &components.responses);
    let path_item = document.paths.paths.get(path);
    let key = path_item
        .and_then(|item| item.parameters.as_deref())
        .into_iter()
        .flatten()
        .chain(operation.parameters.iter().flatten())
        .filter_map(|value| resolve(value, parameters, "#/components/parameters/"))
        .any(|parameter| {
            parameter.parameter_in == ParameterIn::Header
                && parameter.name.eq_ignore_ascii_case(KEY_HEADER)
        });
    key || operation.responses.responses.iter().any(|(status, value)| {
        status.starts_with('2')
            && resolve(value, responses, "#/components/responses/").is_some_and(|response| {
                response
                    .headers
                    .keys()
                    .any(|name| name.eq_ignore_ascii_case(REPLAYED_HEADER))
            })
    })
}
// template:end http-idempotency:http-unmounted-reserved-declarations

const BEARER_SCHEME: &str = "bearerAuth";

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum PolicyFault {
    #[error("fixed_method_in_additional_operations")]
    FixedMethodInAdditionalOperations,
    #[error("invalid_bearer_delegation")]
    InvalidDelegation,
    #[error("invalid_scope_token")]
    InvalidScopeToken,
    #[error("delegation_requires_authentication")]
    PublicDelegation,
    #[error("missing_bearer_scheme")]
    MissingBearerScheme,
    #[error("unsupported_requirement")]
    UnsupportedRequirement,
    #[error("unreadable_requirement")]
    UnreadableRequirement,
    // template:begin http-idempotency:http-policy-idempotency-faults
    #[error("idempotency_requires_authentication")]
    IdempotencyRequiresAuthentication,
    #[error("idempotency_requires_registration")]
    IdempotencyRequiresRegistration,
    // template:end http-idempotency:http-policy-idempotency-faults
}

impl Access {
    fn from_operation(document: &OpenApi, operation: &Operation) -> Result<Self, PolicyFault> {
        let delegate = match operation
            .extensions
            .as_ref()
            .and_then(|extensions| extensions.get("x-bearer-delegation"))
        {
            Some(value) => value.as_bool().ok_or(PolicyFault::InvalidDelegation)?,
            None => false,
        };
        let security = operation.security.as_ref().or(document.security.as_ref());
        let Some(requirements) = security.filter(|requirements| !requirements.is_empty()) else {
            if delegate {
                return Err(PolicyFault::PublicDelegation);
            }
            return Ok(Self::Public);
        };
        if !declares_bearer_scheme(document) {
            return Err(PolicyFault::MissingBearerScheme);
        }
        let requirements: Vec<BTreeMap<String, Vec<String>>> = serde_json::to_value(requirements)
            .and_then(serde_json::from_value)
            .map_err(|_| PolicyFault::UnreadableRequirement)?;
        if requirements
            .iter()
            .any(|requirement| requirement.len() != 1 || !requirement.contains_key(BEARER_SCHEME))
        {
            return Err(PolicyFault::UnsupportedRequirement);
        }
        if requirements
            .iter()
            .flat_map(BTreeMap::values)
            .flatten()
            .any(|scope| !shared_rules::rfc6749::is_scope_token(scope))
        {
            return Err(PolicyFault::InvalidScopeToken);
        }
        Ok(Self::Protected(
            // template:begin authn:http-contract-protected-value
            crate::authn::ProtectedAccess {
                scope_alternatives: requirements
                    .into_iter()
                    .flat_map(BTreeMap::into_values)
                    .map(Vec::into_boxed_slice)
                    .collect(),
                delegate,
            },
            // template:end authn:http-contract-protected-value
        ))
    }
}

fn declares_bearer_scheme(document: &OpenApi) -> bool {
    document
        .components
        .as_ref()
        .and_then(|components| components.security_schemes.get(BEARER_SCHEME))
        .is_some_and(|scheme| {
            matches!(scheme, RefOr::T(SecurityScheme::Http(http)) if matches!(http.scheme, HttpAuthScheme::Bearer))
        })
}
