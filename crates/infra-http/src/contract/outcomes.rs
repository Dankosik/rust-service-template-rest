use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use utoipa::openapi::path::Operation;
use utoipa::openapi::response::Response;
use utoipa::openapi::schema::{AnyOfBuilder, Schema};
use utoipa::openapi::{Components, OpenApi, Ref, RefOr};
use utoipa::{OpenApi as _, ToResponse};

use super::components::ComponentKind;
use super::document::{path_operations_mut, resolve};
use super::{FinalizeError, FinalizeKind};
// template:begin authn:http-outcomes-policy-import
use super::Policy;
// template:end authn:http-outcomes-policy-import
use crate::probes::ProbeRoutes;
use crate::problem::{Code, PROBLEM_MEDIA_TYPE, http_status, responses};

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum ResponseFault {
    #[error("shared response could not be resolved")]
    SharedResponse,
    #[error("existing response must resolve within the service document")]
    ExistingResponse,
    #[error("existing media type must resolve within the service document")]
    ExistingMediaType,
    #[error("shared media type could not be resolved")]
    SharedMediaType,
    #[error("existing header must resolve within the service document")]
    ExistingHeader,
    #[error("shared header could not be resolved")]
    SharedHeader,
}

pub(super) fn compose(
    document: &mut OpenApi,
    probes: &ProbeRoutes,
    // template:begin authn:http-outcomes-policy-parameter
    policy: &Policy,
    // template:end authn:http-outcomes-policy-parameter
) -> Result<(), FinalizeError> {
    let components = document.components.get_or_insert_default();
    if let Some(shared) = responses::ProblemComponents::openapi().components {
        super::components::merge(components, shared)?;
    }
    for (path, item) in &mut document.paths.paths {
        let path = path.as_str();
        for (method, operation) in path_operations_mut(item) {
            // template:begin authn:http-outcomes-operation-policy
            let protected = policy.is_protected(path, method);
            // template:end authn:http-outcomes-operation-policy
            add_outcomes(
                operation,
                components,
                // template:begin authn:http-outcomes-operation-argument
                protected,
                // template:end authn:http-outcomes-operation-argument
                probes.contains_method(method, path),
            )
            .map_err(|(status, fault)| {
                FinalizeError::new(FinalizeKind::InvalidResponse {
                    path: path.to_owned(),
                    method: method.to_owned(),
                    status,
                    fault,
                })
            })?;
        }
    }
    Ok(())
}

fn add_outcomes(
    operation: &mut Operation,
    components: &Components,
    // template:begin authn:http-outcomes-protected-parameter
    protected: bool,
    // template:end authn:http-outcomes-protected-parameter
    probe: bool,
) -> Result<(), (String, ResponseFault)> {
    let json_body = declares_json_body(operation, components);
    let operation_responses = &mut operation.responses.responses;
    let unsupported_media_type = http_status(Code::UnsupportedMediaType);
    if json_body
        && operation_responses
            .get(unsupported_media_type.as_str())
            .is_none_or(|response| can_merge_json_media_type_outcome(response, components))
    {
        let (_, shared) = responses::UnsupportedMediaType::response();
        merge_status(
            operation_responses,
            components,
            unsupported_media_type.as_str(),
            shared,
        )?;
    }
    let mut add = |code: Code, (name, _): (&str, RefOr<Response>)| {
        let reference = Ref::from_response_name(name).into();
        merge_status(
            operation_responses,
            components,
            http_status(code).as_str(),
            reference,
        )
    };
    add(
        Code::RequestEntityTooLarge,
        responses::RequestEntityTooLarge::response(),
    )?;
    add(
        Code::InternalServerError,
        responses::InternalServerError::response(),
    )?;
    add(Code::GatewayTimeout, responses::GatewayTimeout::response())?;
    if operation.request_body.is_some() {
        add(Code::RequestTimeout, responses::RequestTimeout::response())?;
    }
    if !probe {
        add(
            Code::ServiceUnavailable,
            responses::AdmissionUnavailable::response(),
        )?;
    }
    // template:begin authn:http-protected-response-composition
    if protected {
        use utoipa::IntoResponses as _;

        for (status, reference) in crate::authn::ProtectedOperationProblemResponses::responses() {
            if status == http_status(Code::GatewayTimeout).as_str() {
                continue;
            }
            merge_status(operation_responses, components, &status, reference)?;
        }
    }
    // template:end authn:http-protected-response-composition
    Ok(())
}

fn declares_json_body(operation: &Operation, components: &Components) -> bool {
    operation
        .request_body
        .as_ref()
        .and_then(|body| {
            resolve(
                body,
                &components.request_bodies,
                ComponentKind::RequestBodies,
            )
        })
        .is_some_and(|body| {
            !body.content.is_empty()
                && body
                    .content
                    .keys()
                    .all(|key| crate::extract::is_json_media_type(key))
        })
}

fn can_merge_json_media_type_outcome(response: &RefOr<Response>, components: &Components) -> bool {
    resolve(response, &components.responses, ComponentKind::Responses).is_some_and(|response| {
        response
            .content
            .get(PROBLEM_MEDIA_TYPE)
            .is_none_or(|content| {
                resolve(content, &components.media_types, ComponentKind::MediaTypes).is_some()
            })
    })
}

fn merge_status(
    responses: &mut BTreeMap<String, RefOr<Response>>,
    components: &Components,
    status: &str,
    shared: RefOr<Response>,
) -> Result<(), (String, ResponseFault)> {
    merge_response(responses, components, status, shared)
        .map_err(|fault| (status.to_owned(), fault))
}

fn merge_response(
    responses: &mut BTreeMap<String, RefOr<Response>>,
    components: &Components,
    status: &str,
    shared_ref: RefOr<Response>,
) -> Result<(), ResponseFault> {
    let slot = match responses.entry(status.to_owned()) {
        Entry::Vacant(slot) => {
            slot.insert(shared_ref);
            return Ok(());
        }
        Entry::Occupied(slot) if slot.get() == &shared_ref => return Ok(()),
        Entry::Occupied(slot) => slot.into_mut(),
    };
    let shared = resolve(&shared_ref, &components.responses, ComponentKind::Responses)
        .ok_or(ResponseFault::SharedResponse)?;
    let mut merged = resolve(slot, &components.responses, ComponentKind::Responses)
        .ok_or(ResponseFault::ExistingResponse)?
        .clone();
    if let RefOr::Ref(reference) = slot {
        if !reference.description.is_empty() {
            merged.description.clone_from(&reference.description);
        }
        if !reference.summary.is_empty() {
            merged.summary = Some(reference.summary.clone());
        }
    }
    append_description(&mut merged.description, &shared.description);
    merge_response_content(&mut merged, shared, components)?;
    merge_response_headers(&mut merged, shared, components)?;
    *slot = RefOr::T(merged);
    Ok(())
}

fn merge_response_content(
    merged: &mut Response,
    shared: &Response,
    components: &Components,
) -> Result<(), ResponseFault> {
    for (media_type, content) in &shared.content {
        let Some(slot) = merged.content.get_mut(media_type) else {
            merged.content.insert(media_type.clone(), content.clone());
            continue;
        };
        if slot == content {
            continue;
        }
        let mut existing = resolve(slot, &components.media_types, ComponentKind::MediaTypes)
            .ok_or(ResponseFault::ExistingMediaType)?
            .clone();
        let shared_media_type =
            resolve(content, &components.media_types, ComponentKind::MediaTypes)
                .ok_or(ResponseFault::SharedMediaType)?;
        union_schema(&mut existing.schema, shared_media_type.schema.as_ref());
        *slot = RefOr::T(existing);
    }
    Ok(())
}

fn merge_response_headers(
    merged: &mut Response,
    shared: &Response,
    components: &Components,
) -> Result<(), ResponseFault> {
    for (name, header) in &shared.headers {
        let Some((_, slot)) = merged
            .headers
            .iter_mut()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
        else {
            merged.headers.insert(name.clone(), header.clone());
            continue;
        };
        if slot == header {
            continue;
        }
        let mut existing = resolve(slot, &components.headers, ComponentKind::Headers)
            .ok_or(ResponseFault::ExistingHeader)?
            .clone();
        let shared_header = resolve(header, &components.headers, ComponentKind::Headers)
            .ok_or(ResponseFault::SharedHeader)?;
        union_schema(&mut existing.schema, shared_header.schema.as_ref());
        if let Some(description) = &shared_header.description {
            append_description(existing.description.get_or_insert_default(), description);
        }
        *slot = RefOr::T(existing);
    }
    Ok(())
}

fn append_description(text: &mut String, addition: &str) {
    if addition.is_empty() || text.split("\n\n").any(|paragraph| paragraph == addition) {
        return;
    }
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str(addition);
}

fn union_schema(existing: &mut Option<RefOr<Schema>>, addition: Option<&RefOr<Schema>>) {
    if let (Some(old), Some(new)) = (existing.as_ref(), addition)
        && old != new
        && !matches!(old, RefOr::T(Schema::AnyOf(any)) if any.items.contains(new))
    {
        *existing = Some(
            AnyOfBuilder::new()
                .item(old.clone())
                .item(new.clone())
                .into(),
        );
    }
}
