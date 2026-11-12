//! The subscriber is process-wide, so this runs in its own test binary.

use infra_telemetry::{LoggingError, LoggingFormat, LoggingOptions, install_subscriber};
use tracing::Dispatch;
use tracing::subscriber::NoSubscriber;

#[test]
fn a_dropped_scoped_subscriber_does_not_block_the_global_one() {
    let options = LoggingOptions {
        level: "info",
        format: LoggingFormat::Json,
        tracer_provider: None,
    };
    tracing::dispatcher::with_default(&Dispatch::new(NoSubscriber::default()), || {});

    let _guard = install_subscriber(&options).expect("first global install");

    std::assert_matches!(
        install_subscriber(&options),
        Err(LoggingError::AlreadyInstalled)
    );
}
