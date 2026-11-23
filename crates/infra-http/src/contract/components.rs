use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt;

use utoipa::openapi::Components;

use super::{FinalizeError, FinalizeKind};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ComponentKind {
    Schemas,
    Responses,
    Parameters,
    Examples,
    RequestBodies,
    Headers,
    Links,
    Callbacks,
    PathItems,
    MediaTypes,
    SecuritySchemes,
    Extensions,
}

impl ComponentKind {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Schemas => "schemas",
            Self::Responses => "responses",
            Self::Parameters => "parameters",
            Self::Examples => "examples",
            Self::RequestBodies => "requestBodies",
            Self::Headers => "headers",
            Self::Links => "links",
            Self::Callbacks => "callbacks",
            Self::PathItems => "pathItems",
            Self::MediaTypes => "mediaTypes",
            Self::SecuritySchemes => "securitySchemes",
            Self::Extensions => "extensions",
        }
    }
}

impl fmt::Display for ComponentKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

pub(super) fn merge(target: &mut Components, incoming: Components) -> Result<(), FinalizeError> {
    macro_rules! collections {
        ($($field:ident => $kind:ident),+ $(,)?) => {
            $(merge_map(ComponentKind::$kind, &mut target.$field, incoming.$field)?;)+
        };
    }
    collections!(
        schemas => Schemas,
        responses => Responses,
        parameters => Parameters,
        examples => Examples,
        request_bodies => RequestBodies,
        headers => Headers,
        links => Links,
        callbacks => Callbacks,
        path_items => PathItems,
        media_types => MediaTypes,
        security_schemes => SecuritySchemes,
    );
    if let Some(extensions) = incoming.extensions {
        let target = target.extensions.get_or_insert_default();
        if let Some((name, _)) = extensions
            .iter()
            .find(|(name, value)| target.get(*name).is_some_and(|old| old != *value))
        {
            return Err(FinalizeError::new(FinalizeKind::ConflictingComponent {
                kind: ComponentKind::Extensions,
                name: name.clone(),
            }));
        }
        target.merge(extensions);
    }
    Ok(())
}

pub(crate) fn same_definition<T: PartialEq + serde::Serialize>(left: &T, right: &T) -> bool {
    left == right
        || matches!(
            (serde_json::to_value(left), serde_json::to_value(right)),
            (Ok(left), Ok(right)) if left == right
        )
}

pub(crate) fn merge_map<T: PartialEq + serde::Serialize>(
    kind: ComponentKind,
    target: &mut BTreeMap<String, T>,
    incoming: impl IntoIterator<Item = (String, T)>,
) -> Result<(), FinalizeError> {
    for (name, value) in incoming {
        match target.entry(name) {
            Entry::Vacant(entry) => {
                entry.insert(value);
            }
            Entry::Occupied(entry) if !same_definition(entry.get(), &value) => {
                return Err(FinalizeError::new(FinalizeKind::ConflictingComponent {
                    kind,
                    name: entry.key().clone(),
                }));
            }
            Entry::Occupied(_) => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::HttpContract;

    fn parameter_contract(description: &str) -> HttpContract<()> {
        let components = serde_json::from_value(json!({
            "parameters": {
                "Page": {
                    "name": "page", "in": "query", "required": false, "description": description,
                    "schema": {"type": "integer"}
                }
            },
            "pathItems": {
                "Paged": {"parameters": [{"$ref": "#/components/parameters/Page"}]}
            }
        }))
        .unwrap();
        HttpContract::default().with_components(components).unwrap()
    }

    #[test]
    fn nested_components_keep_references_and_reject_conflicting_definitions() {
        let root = HttpContract::default()
            .nest("/v1", parameter_contract("Page number"))
            .unwrap();
        let document = root
            .clone()
            .merge(parameter_contract("Page number"))
            .unwrap()
            .into_openapi()
            .unwrap();
        let components = serde_json::to_value(document.components.unwrap()).unwrap();
        let target = components["pathItems"]["Paged"]["parameters"][0]["$ref"]
            .as_str()
            .unwrap()
            .strip_prefix("#/components")
            .unwrap();
        assert_eq!(
            components.pointer(target).unwrap()["description"],
            "Page number"
        );

        assert!(matches!(
            root.merge(parameter_contract("Number of pages")),
            Err(FinalizeError(FinalizeKind::ConflictingComponent {
                kind: ComponentKind::Parameters,
                name
            })) if name == "Page"
        ));
    }
}
