//! Worker loader flags and the optional PostgreSQL-only operator commands.

use clap::Parser;
use service_config::LoadOptions;

#[derive(Parser)]
#[command(disable_version_flag = true)]
pub(crate) struct WorkerArgs {
    #[command(flatten)]
    pub(crate) options: LoadOptions,
    // template:begin jobs:worker-cli-command-field
    #[command(subcommand)]
    pub(crate) command: Option<OperatorCommand>,
    // template:end jobs:worker-cli-command-field
}

// template:begin jobs:worker-cli-commands
#[derive(clap::Subcommand)]
pub(crate) enum OperatorCommand {
    /// Inspect one job without exposing its payload or stored errors.
    Inspect { id: String },
    /// Scan a bounded page for retained failed jobs.
    Failed {
        #[command(flatten)]
        page: PageArgs,
    },
    /// Scan for active jobs absent from the declared fleet kinds.
    Unhandled {
        /// Comma-separated kinds; an explicit empty argument declares none.
        #[arg(long)]
        handled_kinds: String,
        #[command(flatten)]
        page: PageArgs,
    },
    /// Redrive an inspected failed version after reconciling prior effects.
    Redrive(RecoveryArgs),
    /// Permanently abandon and delete exactly one inspected failed version.
    Discard(RecoveryArgs),
}

#[derive(clap::Args)]
pub(crate) struct PageArgs {
    #[arg(long)]
    pub(crate) after: Option<String>,
    #[arg(long, default_value_t = 100)]
    pub(crate) limit: u16,
}

#[derive(clap::Args)]
pub(crate) struct RecoveryArgs {
    pub(crate) id: String,
    #[arg(long)]
    pub(crate) kind: String,
    #[arg(long)]
    pub(crate) version: String,
}
// template:end jobs:worker-cli-commands
