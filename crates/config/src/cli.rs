//! Loader controls accepted on the command line.
//!
//! Flags select configuration sources; they never set individual keys.
//! `--help` exits 0 and a flag error exits 2 (clap's convention), both
//! printed by clap. `--version` is not a loader flag: binaries publish
//! identity through [`crate::BuildInfo`] / `app.version`, not clap's crate
//! version.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

/// Print `message` to stderr and return [`ExitCode::FAILURE`].
///
/// Does not call `process::exit`, so destructors still run. Use this when
/// tracing is absent or not yet installed.
#[must_use]
pub fn process_failure(message: &str) -> ExitCode {
    #[allow(clippy::print_stderr)]
    {
        eprintln!("{message}");
    }
    ExitCode::FAILURE
}

/// Command-line loader options every binary in this repository accepts.
#[derive(Clone, Debug, Default, Parser, PartialEq, Eq)]
#[command(disable_version_flag = true)]
pub struct LoadOptions {
    /// Base configuration file (TOML). Without it, code defaults apply.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Overlay file applied after the base file; repeatable, applied in order.
    #[arg(long = "config-overlay", value_name = "PATH")]
    pub config_overlay: Vec<PathBuf>,

    /// Directory whose `APP__SECTION__KEY` files each hold one value, for a
    /// platform that mounts secrets as files. The environment overrides it.
    #[arg(long = "secrets-dir", value_name = "PATH")]
    pub secrets_dir: Option<PathBuf>,
}

impl LoadOptions {
    /// Parse argv, including the program name, so binaries need not depend
    /// on clap. `--help` exits 0 and a flag error exits 2; clap prints both.
    #[must_use]
    pub fn parse_from<I, T>(args: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        <Self as Parser>::parse_from(args)
    }

    /// Base file first, then overlays in order.
    pub fn files(&self) -> impl Iterator<Item = &PathBuf> {
        self.config.iter().chain(self.config_overlay.iter())
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn parses_base_and_ordered_overlays() {
        let opts = LoadOptions::try_parse_from([
            "service",
            "--config",
            "base.toml",
            "--config-overlay",
            "a.toml",
            "--config-overlay",
            "b.toml",
            "--secrets-dir",
            "/run/secrets/app",
        ])
        .unwrap();
        assert_eq!(opts.config, Some(PathBuf::from("base.toml")));
        assert_eq!(
            opts.config_overlay,
            vec![PathBuf::from("a.toml"), PathBuf::from("b.toml")]
        );
        assert_eq!(opts.secrets_dir, Some(PathBuf::from("/run/secrets/app")));
        let files: Vec<_> = opts.files().collect();
        assert_eq!(files.len(), 3);
    }

    #[test]
    fn no_flags_means_defaults_only() {
        let opts = LoadOptions::try_parse_from(["service"]).unwrap();
        assert_eq!(opts, LoadOptions::default());
    }

    #[test]
    fn usage_names_the_real_program() {
        // template:begin postgres:cli-migrate-test
        let migrate = LoadOptions::try_parse_from(["migrate", "--unknown"]).unwrap_err();
        assert!(migrate.to_string().contains("migrate"), "{migrate}");
        // template:end postgres:cli-migrate-test
        let service = LoadOptions::try_parse_from(["service", "--unknown"]).unwrap_err();
        assert!(service.to_string().contains("service"), "{service}");
    }

    #[test]
    fn rejects_positional_and_unknown_arguments() {
        assert!(LoadOptions::try_parse_from(["service", "stray"]).is_err());
        assert!(LoadOptions::try_parse_from(["service", "--unknown"]).is_err());
        assert!(LoadOptions::try_parse_from(["service", "--config"]).is_err());
        assert!(
            LoadOptions::try_parse_from(["service", "--version"]).is_err(),
            "version is not a loader flag"
        );
        assert_eq!(
            LoadOptions::try_parse_from(["service", "--help"])
                .unwrap_err()
                .exit_code(),
            0
        );
        assert_eq!(
            LoadOptions::try_parse_from(["service", "--unknown"])
                .unwrap_err()
                .exit_code(),
            2
        );
    }
}
