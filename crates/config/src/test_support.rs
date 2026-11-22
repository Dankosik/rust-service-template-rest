#[cfg(feature = "test-support")]
use crate::{BuildInfo, Config, Error, LoadOptions, ShutdownTail};

#[cfg(feature = "test-support")]
pub fn service(
    environment: &[(&str, &str)],
    build: BuildInfo,
    tail: ShutdownTail,
) -> Result<Config, Error> {
    crate::load::load_with_tail_from(
        &LoadOptions::default(),
        build,
        tail,
        environment
            .iter()
            .map(|&(key, value)| (key.into(), value.into())),
    )
}

// template:begin worker:config-worker-fixture
#[cfg(feature = "test-support")]
pub fn worker(
    environment: &[(&str, &str)],
    build: BuildInfo,
    tail: ShutdownTail,
) -> Result<crate::WorkerConfig, Error> {
    crate::load::load_worker_with_tail_from(
        &LoadOptions::default(),
        build,
        tail,
        environment
            .iter()
            .map(|&(key, value)| (key.into(), value.into())),
    )
}
// template:end worker:config-worker-fixture

#[cfg(test)]
pub(crate) use unit::*;

#[cfg(test)]
mod unit {
    use std::assert_matches;
    use std::ffi::OsString;
    use std::path::PathBuf;

    use crate::load::{ErrorKind, InputError, load_from};
    use crate::{BuildInfo, Config, LoadOptions};

    pub(crate) const BUILD: BuildInfo = BuildInfo {
        version: "1.2.3",
        commit: "abc123",
    };

    #[track_caller]
    #[expect(
        clippy::disallowed_methods,
        reason = "test-owned temporary file setup or rotation completes before the corresponding fixture assertion"
    )]
    pub(crate) fn write(dir: &tempfile::TempDir, name: &str, body: &str) -> PathBuf {
        let path = dir.path().join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    pub(crate) fn env(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|&(key, value)| (key.into(), value.into()))
            .collect()
    }

    #[track_caller]
    pub(crate) fn load_env(pairs: &[(&str, &str)]) -> Config {
        load_from(&LoadOptions::default(), BUILD, env(pairs)).unwrap()
    }

    // template:begin worker:test-support-worker
    #[track_caller]
    pub(crate) fn load_worker_env(pairs: &[(&str, &str)]) -> crate::WorkerConfig {
        crate::load::load_worker_from(&LoadOptions::default(), BUILD, env(pairs)).unwrap()
    }
    // template:end worker:test-support-worker

    #[track_caller]
    #[allow(
        dead_code,
        reason = "removing every profile with secret fields leaves this assertion without a caller"
    )]
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
        assert_matches!(
            &err.0,
            ErrorKind::Input(InputError::SecretInFile { key: found, .. }) if found == key,
            "{err}"
        );
    }

    // template:begin cache:test-support-unknown-env-key
    #[track_caller]
    pub(crate) fn assert_unknown_env_key_fails(variable: &str) {
        assert_matches!(
            load_from(&LoadOptions::default(), BUILD, env(&[(variable, "x")])),
            Err(crate::load::Error(ErrorKind::Deserialize(_)))
        );
    }
    // template:end cache:test-support-unknown-env-key
}
