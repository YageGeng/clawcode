use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use super::record::{PersistedPayload, PersistedRecord, SCHEMA_VERSION, timestamp_now};

/// Trait for appending typed payloads to a session's persistent storage.
#[async_trait]
pub trait SessionRecorder: Send + Sync {
    /// Append typed payloads to the session storage in order.
    async fn append(&self, payloads: &[PersistedPayload]) -> io::Result<()>;
    /// Flush buffered writes to durable storage.
    async fn flush(&self) -> io::Result<()>;
}

/// Append-only JSONL writer for one session file.
#[derive(Clone)]
pub struct FileSessionRecorder {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl FileSessionRecorder {
    /// Create a recorder for a session JSONL file.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Arc::new(Mutex::new(())),
        }
    }
}

#[async_trait]
impl SessionRecorder for FileSessionRecorder {
    async fn append(&self, payloads: &[PersistedPayload]) -> io::Result<()> {
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

    async fn flush(&self) -> io::Result<()> {
        let _guard = self.lock.lock().await;
        let file = match std::fs::OpenOptions::new().read(true).open(&self.path) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        };
        file.sync_all()
    }
}
