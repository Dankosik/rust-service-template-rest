//! The `log` logger and the subscriber are process-wide, so this conflict
//! runs in its own test binary.

use infra_telemetry::{LoggingError, LoggingFormat, LoggingOptions, install_subscriber};

struct Taken;

impl log::Log for Taken {
    fn enabled(&self, _: &log::Metadata<'_>) -> bool {
        false
    }
    fn log(&self, _: &log::Record<'_>) {}
    fn flush(&self) {}
}

#[test]
fn a_taken_log_logger_is_reported_and_installs_nothing() {
    log::set_logger(&Taken).expect("first logger");

    let result = install_subscriber(&LoggingOptions {
        level: "info",
        format: LoggingFormat::Json,
        tracer_provider: None,
    });

    std::assert_matches!(result, Err(LoggingError::LogLoggerInstalled));
    assert!(!tracing::dispatcher::has_been_set());
}
