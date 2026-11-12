//! The `APP__` variables of one load: the secrets directory's files, then
//! the process environment over them.

use std::ffi::OsString;
use std::path::Path;

use super::{ENV_PREFIX, ENV_SEPARATOR, Error};

fn variable_prefix() -> String {
    format!("{ENV_PREFIX}{ENV_SEPARATOR}")
}

/// The variable that sets the dotted `key`, such as `APP__HTTP__ADDR`.
pub(crate) fn variable_name(key: &str) -> String {
    format!(
        "{}{}",
        variable_prefix(),
        key.to_ascii_uppercase().replace('.', ENV_SEPARATOR)
    )
}

/// A byte a key segment may hold once a variable name is lowercased: the
/// config-rs path identifier alphabet.
fn is_name_byte(byte: u8) -> bool {
    matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-')
}

/// Whether the part of a variable name after `APP__` is a key path: every
/// segment a plain identifier. config-rs would report an empty segment as an
/// unknown field with an empty name, and would read `NAME[0]` or `A.B` as a
/// path of its own, building a key no rule here expects a variable to set.
fn is_key_path(suffix: &str) -> bool {
    suffix.split(ENV_SEPARATOR).all(|segment| {
        !segment.is_empty()
            && segment
                .bytes()
                .all(|byte| is_name_byte(byte.to_ascii_lowercase()))
    })
}

/// The form [`is_env_addressable`] admits, for the errors that refuse a key.
pub(crate) const ADDRESSABLE_KEY_FORM: &str =
    "lowercase letters, digits, `_`, or `-`, without `__` or a trailing `_`";

/// Whether an `APP__` variable can address the key `name`. The loader
/// lowercases a variable name and splits it on `__`, so a key outside that
/// form never meets its environment counterpart: the file and the
/// environment would name two entries.
pub(crate) fn is_env_addressable(name: &str) -> bool {
    !name.is_empty()
        && name.bytes().all(is_name_byte)
        && !name.contains(ENV_SEPARATOR)
        && !name.ends_with('_')
}

/// One variable of a carrier, kept under the lowercased name config-rs reads.
pub(super) struct Variable {
    spelled: String,
    /// The dotted key the variable sets, such as `http.addr`.
    pub(super) key: String,
    pub(super) value: String,
}

pub(super) type Variables = std::collections::BTreeMap<String, Variable>;

/// Record a variable whose name is `APP__` and `suffix`, refusing a second
/// spelling of its name: which of the two values won would depend on the
/// order the carrier listed them in.
fn admit(variables: &mut Variables, name: &str, suffix: &str, value: String) -> Result<(), Error> {
    let variable = Variable {
        spelled: name.to_owned(),
        key: suffix.to_lowercase().replace(ENV_SEPARATOR, "."),
        value,
    };
    match variables.insert(name.to_lowercase(), variable) {
        Some(other) => Err(Error::AmbiguousName {
            name: name.to_owned(),
            other: other.spelled,
        }),
        None => Ok(()),
    }
}

/// The `APP__` variables of one load under their lowercased names: the
/// secrets directory's files, then the process environment, whose value wins
/// for a name both carry.
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
    Ok(variables)
}

/// The `APP__*` variables of the process environment. A name that is not
/// Unicode fails the key-path check as well: its replacement character is no
/// identifier byte.
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
        // A lossy conversion would hand a credential on with its bytes changed.
        let Ok(value) = value.into().into_string() else {
            return Err(Error::NonUnicodeEnvValue {
                name: name.into_owned(),
            });
        };
        admit(&mut variables, &name, suffix, value)?;
    }
    Ok(variables)
}

/// The variables a secrets directory holds: each entry named `APP__...` is
/// one variable and its content the value. Every other entry is skipped,
/// which leaves out the `..data` links a Kubernetes volume keeps beside its
/// files. A value ends before its trailing line breaks, since most tools
/// write one; every other byte is kept, and a NUL byte is refused.
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

/// A secret file's text as a variable value: NUL is refused, since the
/// environment cannot carry it and `VALUE_FREE` relies on that; trailing line
/// breaks end the value.
fn secret_value(text: String) -> std::io::Result<String> {
    if text.contains('\0') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the value contains a NUL byte, which no variable can hold",
        ));
    }
    Ok(shared_rules::secret_file::value(text))
}
