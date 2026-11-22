mod compose;
mod declaration;
mod execute;
mod identity;
mod input;
mod openapi;
mod stored;

#[macro_export]
macro_rules! idempotent_route {
    ($($handler:ident)::+, docs = $docs:literal $(,)?) => {{
        #[derive(utoipa::OpenApi)]
        #[openapi(paths($($handler)::+), external_docs(url = $docs))]
        struct Document;

        $crate::idempotency::route(<Document as utoipa::OpenApi>::openapi(), $($handler)::+)
    }};
}

#[macro_export]
macro_rules! json_response {
    ($vis:vis $name:ident($body:ty), status = $status:expr, description = $description:literal $(,)?) => {
        $vis struct $name {
            pub body: $body,
            pub headers: ::axum::http::HeaderMap,
        }

        impl $name {
            #[allow(
                dead_code,
                reason = "generated constructors may not all be used by every response type"
            )]
            #[must_use]
            $vis fn new(body: $body) -> Self {
                Self::with_headers(body, ::axum::http::HeaderMap::new())
            }

            #[allow(
                dead_code,
                reason = "generated constructors may not all be used by every response type"
            )]
            #[must_use]
            $vis fn with_headers(body: $body, headers: ::axum::http::HeaderMap) -> Self {
                Self { body, headers }
            }
        }

        impl ::core::convert::From<$body> for $name {
            fn from(body: $body) -> Self {
                Self::new(body)
            }
        }

        impl ::core::fmt::Debug for $name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.debug_struct(stringify!($name)).finish_non_exhaustive()
            }
        }

        const _: () = {
            #[derive(::utoipa::ToSchema)]
            #[expect(
                dead_code,
                reason = "The schema wrapper exists only for the OpenAPI derive"
            )]
            struct __InfraHttpJsonBody($body);

            impl ::utoipa::IntoResponses for $name {
                fn responses() -> ::std::collections::BTreeMap<
                    ::std::string::String,
                    ::utoipa::openapi::RefOr<::utoipa::openapi::Response>,
                > {
                    $crate::idempotency::json_responses(
                        $status,
                        $description,
                        <__InfraHttpJsonBody as ::utoipa::PartialSchema>::schema(),
                    )
                }
            }

            impl $crate::idempotency::ReplaySuccess for $name {
                fn schemas() -> ::std::vec::Vec<(
                    ::std::string::String,
                    ::utoipa::openapi::RefOr<::utoipa::openapi::Schema>,
                )> {
                    let mut schemas = ::std::vec::Vec::new();
                    <__InfraHttpJsonBody as ::utoipa::ToSchema>::schemas(&mut schemas);
                    schemas
                }

                fn capture(self) -> ::core::result::Result<
                    $crate::idempotency::ReplayableResponse,
                    $crate::idempotency::ReplayError,
                > {
                    $crate::idempotency::ReplayableResponse::json(
                        $status,
                        self.headers,
                        &self.body,
                    )
                }
            }
        };
    };
}

#[macro_export]
macro_rules! created_json_response {
    ($vis:vis $name:ident($body:ty)) => {
        $crate::json_response!(
            $vis $name($body),
            status = ::axum::http::StatusCode::CREATED,
            description = "created",
        );
    };
}

pub use compose::{CommandHandler, FromCommand, FromCommandPart, route};
pub(crate) use declaration::CompositionError;
pub use execute::{FreshCommand, HTTP_IDEMPOTENCY_OUTCOMES_METRIC, Rollback, TerminalMode};
pub use infra_postgres::BudgetedTx as Tx;
pub use input::CommandInput;
#[doc(hidden)]
pub use stored::json_responses;
pub use stored::{
    CreatedText, NoContent, ReplayError, ReplaySuccess, ReplayableResponse, TerminalProblem,
};

pub(crate) const KEY_HEADER: &str = "Idempotency-Key";
pub(crate) const REPLAYED_HEADER: &str = "Idempotent-Replayed";
pub(super) const KEY_HEADER_NAME: axum::http::HeaderName =
    axum::http::HeaderName::from_static("idempotency-key");
pub(super) const REPLAYED_HEADER_NAME: axum::http::HeaderName =
    axum::http::HeaderName::from_static("idempotent-replayed");
const REPLAYED_VALUE: &str = "true";

pub(crate) fn is_key_parameter(parameter: &utoipa::openapi::path::Parameter) -> bool {
    parameter.parameter_in == utoipa::openapi::path::ParameterIn::Header
        && parameter.name.eq_ignore_ascii_case(KEY_HEADER)
}

pub(crate) fn is_replayed_header(name: &str) -> bool {
    name.eq_ignore_ascii_case(REPLAYED_HEADER)
}
