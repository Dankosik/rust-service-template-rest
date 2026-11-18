use std::net::SocketAddr;
use std::process::ExitCode;

use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

fn main() -> ExitCode {
    jobs_worker::run(std::env::args_os(), register)
}

fn register(
    registration: &mut jobs_worker::Registration<'_>,
) -> Result<(), jobs_worker::BuildError> {
    integration_tests::jobs::register(registration)?;
    if let Ok(address) = std::env::var("JOBS_WORKER_FIXTURE_BACKGROUND_RELEASE") {
        let address = address.parse::<SocketAddr>()?;
        registration.spawn("fixture", move |cancel| async move {
            let release = async { TcpStream::connect(address).await?.read_u8().await };
            tokio::select! {
                Ok(1) = release => cancel.cancel(),
                () = cancel.cancelled() => {}
            }
        });
    }
    Ok(())
}
