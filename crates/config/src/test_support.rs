use std::assert_matches;
use std::path::PathBuf;

use crate::load::{Error, load_from};
use crate::{BuildInfo, Config, LoadOptions};

pub(crate) const BUILD: BuildInfo = BuildInfo {
    version: "1.2.3",
    commit: "abc123",
};

#[expect(
    clippy::disallowed_methods,
    reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
)]
pub(crate) fn write(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
    let path = dir.path().join(name);
    std::fs::write(&path, body).unwrap();
    path
}

pub(crate) fn env(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

pub(crate) fn load_env(pairs: &[(&str, &str)]) -> Config {
    load_from(&LoadOptions::default(), BUILD, env(pairs)).unwrap()
}

pub(crate) fn assert_secret_in_file(toml: &str, key: &str) {
    let dir = tempfile::tempdir().unwrap();
    let leaked = write(&dir, "leaked.toml", toml);
    let err = load_from(
        &LoadOptions {
            config: Some(leaked),
            ..LoadOptions::default()
        },
        BUILD,
        env(&[]),
    )
    .unwrap_err();
    assert_matches!(&err, Error::SecretInFile { key: found, .. } if found == key, "{err}");
}

pub(crate) fn assert_unknown_env_key_fails(variable: &str) {
    assert_matches!(
        load_from(&LoadOptions::default(), BUILD, env(&[(variable, "x")])),
        Err(Error::Deserialize(_))
    );
}
