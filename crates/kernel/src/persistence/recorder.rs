use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::record::{PersistedPayload, PersistedRecord, SCHEMA_VERSION, timestamp_now};

/// Append-only writer for one session JSONL file.
#[derive(Clone)]
pub(crate) struct SessionRecorder {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl SessionRecorder {
    /// Create a recorder for a session JSONL file.
    pub(crate) fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Arc::new(Mutex::new(())),
        }
    }

    /// Append typed payloads to the session JSONL file in order.
    pub(crate) async fn append(&self, payloads: &[PersistedPayload]) -> io::Result<()> {
        if payloads.is_empty() {
            return Ok(());
        }
        let _guard = self.lock.lock().await;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        for payload in payloads {
            let record = PersistedRecord::builder()
                .timestamp(timestamp_now())
                .schema_version(SCHEMA_VERSION)
                .payload(payload.clone())
                .build();
            let mut line = serde_json::to_string(&record).map_err(io::Error::other)?;
            line.push('\n');
            file.write_all(line.as_bytes())?;
        }
        file.flush()?;
        file.sync_all()?;
        Ok(())
    }

    /// Flush the recorder by syncing the current file if it has been materialized.
    pub(crate) async fn flush(&self) -> io::Result<()> {
        let _guard = self.lock.lock().await;
        let file = match std::fs::OpenOptions::new().read(true).open(&self.path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        };
        file.sync_all()
    }
}
