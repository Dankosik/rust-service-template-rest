use clap::Parser;
use service_config::LoadOptions;

#[derive(Parser)]
#[command(disable_version_flag = true)]
pub(crate) struct WorkerArgs {
    #[command(flatten)]
    pub(crate) options: LoadOptions,
}
