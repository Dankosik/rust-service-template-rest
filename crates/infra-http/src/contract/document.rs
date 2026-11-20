use std::collections::BTreeMap;

use enum_map::Enum;
use utoipa::openapi::path::{Operation, PathItem};
use utoipa::openapi::{Ref, RefOr};

#[derive(Clone, Copy, Debug, Enum, Eq, PartialEq)]
pub(super) enum StandardMethod {
    Get,
    Put,
    Post,
    Delete,
    Options,
    Head,
    Patch,
    Trace,
    Query,
}

impl StandardMethod {
    pub(super) const ALL: [Self; 9] = [
        Self::Get,
        Self::Put,
        Self::Post,
        Self::Delete,
        Self::Options,
        Self::Head,
        Self::Patch,
        Self::Trace,
        Self::Query,
    ];

    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Put => "PUT",
            Self::Post => "POST",
            Self::Delete => "DELETE",
            Self::Options => "OPTIONS",
            Self::Head => "HEAD",
            Self::Patch => "PATCH",
            Self::Trace => "TRACE",
            Self::Query => "QUERY",
        }
    }

    pub(super) fn parse(method: &str) -> Option<Self> {
        match method {
            "GET" => Some(Self::Get),
            "PUT" => Some(Self::Put),
            "POST" => Some(Self::Post),
            "DELETE" => Some(Self::Delete),
            "OPTIONS" => Some(Self::Options),
            "HEAD" => Some(Self::Head),
            "PATCH" => Some(Self::Patch),
            "TRACE" => Some(Self::Trace),
            "QUERY" => Some(Self::Query),
            _ => None,
        }
    }

    pub(super) const fn operation(self, item: &PathItem) -> Option<&Operation> {
        match self {
            Self::Get => item.get.as_ref(),
            Self::Put => item.put.as_ref(),
            Self::Post => item.post.as_ref(),
            Self::Delete => item.delete.as_ref(),
            Self::Options => item.options.as_ref(),
            Self::Head => item.head.as_ref(),
            Self::Patch => item.patch.as_ref(),
            Self::Trace => item.trace.as_ref(),
            Self::Query => item.query.as_ref(),
        }
    }
}

pub(super) fn fixed_operations(
    item: &PathItem,
) -> impl Iterator<Item = (&'static str, &Operation)> {
    StandardMethod::ALL
        .into_iter()
        .filter_map(move |method| method.operation(item).map(|op| (method.as_str(), op)))
}

pub(super) fn documented_methods(item: &PathItem) -> impl Iterator<Item = &str> {
    fixed_operations(item)
        .map(|(method, _)| method)
        .chain(item.additional_operations.keys().map(String::as_str))
}

pub(crate) fn path_operations_mut<'a>(
    item: &'a mut PathItem,
) -> impl Iterator<Item = (&'a str, &'a mut Operation)> {
    [
        (StandardMethod::Get, &mut item.get),
        (StandardMethod::Put, &mut item.put),
        (StandardMethod::Post, &mut item.post),
        (StandardMethod::Delete, &mut item.delete),
        (StandardMethod::Options, &mut item.options),
        (StandardMethod::Head, &mut item.head),
        (StandardMethod::Patch, &mut item.patch),
        (StandardMethod::Trace, &mut item.trace),
        (StandardMethod::Query, &mut item.query),
    ]
    .into_iter()
    .filter_map(|(method, op)| -> Option<(&'a str, &'a mut Operation)> {
        op.as_mut().map(|op| (method.as_str(), op))
    })
    .chain(
        item.additional_operations
            .iter_mut()
            .map(|(method, op)| (method.as_str(), op)),
    )
}

pub(super) fn resolve<'a, T>(
    value: &'a RefOr<T>,
    components: &'a BTreeMap<String, RefOr<T>>,
    prefix: &str,
) -> Option<&'a T> {
    resolve_when(value, components, prefix, |_| true)
}

pub(super) fn resolve_when<'a, T>(
    mut value: &'a RefOr<T>,
    components: &'a BTreeMap<String, RefOr<T>>,
    prefix: &str,
    accept: impl Fn(&Ref) -> bool,
) -> Option<&'a T> {
    for _ in 0..=components.len() {
        match value {
            RefOr::T(resolved) => return Some(resolved),
            RefOr::Ref(reference) if accept(reference) => {
                value = components.get(reference.ref_location.strip_prefix(prefix)?)?;
            }
            RefOr::Ref(_) => return None,
        }
    }
    None
}
