use std::io::{self, Write};
use std::sync::{Arc, Mutex, OnceLock};

/// Process-wide capture buffer written by the installed global tracing subscriber.
static BUFFER: OnceLock<Arc<Mutex<Vec<u8>>>> = OnceLock::new();

/// Returns the shared capture buffer, installing the global tracing subscriber once.
///
/// `tracing::subscriber::set_default` is thread-local and is lost when a `tokio`
/// task migrates between worker threads, which makes lifecycle log capture flaky.
/// A process-global subscriber written to one shared buffer is the only reliable
/// way to retain logs emitted on any thread or from a `tokio::spawn`ed task.
fn buffer() -> &'static Arc<Mutex<Vec<u8>>> {
    BUFFER.get_or_init(|| {
        let buf = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(SharedLogWriter(Arc::clone(&buf)))
            .finish();
        tracing::subscriber::set_global_default(subscriber)
            .expect("global tracing subscriber already installed");
        buf
    })
}

/// Shared in-memory writer used to inspect formatted tracing output.
///
/// The capture buffer is process-wide, so every log emitted anywhere in the test
/// binary — including `tokio::spawn`ed tasks and other worker threads — is
/// retained regardless of which thread ran the emitting code.
#[derive(Clone)]
pub struct CapturedLogs {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl CapturedLogs {
    /// Returns all UTF-8 tracing output written by the subscriber.
    pub fn content(&self) -> String {
        String::from_utf8(self.buf.lock().expect("log lock").clone())
            .expect("UTF-8 logs")
    }
}

impl Default for CapturedLogs {
    /// Creates a handle over the shared capture buffer.
    fn default() -> Self {
        CapturedLogs {
            buf: Arc::clone(buffer()),
        }
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CapturedLogs {
    type Writer = SharedLogWriter;

    /// Creates a writer handle sharing the captured byte buffer.
    fn make_writer(&'writer self) -> Self::Writer {
        SharedLogWriter(Arc::clone(&self.buf))
    }
}

/// One writer handle backed by the shared test buffer.
pub struct SharedLogWriter(Arc<Mutex<Vec<u8>>>);

impl Write for SharedLogWriter {
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

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for SharedLogWriter {
    type Writer = SharedLogWriter;

    /// Creates a writer handle sharing the captured byte buffer.
    fn make_writer(&'writer self) -> Self::Writer {
        SharedLogWriter(Arc::clone(&self.0))
    }
}
