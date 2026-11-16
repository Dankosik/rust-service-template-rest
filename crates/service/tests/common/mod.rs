use std::time::{Duration, Instant};

pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .proxy(None)
        .timeout_global(Some(Duration::from_secs(3)))
        .http_status_as_error(false)
        .build()
        .new_agent()
}

/// An error status is a response, not an error.
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
