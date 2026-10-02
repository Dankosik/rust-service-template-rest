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
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u16,
    },
    /// Scan for active jobs absent from the declared fleet kinds.
    Unhandled {
        /// Comma-separated kinds; an explicit empty argument declares none.
        #[arg(long)]
        handled_kinds: String,
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: u16,
    },
    /// Redrive an inspected failed version after reconciling prior effects.
    Redrive {
        id: String,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        version: String,
    },
    /// Permanently abandon and delete exactly one inspected failed version.
    Discard {
        id: String,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        version: String,
    },
}
// template:end jobs:worker-cli-commands
