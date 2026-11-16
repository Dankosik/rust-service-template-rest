use std::ffi::OsString;
use std::path::Path;

use super::{ENV_PREFIX, ENV_SEPARATOR, Error};

fn variable_prefix() -> String {
    format!("{ENV_PREFIX}{ENV_SEPARATOR}")
}

pub(crate) fn variable_name(key: &str) -> String {
    format!(
        "{}{}",
        variable_prefix(),
        key.to_ascii_uppercase().replace('.', ENV_SEPARATOR)
    )
}

fn is_name_byte(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-')
}

fn is_key_path(suffix: &str) -> bool {
    suffix.split(ENV_SEPARATOR).all(|segment| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|byte| is_name_byte(byte.to_ascii_lowercase()))
    })
}

pub(crate) const ADDRESSABLE_KEY_FORM: &str =
    "lowercase letters, digits, `_`, or `-`, without `__` or a trailing `_`";

pub(crate) fn is_env_addressable(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(is_name_byte)
        && !name.contains(ENV_SEPARATOR)
        && !name.ends_with('_')
}

pub(super) struct Variable {
    pub(super) original_name: String,
    pub(super) key: String,
    pub(super) value: String,
}

pub(super) type Variables = std::collections::BTreeMap<String, Variable>;

fn admit(variables: &mut Variables, name: &str, suffix: &str, value: String) -> Result<(), Error> {
    let name_key = name.to_ascii_lowercase();
    let key = suffix.to_ascii_lowercase().replace(ENV_SEPARATOR, ".");
    // template:begin authn:load-audience-list-alias
    let (name_key, key) = if key == AUDIENCE_LIST_ALIAS {
        (
            variable_name(AUDIENCE_KEY).to_ascii_lowercase(),
            AUDIENCE_KEY.to_owned(),
        )
    } else {
        (name_key, key)
    };
    // template:end authn:load-audience-list-alias
    let variable = Variable {
        original_name: name.to_owned(),
        key,
        value,
    };
    match variables.insert(name_key, variable) {
        Some(other) => Err(Error::AmbiguousName {
            name: name.to_owned(),
            other: other.original_name,
        }),
        None => Ok(()),
    }
}

// template:begin authn:load-audience-list
pub(super) const AUDIENCE_KEY: &str = "authn.audience";
const AUDIENCE_LIST_ALIAS: &str = "authn.audiences";

pub(super) fn audience_list(variables: &Variables) -> Result<Option<Vec<String>>, Error> {
    use serde::Deserialize as _;

    let name = variable_name(AUDIENCE_KEY).to_ascii_lowercase();
    let Some(variable) = variables.get(&name).filter(|variable| {
        variable
            .original_name
            .eq_ignore_ascii_case(&variable_name(AUDIENCE_LIST_ALIAS))
    }) else {
        return Ok(None);
    };
    let invalid = || {
        Error::Deserialize(format!(
            "{} must be a TOML array of strings",
            variable.original_name
        ))
    };
    let value = toml::de::ValueDeserializer::parse(&variable.value).map_err(|_| invalid())?;
    Vec::<String>::deserialize(value)
        .map(Some)
        .map_err(|_| invalid())
}
// template:end authn:load-audience-list

pub(super) fn collect_variables<I, K, V>(
    secrets_dir: Option<&Path>,
    environment: I,
) -> Result<Variables, Error>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let mut variables = match secrets_dir {
        Some(dir) => read_secrets_dir(dir)?,
        None => Variables::new(),
    };
    variables.extend(read_environment(environment)?);
    for (name, child) in &variables {
        for (end, _) in name.match_indices(ENV_SEPARATOR).skip(1) {
            if let Some(parent) = name.get(..end).and_then(|prefix| variables.get(prefix)) {
                return Err(Error::OverlappingVariables {
                    parent: parent.original_name.clone(),
                    child: child.original_name.clone(),
                });
            }
        }
    }
    Ok(variables)
}

fn read_environment<I, K, V>(environment: I) -> Result<Variables, Error>
where
    I: IntoIterator<Item = (K, V)>,
    K: Into<OsString>,
    V: Into<OsString>,
{
    let prefix = variable_prefix();
    let mut variables = Variables::new();
    for (key, value) in environment {
        let key = key.into();
        let name = key.to_string_lossy();
        let Some(suffix) = name.strip_prefix(&prefix) else {
            continue;
        };
        if !is_key_path(suffix) {
            return Err(Error::MalformedEnvName {
                name: name.into_owned(),
            });
        }
        let Ok(value) = value.into().into_string() else {
            return Err(Error::NonUnicodeEnvValue {
                name: name.into_owned(),
            });
        };
        admit(&mut variables, &name, suffix, value)?;
    }
    Ok(variables)
}

#[expect(
    clippy::disallowed_methods,
    reason = "pre-runtime configuration admission owns finite secret-directory reads; startup can wait on the filesystem"
)]
fn read_secrets_dir(dir: &Path) -> Result<Variables, Error> {
    let unreadable = |error| Error::ReadSecretsDir {
        path: dir.to_owned(),
        error,
    };
    let prefix = variable_prefix();
    let mut variables = Variables::new();
    for entry in std::fs::read_dir(dir).map_err(unreadable)? {
        let entry = entry.map_err(unreadable)?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let Some(suffix) = name.strip_prefix(&prefix) else {
            continue;
        };
        let file = entry.path();
        if !is_key_path(suffix) {
            return Err(Error::MalformedSecretName { path: file });
        }
        let value = std::fs::read_to_string(&file)
            .and_then(secret_value)
            .map_err(|error| Error::ReadSecret { path: file, error })?;
        admit(&mut variables, &name, suffix, value)?;
    }
    Ok(variables)
}

fn secret_value(text: String) -> std::io::Result<String> {
    if text.contains('\0') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the value contains a NUL byte, which no variable can hold",
        ));
    }
    Ok(shared_rules::secret_file::value(text))
}
