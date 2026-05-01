use std::{
    fs,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use snafu::ResultExt;

use crate::event::SessionEvent;
use crate::{
    Result,
    error::{IoSnafu, JoinSnafu, JsonSnafu},
};

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
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context(IoSnafu {
                stage: "store-open-create-dir".to_string(),
                path: parent.to_path_buf(),
            })?;
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .context(IoSnafu {
                stage: "store-open-file".to_string(),
                path: path.clone(),
            })?;
        Ok(Self {
            file: Arc::new(Mutex::new(BufWriter::new(file))),
            path,
        })
    }

    /// Serializes the event and appends it as a JSON line in the blocking
    /// thread pool so the executor is never stalled on disk I/O.
    pub async fn write_event(&self, event: &SessionEvent) -> Result<()> {
        // Serialize on the current task (CPU-bound, not blocking).
        let mut line = serde_json::to_vec(event).context(JsonSnafu {
            stage: "store-write-event-serialize".to_string(),
            path: self.path.clone(),
        })?;
        line.push(b'\n');

        let file = Arc::clone(&self.file);
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let mut f = file.lock().unwrap_or_else(|e| e.into_inner());
            f.write_all(&line).context(IoSnafu {
                stage: "store-write-event-write".to_string(),
                path: path.clone(),
            })?;
            f.flush().context(IoSnafu {
                stage: "store-write-event-flush".to_string(),
                path,
            })?;
            Ok(())
        })
        .await
        .context(JoinSnafu {
            stage: "store-write-event-blocking".to_string(),
            path: self.path.clone(),
        })?
    }

    /// Returns the file path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}
