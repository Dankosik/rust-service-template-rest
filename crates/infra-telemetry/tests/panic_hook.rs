//! The panic hook is process-wide, so it has a test binary of its own with
//! one test in it.

use std::io;
use std::sync::{Arc, Mutex};

use infra_telemetry::{PanicMessage, install_panic_hook};
use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl io::Write for Buffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("test writer mutex"))?
            .extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'writer> MakeWriter<'writer> for Buffer {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

#[test]
fn a_panic_is_one_error_record_with_its_place_and_its_message_only_when_recorded() {
    let original_hook = std::panic::take_hook();
    let record_of_a_panic = |message| {
        install_panic_hook(message);
        let buffer = Buffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(buffer.clone())
            .with_ansi(false)
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let detail = std::hint::black_box("caller text");
            std::panic::catch_unwind(|| panic!("refused {detail}")).expect_err("it panics");
        });
        let bytes = buffer.0.lock().expect("test writer mutex").clone();
        String::from_utf8(bytes).expect("the formatter writes UTF-8")
    };

    let recorded = record_of_a_panic(PanicMessage::Recorded);
    let withheld = record_of_a_panic(PanicMessage::Withheld);
    // Assertion failures should use the test runner's hook. A backtrace may
    // add continuation lines, but each panic still emits one ERROR record.
    std::panic::set_hook(original_hook);
    assert_eq!(recorded.matches(" ERROR ").count(), 1, "{recorded}");
    assert!(recorded.contains("ERROR"), "{recorded}");
    assert!(
        recorded.contains(r#"panic.message="refused caller text""#),
        "{recorded}"
    );
    assert!(recorded.contains("panic_hook.rs"), "{recorded}");
    assert!(recorded.contains("panic.line="), "{recorded}");

    assert_eq!(withheld.matches(" ERROR ").count(), 1, "{withheld}");
    assert!(!withheld.contains("caller text"), "{withheld}");
    assert!(!withheld.contains("panic.message"), "{withheld}");
    assert!(withheld.contains("panic_hook.rs"), "{withheld}");
}
