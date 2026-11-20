use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use itertools::Itertools as _;
use serde::de::{
    DeserializeOwned, DeserializeSeed, Deserializer, Error as _, Expected, IntoDeserializer as _,
    MapAccess, SeqAccess, Unexpected, Visitor,
};
use zeroize::Zeroizing;

use super::variables::{Variable, VariableSource, Variables};
use super::{InputError, ScannedFile};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Origin {
    File(Arc<Path>),
    Variable(VariableSource),
    MixedVariables,
    Empty,
}

impl Origin {
    fn merge(&mut self, incoming: &Self) {
        match (&*self, incoming) {
            (
                Self::Empty | Self::File(_),
                Self::File(_) | Self::Variable(_) | Self::MixedVariables,
            )
            | (Self::Variable(_), Self::MixedVariables) => *self = incoming.clone(),
            (Self::Variable(current), Self::Variable(next)) if current != next => {
                *self = Self::MixedVariables;
            }
            (_, Self::Empty) | (Self::Variable(_) | Self::MixedVariables, _) => {}
        }
    }
}

pub(super) struct Node {
    origin: Origin,
    kind: NodeKind,
}

enum NodeKind {
    String(Zeroizing<String>),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Datetime,
    Array(Vec<Node>),
    Table(BTreeMap<String, Node>),
}

impl NodeKind {
    fn unexpected(&self) -> Unexpected<'_> {
        match self {
            Self::String(value) => Unexpected::Str(value.as_str()),
            Self::Integer(value) => Unexpected::Signed(*value),
            Self::Float(value) => Unexpected::Float(*value),
            Self::Boolean(value) => Unexpected::Bool(*value),
            Self::Datetime => Unexpected::Other("TOML datetime"),
            Self::Array(_) => Unexpected::Seq,
            Self::Table(_) => Unexpected::Map,
        }
    }
}

pub(super) fn merge(files: Vec<ScannedFile>, variables: Variables) -> Result<Node, InputError> {
    let mut root = BTreeMap::new();
    let mut origin = Origin::Empty;
    let mut segments = Vec::new();
    for file in files {
        let path: Arc<Path> = Arc::from(file.path);
        origin.merge(&Origin::File(Arc::clone(&path)));
        merge_toml_table(&mut root, file.table, &path, &mut segments)?;
    }
    for variable in variables.into_values() {
        insert_variable(&mut root, &mut origin, variable)?;
    }
    Ok(Node {
        origin,
        kind: NodeKind::Table(root),
    })
}

#[cfg(test)]
pub(super) fn from_test_toml(table: toml::Table) -> Node {
    from_toml_table(table, &Arc::from(Path::new("<test>")))
}

fn merge_toml_table(
    target: &mut BTreeMap<String, Node>,
    source: toml::Table,
    path: &Arc<Path>,
    segments: &mut Vec<String>,
) -> Result<(), InputError> {
    for (key, value) in source {
        segments.push(key.clone());
        let result = match (target.entry(key), value) {
            (Entry::Occupied(mut entry), toml::Value::Table(incoming)) => {
                let node = entry.get_mut();
                if let NodeKind::Table(existing) = &mut node.kind {
                    node.origin.merge(&Origin::File(Arc::clone(path)));
                    merge_toml_table(existing, incoming, path, segments)
                } else {
                    *node = from_toml_table(incoming, path);
                    Ok(())
                }
            }
            (Entry::Occupied(mut entry), incoming) => {
                if matches!(entry.get().kind, NodeKind::Table(_)) {
                    Err(InputError::TableOverride {
                        key: segments.join("."),
                        input: path.display().to_string(),
                    })
                } else {
                    entry.insert(from_toml_value(incoming, path));
                    Ok(())
                }
            }
            (Entry::Vacant(entry), incoming) => {
                entry.insert(from_toml_value(incoming, path));
                Ok(())
            }
        };
        segments.pop();
        result?;
    }
    Ok(())
}

fn from_toml_table(table: toml::Table, path: &Arc<Path>) -> Node {
    let entries = table
        .into_iter()
        .map(|(key, value)| (key, from_toml_value(value, path)))
        .collect();
    Node {
        origin: Origin::File(Arc::clone(path)),
        kind: NodeKind::Table(entries),
    }
}

fn from_toml_value(value: toml::Value, path: &Arc<Path>) -> Node {
    let kind = match value {
        toml::Value::String(text) => NodeKind::String(Zeroizing::new(text)),
        toml::Value::Integer(number) => NodeKind::Integer(number),
        toml::Value::Float(number) => NodeKind::Float(number),
        toml::Value::Boolean(flag) => NodeKind::Boolean(flag),
        toml::Value::Datetime(_) => NodeKind::Datetime,
        toml::Value::Array(items) => NodeKind::Array(
            items
                .into_iter()
                .map(|item| from_toml_value(item, path))
                .collect(),
        ),
        toml::Value::Table(table) => return from_toml_table(table, path),
    };
    Node {
        origin: Origin::File(Arc::clone(path)),
        kind,
    }
}

struct PendingVariable<'a> {
    key: &'a str,
    original_name: &'a str,
    value: Zeroizing<String>,
    origin: Origin,
}

fn insert_variable(
    root: &mut BTreeMap<String, Node>,
    root_origin: &mut Origin,
    variable: Variable,
) -> Result<(), InputError> {
    let Variable {
        original_name,
        key,
        value,
        source,
    } = variable;
    let origin = Origin::Variable(source);
    root_origin.merge(&origin);
    let mut segments = key.split('.');
    let Some(first) = segments.next() else {
        return Ok(());
    };
    insert_variable_path(
        root,
        &mut segments,
        first,
        PendingVariable {
            key: &key,
            original_name: &original_name,
            value,
            origin,
        },
    )
}

fn insert_variable_path(
    table: &mut BTreeMap<String, Node>,
    segments: &mut std::str::Split<'_, char>,
    current: &str,
    pending: PendingVariable<'_>,
) -> Result<(), InputError> {
    let Some(next) = segments.next() else {
        if table
            .get(current)
            .is_some_and(|node| matches!(node.kind, NodeKind::Table(_)))
        {
            return Err(InputError::TableOverride {
                key: pending.key.to_owned(),
                input: pending.original_name.to_ascii_uppercase(),
            });
        }
        table.insert(
            current.to_owned(),
            Node {
                origin: pending.origin,
                kind: NodeKind::String(pending.value),
            },
        );
        return Ok(());
    };
    if let Some(node) = table.get_mut(current)
        && let NodeKind::Table(nested) = &mut node.kind
    {
        node.origin.merge(&pending.origin);
        return insert_variable_path(nested, segments, next, pending);
    }
    let origin = pending.origin.clone();
    let mut nested = BTreeMap::new();
    insert_variable_path(&mut nested, segments, next, pending)?;
    table.insert(
        current.to_owned(),
        Node {
            origin,
            kind: NodeKind::Table(nested),
        },
    );
    Ok(())
}

impl Node {
    pub(super) fn first_datetime(&self) -> Option<(String, &Path)> {
        let NodeKind::Table(table) = &self.kind else {
            return None;
        };
        find_datetime(table, &mut Vec::new())
    }

    pub(super) fn first_unknown_section(&self, retained: &[&str]) -> Option<&str> {
        let NodeKind::Table(sections) = &self.kind else {
            return None;
        };
        sections
            .keys()
            .map(String::as_str)
            .find(|key| !retained.contains(key))
    }

    pub(super) fn deserialize_into<T: DeserializeOwned>(self) -> Result<T, DecodeError> {
        let origin = self.origin.clone();
        T::deserialize(self).map_err(|error| error.with_origin(origin))
    }
}

fn find_datetime<'a>(
    table: &'a BTreeMap<String, Node>,
    segments: &mut Vec<&'a str>,
) -> Option<(String, &'a Path)> {
    for (key, node) in table {
        segments.push(key);
        let found = match &node.kind {
            NodeKind::Table(nested) => find_datetime(nested, segments),
            _ => datetime_path(node).map(|path| (segments.join("."), path)),
        };
        segments.pop();
        if found.is_some() {
            return found;
        }
    }
    None
}

fn datetime_path(node: &Node) -> Option<&Path> {
    match (&node.kind, &node.origin) {
        (NodeKind::Datetime, Origin::File(path)) => Some(path.as_ref()),
        (NodeKind::Array(items), _) => items.iter().find_map(datetime_path),
        _ => None,
    }
}

pub(super) struct DecodeError {
    detail: DecodeDetail,
    segments: Vec<PathSegment>,
    origin: Option<Origin>,
}

enum DecodeDetail {
    KeysOnly(String),
    Expected {
        full: Zeroizing<String>,
        expected: String,
    },
    Opaque(Zeroizing<String>),
}

enum PathSegment {
    Key(String),
    Index(usize),
}

impl DecodeError {
    const fn new(detail: DecodeDetail) -> Self {
        Self {
            detail,
            segments: Vec::new(),
            origin: None,
        }
    }

    fn with_origin(mut self, origin: Origin) -> Self {
        self.origin.get_or_insert(origin);
        self
    }

    fn push_key(mut self, key: String) -> Self {
        self.segments.push(PathSegment::Key(key));
        self
    }

    fn push_index(mut self, index: usize) -> Self {
        self.segments.push(PathSegment::Index(index));
        self
    }

    pub(super) fn render(&self) -> String {
        let place =
            format_path(&self.segments).map_or_else(String::new, |key| format!(" for key `{key}`"));
        let origin_suffix = match &self.origin {
            Some(Origin::Variable(source)) => format!(" in {}", source.as_str()),
            Some(Origin::MixedVariables) => format!(
                " in {} or {}",
                VariableSource::Environment.as_str(),
                VariableSource::SecretsDir.as_str(),
            ),
            Some(Origin::File(_) | Origin::Empty) | None => String::new(),
        };
        match (&self.detail, &self.origin) {
            (DecodeDetail::KeysOnly(message), _) => format!("{message}{place}"),
            (
                DecodeDetail::Expected { full, .. } | DecodeDetail::Opaque(full),
                Some(Origin::File(path)),
            ) => format!("{}{place} in {}", full.as_str(), path.display()),
            (DecodeDetail::Expected { expected, .. }, _) => {
                format!("invalid value, expected {expected}{place}{origin_suffix}")
            }
            (DecodeDetail::Opaque(_), _) => format!("invalid value{place}{origin_suffix}"),
        }
    }
}

impl std::fmt::Debug for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("DecodeError").field(&self.render()).finish()
    }
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.render())
    }
}

impl std::error::Error for DecodeError {}

impl serde::de::Error for DecodeError {
    fn custom<T: std::fmt::Display>(msg: T) -> Self {
        Self::new(DecodeDetail::Opaque(Zeroizing::new(msg.to_string())))
    }

    fn invalid_type(unexp: Unexpected<'_>, exp: &dyn Expected) -> Self {
        Self::new(DecodeDetail::Expected {
            full: Zeroizing::new(format!("invalid type: {unexp}, expected {exp}")),
            expected: exp.to_string(),
        })
    }

    fn invalid_value(unexp: Unexpected<'_>, exp: &dyn Expected) -> Self {
        Self::new(DecodeDetail::Expected {
            full: Zeroizing::new(format!("invalid value: {unexp}, expected {exp}")),
            expected: exp.to_string(),
        })
    }

    fn invalid_length(len: usize, exp: &dyn Expected) -> Self {
        Self::new(DecodeDetail::Opaque(Zeroizing::new(format!(
            "invalid length {len}, expected {exp}"
        ))))
    }

    fn unknown_variant(variant: &str, expected: &'static [&'static str]) -> Self {
        if expected.is_empty() {
            return Self::new(DecodeDetail::Opaque(Zeroizing::new(format!(
                "unknown variant `{variant}`, there are no variants"
            ))));
        }
        let expected = format_one_of(expected);
        Self::new(DecodeDetail::Expected {
            full: Zeroizing::new(format!("unknown variant `{variant}`, expected {expected}")),
            expected,
        })
    }

    fn unknown_field(field: &str, expected: &'static [&'static str]) -> Self {
        let message = if expected.is_empty() {
            format!("unknown field `{field}`, there are no fields")
        } else {
            format!(
                "unknown field `{field}`, expected {}",
                format_one_of(expected)
            )
        };
        Self::new(DecodeDetail::KeysOnly(message))
    }

    fn missing_field(field: &'static str) -> Self {
        Self::new(DecodeDetail::KeysOnly(format!("missing field `{field}`")))
    }

    fn duplicate_field(field: &'static str) -> Self {
        Self::new(DecodeDetail::KeysOnly(format!("duplicate field `{field}`")))
    }
}

fn format_one_of(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [only] => format!("`{only}`"),
        [first, second] => format!("`{first}` or `{second}`"),
        many => format!(
            "one of {}",
            many.iter()
                .format_with(", ", |name, f| f(&format_args!("`{name}`")))
        ),
    }
}

fn format_path(segments: &[PathSegment]) -> Option<String> {
    if segments.is_empty() {
        return None;
    }
    let mut path = String::new();
    for segment in segments.iter().rev() {
        match segment {
            PathSegment::Key(key) => {
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(key);
            }
            PathSegment::Index(index) => {
                let _ = write!(path, "[{index}]");
            }
        }
    }
    Some(path)
}

impl<'de> Deserializer<'de> for Node {
    type Error = DecodeError;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.kind {
            NodeKind::String(mut value) => visitor.visit_string(std::mem::take(&mut *value)),
            NodeKind::Integer(value) => visitor.visit_i64(value),
            NodeKind::Float(value) => visitor.visit_f64(value),
            NodeKind::Boolean(value) => visitor.visit_bool(value),
            NodeKind::Datetime => Err(DecodeError::invalid_type(
                Unexpected::Other("TOML datetime"),
                &visitor,
            )),
            NodeKind::Array(items) => visitor.visit_seq(NodeSeq {
                items: items.into_iter(),
                index: 0,
            }),
            NodeKind::Table(entries) => visitor.visit_map(NodeMap {
                entries: entries.into_iter(),
                pending: None,
            }),
        }
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_some(self)
    }

    fn deserialize_newtype_struct<V>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        match self.kind {
            NodeKind::String(value) => visitor.visit_enum(value.as_str().into_deserializer()),
            other => Err(DecodeError::invalid_type(other.unexpected(), &"a string")),
        }
    }

    fn deserialize_ignored_any<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        visitor.visit_unit()
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct seq tuple tuple_struct map struct
        identifier
    }
}

struct NodeMap {
    entries: std::collections::btree_map::IntoIter<String, Node>,
    pending: Option<(String, Node)>,
}

impl<'de> MapAccess<'de> for NodeMap {
    type Error = DecodeError;

    fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
    where
        K: DeserializeSeed<'de>,
    {
        let Some((key, node)) = self.entries.next() else {
            return Ok(None);
        };
        let deserialized = seed.deserialize(key.as_str().into_deserializer())?;
        self.pending = Some((key, node));
        Ok(Some(deserialized))
    }

    fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
    where
        V: DeserializeSeed<'de>,
    {
        let (key, node) = self
            .pending
            .take()
            .ok_or_else(|| DecodeError::custom("map value requested without a key"))?;
        let origin = node.origin.clone();
        seed.deserialize(node)
            .map_err(|error| error.with_origin(origin).push_key(key))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.entries.len())
    }
}

struct NodeSeq {
    items: std::vec::IntoIter<Node>,
    index: usize,
}

impl<'de> SeqAccess<'de> for NodeSeq {
    type Error = DecodeError;

    fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
    where
        T: DeserializeSeed<'de>,
    {
        let Some(node) = self.items.next() else {
            return Ok(None);
        };
        let index = self.index;
        self.index += 1;
        let origin = node.origin.clone();
        seed.deserialize(node)
            .map(Some)
            .map_err(|error| error.with_origin(origin).push_index(index))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.items.len())
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    const VALUE: &str = "hunter2";

    fn in_env(error: DecodeError) -> DecodeError {
        error.with_origin(Origin::Variable(VariableSource::Environment))
    }

    #[test]
    fn serde_value_forms_keep_the_expected_form_and_drop_the_value() {
        for error in [
            DecodeError::invalid_type(Unexpected::Str(VALUE), &"a boolean"),
            DecodeError::invalid_value(Unexpected::Str(VALUE), &"a boolean"),
        ] {
            let error = in_env(error);
            assert_eq!(
                error.render(),
                "invalid value, expected a boolean in the environment"
            );
            assert!(!format!("{error:?}").contains(VALUE), "{error:?}");
        }
        let variant = in_env(DecodeError::unknown_variant(VALUE, &["json", "text"]));
        assert_eq!(
            variant.render(),
            "invalid value, expected `json` or `text` in the environment"
        );
        assert!(!format!("{variant:?}").contains(VALUE), "{variant:?}");
    }

    #[test]
    fn serde_key_forms_are_shown_as_written() {
        assert_eq!(
            DecodeError::unknown_field("levl", &["level"]).render(),
            "unknown field `levl`, expected `level`"
        );
        assert_eq!(
            DecodeError::unknown_field("levl", &["a", "b", "c"]).render(),
            "unknown field `levl`, expected one of `a`, `b`, `c`"
        );
        assert_eq!(
            DecodeError::missing_field("level").render(),
            "missing field `level`"
        );
        assert_eq!(
            DecodeError::duplicate_field("level").render(),
            "duplicate field `level`"
        );
    }

    #[test]
    fn an_unrecognised_form_hides_its_text() {
        for error in [
            DecodeError::invalid_length(3, &VALUE),
            DecodeError::custom(VALUE),
        ] {
            let error = in_env(error);
            assert_eq!(error.render(), "invalid value in the environment");
            assert!(!format!("{error:?}").contains(VALUE), "{error:?}");
        }
    }

    #[test]
    fn mixed_variable_origins_name_both_carriers() {
        let error = DecodeError::invalid_type(Unexpected::Str(VALUE), &"a boolean")
            .with_origin(Origin::MixedVariables)
            .push_key("flag".to_owned());
        assert_eq!(
            error.render(),
            "invalid value, expected a boolean for key `flag` in the environment or the secrets directory"
        );
    }

    #[test]
    fn node_deserializer_rejects_non_strings_for_string_and_enum_fields() {
        #[derive(Debug, Deserialize, PartialEq, Eq)]
        #[serde(rename_all = "lowercase")]
        enum Kind {
            Alpha,
        }

        #[derive(Debug, Default, Deserialize)]
        #[serde(default)]
        struct Probe {
            text: String,
            kind: Option<Kind>,
        }

        let valid: Probe =
            from_test_toml(toml::from_str("text = \"ok\"\nkind = \"alpha\"\n").unwrap())
                .deserialize_into()
                .unwrap();
        assert_eq!(valid.text, "ok");
        assert_eq!(valid.kind, Some(Kind::Alpha));

        for body in [
            "text = 123\n",
            "text = true\n",
            "kind = 1\n",
            "kind = false\n",
        ] {
            let table: toml::Table = toml::from_str(body).unwrap();
            let err = from_test_toml(table)
                .deserialize_into::<Probe>()
                .unwrap_err()
                .render();
            assert!(err.contains("expected a string"), "{body}: {err}");
        }
    }
}
