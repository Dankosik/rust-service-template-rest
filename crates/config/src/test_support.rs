//! Fixtures shared by the loader tests and the section tests that load
//! through it.

use std::path::PathBuf;

use crate::BuildInfo;

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
