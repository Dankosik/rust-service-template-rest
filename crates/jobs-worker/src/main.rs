//! The shipped jobs worker. It registers no job kind, so it refuses at
//! startup. A derived service registers its kinds here by passing
//! `Some(register)`, where `register` adds each kind with its policy and
//! handler (see docs/background-jobs.md).

use std::process::ExitCode;

fn main() -> ExitCode {
    jobs_worker::run(std::env::args_os(), None)
}
