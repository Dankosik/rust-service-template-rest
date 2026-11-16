use std::io::{BufRead, BufReader};
use std::process::{Child, Command};
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

#[expect(
    clippy::disallowed_types,
    reason = "synchronous process fixture configures the spawned child binary"
)]
pub(crate) fn baseline_env(command: &mut Command) -> &mut Command {
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("APP__HTTP__ADDR", "127.0.0.1:0")
        .env("APP__OBSERVABILITY__METRICS__ADDR", "127.0.0.1:0")
        .env("APP__HTTP__READINESS_PROPAGATION_DELAY", "300ms")
        .env("APP__HTTP__REQUEST_TIMEOUT", "2s")
        .env("APP__LOG__FORMAT", "json")
}

#[expect(
    clippy::disallowed_types,
    clippy::expect_used,
    reason = "a broken fixture or setup fails the test"
)]
pub(crate) fn spawn_stdout_lines(child: &mut Child) -> (mpsc::Receiver<String>, JoinHandle<()>) {
    let stdout = child.stdout.take().expect("piped stdout");
    let (tx, lines) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    (lines, reader)
}

#[expect(
    clippy::disallowed_types,
    clippy::expect_used,
    reason = "a broken fixture or setup fails the test"
)]
pub(crate) fn terminate(child: &Child) {
    kill(Pid::from_raw(child.id().cast_signed()), Signal::SIGTERM).expect("send SIGTERM");
}

#[expect(
    clippy::disallowed_methods,
    clippy::disallowed_types,
    reason = "synchronous lifecycle fixture polls the spawned child binary over loopback HTTP"
)]
pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .proxy(None)
        .timeout_global(Some(Duration::from_secs(3)))
        .http_status_as_error(false)
        .build()
        .new_agent()
}

pub(crate) fn get(url: &str) -> Result<(u16, String), ureq::Error> {
    agent().get(url).call().map(|mut response| {
        let status = response.status().as_u16();
        let body = response.body_mut().read_to_string().unwrap_or_default();
        (status, body)
    })
}

#[expect(
    clippy::disallowed_methods,
    reason = "synchronous fixture polling waits for owned child or thread completion within its existing timeout"
)]
pub(crate) fn poll_until(url: &str, want: u16, within: Duration) -> bool {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Ok((status, _)) = get(url)
            && status == want
        {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}
