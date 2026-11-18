use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use utoipa::openapi::Components;

use super::FinalizeError;

pub(super) fn merge(target: &mut Components, incoming: Components) -> Result<(), FinalizeError> {
    macro_rules! collections {
        ($($field:ident => $name:literal),+ $(,)?) => {
            $(merge_map($name, &mut target.$field, incoming.$field)?;)+
        };
    }
    collections!(
        schemas => "schemas",
        responses => "responses",
        parameters => "parameters",
        examples => "examples",
        request_bodies => "requestBodies",
        headers => "headers",
        links => "links",
        callbacks => "callbacks",
        path_items => "pathItems",
        media_types => "mediaTypes",
        security_schemes => "securitySchemes",
    );
    if let Some(extensions) = incoming.extensions {
        let target = target.extensions.get_or_insert_default();
        if let Some((name, _)) = extensions
            .iter()
            .find(|(name, value)| target.get(*name).is_some_and(|old| old != *value))
        {
            return Err(FinalizeError::ConflictingComponent {
                kind: "extensions",
                name: name.clone(),
            });
        }
        target.merge(extensions);
    }
    Ok(())
}

pub(crate) fn merge_map<T: PartialEq>(
    kind: &'static str,
    target: &mut BTreeMap<String, T>,
    incoming: impl IntoIterator<Item = (String, T)>,
) -> Result<(), FinalizeError> {
    for (name, value) in incoming {
        match target.entry(name) {
            Entry::Vacant(entry) => {
                entry.insert(value);
            }
            Entry::Occupied(entry) if entry.get() != &value => {
                return Err(FinalizeError::ConflictingComponent {
                    kind,
                    name: entry.key().clone(),
                });
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
            Err(FinalizeError::ConflictingComponent { kind: "parameters", name }) if name == "Page"
        ));
    }
}
