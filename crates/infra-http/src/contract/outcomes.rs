use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use utoipa::openapi::path::Operation;
use utoipa::openapi::response::Response;
use utoipa::openapi::schema::{AnyOfBuilder, Schema};
use utoipa::openapi::{Components, OpenApi, Ref, RefOr};
use utoipa::{OpenApi as _, ToResponse};

use super::{FinalizeError, fixed_path_operations};
// template:begin authn:http-outcomes-policy-import
use super::{Access, Policy};
// template:end authn:http-outcomes-policy-import
use crate::probes::ProbeRoutes;
use crate::problem::responses;

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
        let path = if path.is_empty() { "/" } else { path.as_str() };
        let fixed = fixed_path_operations!(item, as_mut)
            .into_iter()
            .filter_map(|(method, operation)| operation.map(|operation| (method, operation)));
        for (method, operation) in fixed.chain(
            item.additional_operations
                .iter_mut()
                .map(|(method, operation)| (method.as_str(), operation)),
        ) {
            // template:begin authn:http-outcomes-operation-policy
            let methods = policy.0.routes.get(path);
            let protected = matches!(
                methods.and_then(|methods| methods.get(method)),
                Some(Access::Protected(..))
            );
            // template:end authn:http-outcomes-operation-policy
            add_outcomes(
                operation,
                components,
                // template:begin authn:http-outcomes-operation-argument
                protected,
                // template:end authn:http-outcomes-operation-argument
                probes.contains_method(method, path),
            )
            .map_err(|(status, fault)| FinalizeError::InvalidResponse {
                path: path.to_owned(),
                method: method.to_owned(),
                status: status.to_owned(),
                fault,
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
) -> Result<(), (&'static str, &'static str)> {
    let reads_body = operation.request_body.is_some();
    let mut add = |status, common| {
        merge_response(
            &mut operation.responses.responses,
            components,
            status,
            common,
        )
        .map_err(|fault| (status, fault))
    };
    add("413", responses::RequestEntityTooLarge::response())?;
    add("500", responses::InternalServerError::response())?;
    add("504", responses::GatewayTimeout::response())?;
    if reads_body {
        add("408", responses::RequestTimeout::response())?;
    }
    if !probe {
        add("503", responses::AdmissionUnavailable::response())?;
    }
    // template:begin authn:http-protected-response-composition
    if protected {
        use crate::authn::{
            AuthenticationForbidden, AuthenticationMalformed, AuthenticationUnauthorized,
            AuthenticationUnavailable,
        };
        add("400", AuthenticationMalformed::response())?;
        add("401", AuthenticationUnauthorized::response())?;
        add("403", AuthenticationForbidden::response())?;
        add("503", AuthenticationUnavailable::response())?;
    }
    // template:end authn:http-protected-response-composition
    Ok(())
}

fn merge_response(
    responses: &mut BTreeMap<String, RefOr<Response>>,
    components: &Components,
    status: &str,
    (name, common): (&str, RefOr<Response>),
) -> Result<(), &'static str> {
    let reference = RefOr::Ref(Ref::from_response_name(name));
    let slot = match responses.entry(status.to_owned()) {
        Entry::Vacant(slot) => {
            slot.insert(reference);
            return Ok(());
        }
        Entry::Occupied(slot) if slot.get() == &reference => return Ok(()),
        Entry::Occupied(slot) => slot.into_mut(),
    };
    let Some(common) = resolve(&common, &components.responses, "#/components/responses/") else {
        return Err("shared response could not be resolved");
    };
    let mut merged = resolve(slot, &components.responses, "#/components/responses/")
        .ok_or("existing response must resolve within the service document")?
        .clone();
    if let RefOr::Ref(reference) = slot {
        if !reference.description.is_empty() {
            merged.description.clone_from(&reference.description);
        }
        if !reference.summary.is_empty() {
            merged.summary = Some(reference.summary.clone());
        }
    }
    append_description(&mut merged.description, &common.description);
    for (media_type, content) in &common.content {
        let Some(slot) = merged.content.get_mut(media_type) else {
            merged.content.insert(media_type.clone(), content.clone());
            continue;
        };
        if slot == content {
            continue;
        }
        let mut existing = resolve(slot, &components.media_types, "#/components/mediaTypes/")
            .ok_or("existing media type must resolve within the service document")?
            .clone();
        let other = resolve(content, &components.media_types, "#/components/mediaTypes/")
            .ok_or("shared media type could not be resolved")?;
        union_schema(&mut existing.schema, other.schema.as_ref());
        *slot = RefOr::T(existing);
    }
    for (name, header) in &common.headers {
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
        let mut existing = resolve(slot, &components.headers, "#/components/headers/")
            .ok_or("existing header must resolve within the service document")?
            .clone();
        let other = resolve(header, &components.headers, "#/components/headers/")
            .ok_or("shared header could not be resolved")?;
        union_schema(&mut existing.schema, other.schema.as_ref());
        if let Some(description) = &other.description {
            append_description(existing.description.get_or_insert_default(), description);
        }
        *slot = RefOr::T(existing);
    }
    *slot = RefOr::T(merged);
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

pub(super) fn resolve<'a, T>(
    mut value: &'a RefOr<T>,
    components: &'a BTreeMap<String, RefOr<T>>,
    prefix: &str,
) -> Option<&'a T> {
    for _ in 0..=components.len() {
        match value {
            RefOr::T(resolved) => return Some(resolved),
            RefOr::Ref(reference) => {
                value = components.get(reference.ref_location.strip_prefix(prefix)?)?;
            }
        }
    }
    None
}
