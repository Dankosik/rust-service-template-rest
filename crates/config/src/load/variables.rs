use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use super::{ENV_PREFIX, ENV_SEPARATOR, Error};

const SECRET_FILE_BYTES: u64 = 1024 * 1024;
const SECRETS_SNAPSHOT_ATTEMPTS: usize = 3;

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
    pub(super) source: &'static str,
}

pub(super) type Variables = std::collections::BTreeMap<String, Variable>;

fn admit(
    variables: &mut Variables,
    name: &str,
    suffix: &str,
    value: String,
    source: &'static str,
) -> Result<(), Error> {
    let name_key = name.to_ascii_lowercase();
    let key = suffix.to_ascii_lowercase().replace(ENV_SEPARATOR, ".");
    let variable = Variable {
        original_name: name.to_owned(),
        key,
        value,
        source,
    };
    match variables.insert(name_key, variable) {
        Some(other) => Err(Error::AmbiguousName {
            name: name.to_owned(),
            other: other.original_name,
        }),
        None => Ok(()),
    }
}

pub(super) fn collect_variables(
    secrets_dir: Option<&Path>,
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<Variables, Error> {
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

fn read_environment(
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<Variables, Error> {
    let prefix = variable_prefix();
    let mut variables = Variables::new();
    for (key, value) in environment {
        let name = key.to_string_lossy();
        let Some(suffix) = name.strip_prefix(&prefix) else {
            continue;
        };
        if !is_key_path(suffix) {
            return Err(Error::MalformedEnvName {
                name: name.into_owned(),
            });
        }
        let Ok(value) = value.into_string() else {
            return Err(Error::NonUnicodeEnvValue {
                name: name.into_owned(),
            });
        };
        admit(&mut variables, &name, suffix, value, "the environment")?;
    }
    Ok(variables)
}

fn read_secrets_dir(dir: &Path) -> Result<Variables, Error> {
    if let Some(generation) = projected_generation(dir)? {
        return read_projected_secrets(dir, generation);
    }

    let variables = read_secret_files(dir, dir);
    match projected_generation(dir)? {
        Some(generation) => read_projected_secrets(dir, generation),
        None => variables,
    }
}

fn read_projected_secrets(dir: &Path, generation: PathBuf) -> Result<Variables, Error> {
    let mut generation = Some(generation);
    for attempt in 1..=SECRETS_SNAPSHOT_ATTEMPTS {
        let variables = generation
            .as_deref()
            .map(|source| read_secret_files(dir, source));
        let current = projected_generation(dir)?;
        if let Some(variables) = variables
            && current == generation
        {
            match &variables {
                Err(Error::ReadSecretsDir { error, .. } | Error::ReadSecret { error, .. })
                    if error.kind() == io::ErrorKind::NotFound
                        && attempt < SECRETS_SNAPSHOT_ATTEMPTS => {}
                _ => return variables,
            }
        }
        generation = current;
    }

    Err(Error::ReadSecretsDir {
        path: dir.to_owned(),
        error: io::Error::new(
            io::ErrorKind::Interrupted,
            "the projected secrets generation changed while reading",
        ),
    })
}

#[expect(
    clippy::disallowed_methods,
    reason = "pre-runtime configuration admission checks the volume publication link"
)]
fn projected_generation(dir: &Path) -> Result<Option<PathBuf>, Error> {
    let link = dir.join("..data");
    match fs::symlink_metadata(&link) {
        Ok(metadata) if metadata.is_symlink() => {}
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(Error::ReadSecretsDir {
                path: dir.to_owned(),
                error,
            });
        }
    }
    let target = match fs::read_link(link) {
        Ok(target) => target,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
            ) =>
        {
            return Ok(None);
        }
        Err(error) => {
            return Err(Error::ReadSecretsDir {
                path: dir.to_owned(),
                error,
            });
        }
    };
    // https://github.com/kubernetes/kubernetes/blob/master/pkg/volume/util/atomic_writer.go
    let timestamped_child = target.file_name() == Some(target.as_os_str())
        && target
            .as_os_str()
            .as_encoded_bytes()
            .strip_prefix(b"..")
            .is_some_and(|name| name.first().is_some_and(u8::is_ascii_digit));
    Ok(timestamped_child.then(|| dir.join(target)))
}

#[expect(
    clippy::disallowed_methods,
    reason = "pre-runtime configuration admission enumerates the selected secrets directory"
)]
fn read_secret_files(dir: &Path, source: &Path) -> Result<Variables, Error> {
    let unreadable = |error| Error::ReadSecretsDir {
        path: dir.to_owned(),
        error,
    };
    let prefix = variable_prefix();
    let mut variables = Variables::new();
    for entry in fs::read_dir(source).map_err(unreadable)? {
        let entry = entry.map_err(unreadable)?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let Some(suffix) = name.strip_prefix(&prefix) else {
            continue;
        };
        let file = dir.join(&file_name);
        if !is_key_path(suffix) {
            return Err(Error::MalformedSecretName { path: file });
        }
        let value = read_secret_file(&entry.path())
            .map_err(|error| Error::ReadSecret { path: file, error })?;
        admit(
            &mut variables,
            &name,
            suffix,
            value,
            "the secrets directory",
        )?;
    }
    Ok(variables)
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "the bounded startup reader validates and reads the same open descriptor"
)]
fn read_secret_file(path: &Path) -> io::Result<String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;

        options.custom_flags(nix::fcntl::OFlag::O_NONBLOCK.bits());
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a regular file",
        ));
    }
    let text = io::read_to_string(file.take(SECRET_FILE_BYTES + 1))?;
    if text.len() as u64 > SECRET_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the value exceeds the 1 MiB secret file limit",
        ));
    }
    if text.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the value contains a NUL byte, which no variable can hold",
        ));
    }
    Ok(shared_rules::secret_file::value(text))
}

#[cfg(all(test, unix))]
#[expect(
    clippy::disallowed_methods,
    reason = "the fixture rotates the mount after the reader selects a generation"
)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[rstest::rstest]
    #[case::previous_generation_present(false)]
    #[case::previous_generation_removed(true)]
    fn projected_snapshot_retries_after_publication_changes(#[case] remove_previous: bool) {
        let dir = tempfile::tempdir().unwrap();
        let old = "..2026_11_18_01_00_00.10000001";
        let next = "..2026_11_18_01_00_01.10000002";
        for (generation, level, format) in [(old, "warn", "json"), (next, "debug", "text")] {
            let path = dir.path().join(generation);
            fs::create_dir(&path).unwrap();
            fs::write(path.join("APP__LOG__LEVEL"), level).unwrap();
            fs::write(path.join("APP__LOG__FORMAT"), format).unwrap();
        }

        let active = dir.path().join("..data");
        symlink(old, &active).unwrap();
        let selected = projected_generation(dir.path()).unwrap().unwrap();
        let pending = dir.path().join("..data_tmp");
        symlink(next, &pending).unwrap();
        fs::rename(&pending, &active).unwrap();
        if remove_previous {
            fs::remove_dir_all(dir.path().join(old)).unwrap();
        }

        let snapshot = read_projected_secrets(dir.path(), selected).unwrap();
        assert_eq!(snapshot["app__log__level"].value, "debug");
        assert_eq!(snapshot["app__log__format"].value, "text");
    }
}
