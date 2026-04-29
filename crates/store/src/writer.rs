use std::{
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use crate::event::TurnEvent;

/// Thread-safe append-only JSONL writer.
///
/// Serialization runs on the current task; file I/O is dispatched to
/// `spawn_blocking` so the async runtime is never blocked by disk writes.
pub struct JsonlWriter {
    file: Arc<Mutex<BufWriter<fs::File>>>,
    path: PathBuf,
}

impl JsonlWriter {
    /// Opens or creates a JSONL file at `path` for append-only writing.
    pub fn open(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        Ok(Self {
            file: Arc::new(Mutex::new(BufWriter::new(file))),
            path,
        })
    }

    /// Serializes the event and appends it as a JSON line in the blocking
    /// thread pool so the executor is never stalled on disk I/O.
    pub async fn write_event(&self, event: &TurnEvent) -> std::io::Result<()> {
        // Serialize on the current task (CPU-bound, not blocking).
        let mut line = serde_json::to_vec(event).map_err(std::io::Error::other)?;
        line.push(b'\n');

        let file = Arc::clone(&self.file);
        tokio::task::spawn_blocking(move || {
            let mut f = file.lock().unwrap_or_else(|e| e.into_inner());
            f.write_all(&line)?;
            f.flush()
        })
        .await
        .map_err(std::io::Error::other)?
    }

    /// Returns the file path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}
