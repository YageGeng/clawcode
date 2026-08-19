use std::io::{self, Write};
use std::sync::{Arc, Mutex};

/// Shared in-memory writer used to inspect formatted tracing output.
#[derive(Clone, Default)]
pub struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl CapturedLogs {
    /// Returns all UTF-8 tracing output written by the subscriber.
    pub fn content(&self) -> String {
        String::from_utf8(self.0.lock().expect("log lock").clone())
            .expect("UTF-8 logs")
    }
}

/// One writer handle backed by the shared test buffer.
pub struct CapturedLogWriter(Arc<Mutex<Vec<u8>>>);

impl Write for CapturedLogWriter {
    /// Appends one formatted tracing buffer to the shared capture.
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_poison_error| io::Error::other("log lock poisoned"))?
            .write(buffer)
    }

    /// The in-memory capture has no buffered state to flush.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CapturedLogs {
    type Writer = CapturedLogWriter;

    /// Creates a writer handle sharing the captured byte buffer.
    fn make_writer(&'writer self) -> Self::Writer {
        CapturedLogWriter(Arc::clone(&self.0))
    }
}
