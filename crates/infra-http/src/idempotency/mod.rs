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
macro_rules! created_json_response {
    ($vis:vis $name:ident($body:ty)) => {
        $vis struct $name {
            pub body: $body,
            pub headers: ::axum::http::HeaderMap,
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
            struct __InfraHttpCreatedBody($body);

            impl ::utoipa::IntoResponses for $name {
                fn responses() -> ::std::collections::BTreeMap<
                    ::std::string::String,
                    ::utoipa::openapi::RefOr<::utoipa::openapi::Response>,
                > {
                    $crate::idempotency::created_json_responses(
                        <__InfraHttpCreatedBody as ::utoipa::PartialSchema>::schema(),
                    )
                }
            }

            impl $crate::idempotency::ReplaySuccess for $name {
                fn schemas() -> ::std::vec::Vec<(
                    ::std::string::String,
                    ::utoipa::openapi::RefOr<::utoipa::openapi::Schema>,
                )> {
                    let mut schemas = ::std::vec::Vec::new();
                    <__InfraHttpCreatedBody as ::utoipa::ToSchema>::schemas(&mut schemas);
                    schemas
                }

                fn capture(self) -> ::core::result::Result<
                    $crate::idempotency::ReplayableResponse,
                    $crate::idempotency::ReplayError,
                > {
                    $crate::idempotency::ReplayableResponse::json(
                        ::axum::http::StatusCode::CREATED,
                        self.headers,
                        &self.body,
                    )
                }
            }
        };
    };
}

pub use compose::route;
pub use declaration::CompositionError;
pub use execute::{FreshCommand, HTTP_IDEMPOTENCY_OUTCOMES_METRIC, Rollback, TerminalMode};
pub use infra_postgres::BudgetedTx as Tx;
pub use input::CommandInput;
#[doc(hidden)]
pub use stored::created_json_responses;
pub use stored::{CreatedText, ReplayError, ReplaySuccess, ReplayableResponse, TerminalProblem};

pub(crate) const KEY_HEADER: &str = "Idempotency-Key";
pub(crate) const REPLAYED_HEADER: &str = "Idempotent-Replayed";
pub(super) const KEY_HEADER_NAME: axum::http::HeaderName =
    axum::http::HeaderName::from_static("idempotency-key");
pub(super) const REPLAYED_HEADER_NAME: axum::http::HeaderName =
    axum::http::HeaderName::from_static("idempotent-replayed");
const REPLAYED_VALUE: &str = "true";
