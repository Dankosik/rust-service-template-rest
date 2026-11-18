use std::path::{Path, PathBuf};

use clap::Parser;

/// Command-line options for selecting configuration sources.
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
    pub fn try_parse_from<I, T>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        <Self as Parser>::try_parse_from(args)
    }

    pub(crate) fn files(&self) -> impl Iterator<Item = &Path> {
        self.config
            .iter()
            .chain(self.config_overlay.iter())
            .map(PathBuf::as_path)
    }
}

#[cfg(test)]
#[warn(clippy::assertions_on_result_states)]
mod tests {
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
        assert_eq!(opts.files().count(), 3);
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
        for (argument, code) in [
            ("stray", 2),
            ("--unknown", 2),
            ("--config", 2),
            ("--version", 2),
            ("--help", 0),
        ] {
            let error = LoadOptions::try_parse_from(["service", argument]).unwrap_err();
            assert_eq!(error.exit_code(), code, "{argument}");
        }
    }
}
