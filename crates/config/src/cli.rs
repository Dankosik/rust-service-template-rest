//! Loader controls accepted on the command line.
//!
//! Flags select configuration sources; they never set individual keys.
//! `--version` is not a loader flag: binaries publish identity through
//! [`crate::BuildInfo`] / `app.version`, not clap's crate version.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;

/// Process argv outcome: either loader options or a process exit code.
///
/// `--help` is [`Self::Exit`] with [`ExitCode::SUCCESS`], not an `Err`.
#[derive(Debug)]
pub enum FromArgs {
    /// Parsed loader flags; continue startup.
    Run(LoadOptions),
    /// Printed clap's message; return this code from `main`.
    Exit(ExitCode),
}

impl FromArgs {
    /// Parse argv, printing clap's message when clap does not yield options.
    ///
    /// `--help` is [`Self::Exit`] with success; other clap errors are
    /// [`Self::Exit`] with failure. Does not call `process::exit`, so
    /// destructors still run.
    pub fn from_argv<I, T>(args: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        match LoadOptions::parse_args(args) {
            Ok(options) => Self::Run(options),
            Err(err) => Self::Exit(LoadOptions::clap_exit(&err)),
        }
    }
}

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
    /// Parse the process argv, including the program name. Positional
    /// arguments are rejected: a stray argument is usually a mistyped flag,
    /// and starting with the wrong configuration is worse than not starting.
    ///
    /// Returns [`clap::Error`] instead of exiting so destructors still run.
    /// Production binaries should call [`FromArgs::from_argv`], which prints
    /// and maps the outcome without naming clap at the call site.
    ///
    /// # Errors
    ///
    /// Returns the clap error, whose `Display` is the usage message.
    pub fn parse_args<I, T>(args: I) -> Result<Self, clap::Error>
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        Self::try_parse_from(args)
    }

    fn clap_exit(err: &clap::Error) -> ExitCode {
        let success = err.exit_code() == 0;
        let _ = err.print();
        if success {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        }
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
            "service",
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
        let opts = LoadOptions::parse_args(["service"]).unwrap();
        assert_eq!(opts, LoadOptions::default());
    }

    #[test]
    fn usage_names_the_real_program() {
        let migrate = LoadOptions::parse_args(["migrate", "--unknown"]).unwrap_err();
        assert!(migrate.to_string().contains("migrate"), "{migrate}");
        let service = LoadOptions::parse_args(["service", "--unknown"]).unwrap_err();
        assert!(service.to_string().contains("service"), "{service}");
    }

    #[test]
    fn rejects_positional_and_unknown_arguments() {
        assert!(LoadOptions::parse_args(["service", "stray"]).is_err());
        assert!(LoadOptions::parse_args(["service", "--unknown"]).is_err());
        assert!(LoadOptions::parse_args(["service", "--config"]).is_err());
        assert!(
            LoadOptions::parse_args(["service", "--version"]).is_err(),
            "version is not a loader flag"
        );
    }

    #[test]
    fn from_args_maps_unknown_flag_to_failure() {
        assert!(matches!(
            FromArgs::from_argv(["service", "--unknown"]),
            FromArgs::Exit(_)
        ));
        assert_ne!(
            LoadOptions::parse_args(["service", "--unknown"])
                .unwrap_err()
                .exit_code(),
            0
        );
    }

    #[test]
    fn from_args_maps_help_to_success() {
        assert!(matches!(
            FromArgs::from_argv(["service", "--help"]),
            FromArgs::Exit(_)
        ));
        assert_eq!(
            LoadOptions::parse_args(["service", "--help"])
                .unwrap_err()
                .exit_code(),
            0
        );
    }
}
