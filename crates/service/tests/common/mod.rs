//! Probe polling shared by the process-level tests of the built binaries.

use std::time::{Duration, Instant};

/// The status and body of one GET; an error status is a response, not an error.
pub(crate) fn get(url: &str) -> Result<(u16, String), ureq::Error> {
    ureq::get(url)
        .config()
        .timeout_global(Some(Duration::from_secs(3)))
        .http_status_as_error(false)
        .build()
        .call()
        .map(|mut response| {
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
