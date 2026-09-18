//! Loader controls accepted on the command line.
//!
//! Flags select configuration sources; they never set individual keys.

use std::path::PathBuf;

use clap::Parser;

/// Command-line loader options every binary in this repository accepts.
#[derive(Clone, Debug, Default, Parser, PartialEq, Eq)]
#[command(disable_help_flag = false, disable_version_flag = true)]
pub struct LoadOptions {
    /// Base configuration file (TOML). Without it, code defaults apply.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// Overlay file applied after the base file; repeatable, applied in order.
    #[arg(long = "config-overlay", value_name = "PATH")]
    pub config_overlay: Vec<PathBuf>,
}

impl LoadOptions {
    /// Parse `args` without the program name. The composition root consumes
    /// argv0 and passes the remainder here. Positional arguments are
    /// rejected: a stray argument is usually a mistyped flag, and starting
    /// with the wrong configuration is worse than not starting.
    ///
    /// # Errors
    ///
    /// Returns the clap error, whose `Display` is the usage message.
    pub fn parse_args<I, T>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        let with_program = std::iter::once(std::ffi::OsString::from("service"))
            .chain(args.into_iter().map(Into::into));
        Self::try_parse_from(with_program)
    }

    /// Base file first, then overlays in order.
    pub fn files(&self) -> impl Iterator<Item = &PathBuf> {
        self.config.iter().chain(self.config_overlay.iter())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_base_and_ordered_overlays() {
        let opts = LoadOptions::parse_args([
            "--config",
            "base.toml",
            "--config-overlay",
            "a.toml",
            "--config-overlay",
            "b.toml",
        ])
        .unwrap();
        assert_eq!(opts.config, Some(PathBuf::from("base.toml")));
        assert_eq!(
            opts.config_overlay,
            vec![PathBuf::from("a.toml"), PathBuf::from("b.toml")]
        );
        let files: Vec<_> = opts.files().collect();
        assert_eq!(files.len(), 3);
    }

    #[test]
    fn no_flags_means_defaults_only() {
        let opts = LoadOptions::parse_args::<[&str; 0], &str>([]).unwrap();
        assert_eq!(opts, LoadOptions::default());
    }

    #[test]
    fn rejects_positional_and_unknown_arguments() {
        assert!(LoadOptions::parse_args(["stray"]).is_err());
        assert!(LoadOptions::parse_args(["--unknown"]).is_err());
        assert!(LoadOptions::parse_args(["--config"]).is_err());
    }
}
