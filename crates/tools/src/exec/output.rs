//! Bounded process output that keeps both its beginning and its end.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Write as _};
use std::path::PathBuf;

use protocol::ProductIdentity;

/// A spilled transcript keeps at most this many times what memory keeps, so a
/// chatty background process cannot fill the disk.
const SPILL_MAX_BUFFERS: usize = 64;

/// Capped bytes that keep a stable head and the newest tail, counting the
/// dropped middle; each end gets half of the capacity, as in codex.
#[derive(Debug, Default, Clone)]
pub struct HeadTailBuffer {
    max_bytes: usize,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    omitted_bytes: usize,
}

impl HeadTailBuffer {
    /// Creates an empty buffer retaining at most `max_bytes`.
    #[must_use]
    pub fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            ..Self::default()
        }
    }

    /// Appends one chunk: the head fills first, then the tail keeps the newest bytes.
    pub fn push(&mut self, chunk: &[u8]) {
        let head_budget = self.max_bytes / 2;
        let tail_budget = self.max_bytes - head_budget;
        let room = head_budget.saturating_sub(self.head.len());
        let (into_head, rest) = chunk.split_at(room.min(chunk.len()));
        self.head.extend_from_slice(into_head);
        // Drop old tail bytes first, then any excess of the incoming chunk.
        let excess = (self.tail.len() + rest.len()).saturating_sub(tail_budget);
        self.omitted_bytes = self.omitted_bytes.saturating_add(excess);
        let from_tail = excess.min(self.tail.len());
        self.tail.drain(..from_tail);
        self.tail
            .extend(rest.get(excess - from_tail..).unwrap_or_default());
    }

    /// Most bytes this buffer retains.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.max_bytes
    }

    /// Bytes seen so far, including the omitted middle.
    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.head.len() + self.tail.len() + self.omitted_bytes
    }

    /// Copies the retained bytes without clearing the buffer.
    #[must_use]
    pub fn snapshot(&self) -> Captured {
        Captured {
            head_len: self.head.len(),
            bytes: self.head.iter().chain(self.tail.iter()).copied().collect(),
            omitted_bytes: self.omitted_bytes,
        }
    }

    /// Returns the retained bytes and starts over empty.
    pub fn take(&mut self) -> Captured {
        let captured = self.snapshot();
        *self = Self::new(self.max_bytes);
        captured
    }

    /// Decodes the retained bytes with an omission marker, without clearing.
    #[must_use]
    pub fn text(&self) -> String {
        self.snapshot().text()
    }

    /// Decodes at most the newest `max_bytes`, copying only those, so a
    /// display tail costs its own size instead of the whole buffer's.
    #[must_use]
    pub fn last_text(&self, max_bytes: usize) -> String {
        // Past a dropped middle only the tail is contiguous with the newest byte.
        let head: &[u8] = if self.omitted_bytes == 0 {
            &self.head
        } else {
            &[]
        };
        let available = head.len() + self.tail.len();
        let bytes: Vec<u8> = head
            .iter()
            .chain(self.tail.iter())
            .skip(available.saturating_sub(max_bytes))
            .copied()
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

/// Bytes collected from a buffer, split where the dropped middle was.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Captured {
    /// Retained head followed by retained tail.
    pub bytes: Vec<u8>,
    /// Bytes dropped between the head and the tail.
    pub omitted_bytes: usize,
    /// Length of the head inside `bytes`.
    head_len: usize,
}

impl Captured {
    /// Decodes lossily, so invalid UTF-8 never fails, and marks the dropped middle.
    #[must_use]
    pub fn text(&self) -> String {
        if self.omitted_bytes == 0 {
            return String::from_utf8_lossy(&self.bytes).into_owned();
        }
        let (head, tail) = self.bytes.split_at(self.head_len);
        // Each side decodes alone so a character cut by the gap cannot join.
        format!(
            "{}\n... {} bytes omitted ...\n{}",
            String::from_utf8_lossy(head),
            self.omitted_bytes,
            String::from_utf8_lossy(tail)
        )
    }

    /// Bytes the process produced, including the omitted middle.
    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.bytes.len() + self.omitted_bytes
    }
}

/// The two views of one process's output: what the model has not read yet,
/// and the whole transcript clients display.
#[derive(Debug)]
pub struct OutputBuffers {
    /// Output since the model last collected; drained only through
    /// `take_pending`, which also hands out the spill file it may need.
    pending: HeadTailBuffer,
    /// Everything the process printed; never drained.
    pub transcript: HeadTailBuffer,
    /// Every transcript byte on disk, once memory alone would lose some.
    spill: TranscriptSpill,
}

impl OutputBuffers {
    /// Creates both buffers with the same capacity.
    #[must_use]
    pub fn new(max_bytes: usize) -> Self {
        Self {
            pending: HeadTailBuffer::new(max_bytes),
            transcript: HeadTailBuffer::new(max_bytes),
            spill: TranscriptSpill::default(),
        }
    }

    /// Appends one chunk to both views under the caller's single lock; the
    /// spill sees it first, before the transcript can drop anything.
    pub fn push_chunk(&mut self, chunk: &[u8]) {
        self.pending.push(chunk);
        self.spill.record(&self.transcript, chunk);
        self.transcript.push(chunk);
    }

    /// Drains the model's view; when that view lost bytes, also names the
    /// transcript file that still holds them.
    pub fn take_pending(&mut self) -> (Captured, Option<PathBuf>) {
        let output = self.pending.take();
        let spilled = if output.omitted_bytes > 0 {
            self.spill.path()
        } else {
            None
        };
        (output, spilled)
    }

    /// The flushed transcript file, once memory alone would have lost bytes.
    pub fn spilled_transcript(&mut self) -> Option<PathBuf> {
        self.spill.path()
    }
}

/// The transcript on disk, started just before memory drops its first byte,
/// so the file misses nothing up to its cap.
#[derive(Debug, Default)]
enum TranscriptSpill {
    /// Everything still fits in memory.
    #[default]
    Unneeded,
    /// Every byte so far is in this file.
    Writing {
        file: BufWriter<File>,
        path: PathBuf,
        written: usize,
    },
    /// The cap was reached; the file ends with a marker.
    Capped(PathBuf),
    /// The file could not be written; bytes memory drops are lost.
    Failed,
}

impl TranscriptSpill {
    /// Writes `chunk`; the first chunk `transcript` would drop bytes for
    /// starts the file with everything it holds, which is all of it so far.
    fn record(&mut self, transcript: &HeadTailBuffer, chunk: &[u8]) {
        let cap = transcript.capacity().saturating_mul(SPILL_MAX_BUFFERS);
        if matches!(self, Self::Unneeded) {
            if transcript.total_bytes() + chunk.len() <= transcript.capacity() {
                return;
            }
            *self = Self::start();
            self.append(&transcript.snapshot().bytes, cap);
        }
        self.append(chunk, cap);
    }

    /// Creates the retained file.
    fn start() -> Self {
        match retained_log() {
            Ok((file, path)) => {
                tracing::info!(
                    "spilling exec transcript to {}",
                    path.display()
                );
                Self::Writing {
                    file: BufWriter::new(file),
                    path,
                    written: 0,
                }
            }
            Err(error) => {
                tracing::warn!(
                    "failed to create exec transcript file: {}",
                    error
                );
                Self::Failed
            }
        }
    }

    /// Appends up to the cap; reaching it writes a marker and stops.
    fn append(&mut self, bytes: &[u8], cap: usize) {
        let Self::Writing {
            file,
            path,
            written,
        } = self
        else {
            return;
        };
        let kept = bytes.len().min(cap.saturating_sub(*written));
        *written += kept;
        let capped = kept < bytes.len();
        let mut result = file.write_all(bytes.get(..kept).unwrap_or_default());
        if capped {
            result = result
                .and_then(|()| {
                    write!(
                        file,
                        "\n... output past {cap} bytes was not kept ...\n"
                    )
                })
                .and_then(|()| file.flush());
        }
        match result {
            Err(error) => {
                tracing::warn!(
                    "failed to write exec transcript {}: {}",
                    path.display(),
                    error
                );
                *self = Self::Failed;
            }
            Ok(()) if capped => {
                tracing::info!(
                    "exec transcript {} reached {} bytes",
                    path.display(),
                    cap
                );
                *self = Self::Capped(std::mem::take(path));
            }
            Ok(()) => {}
        }
    }

    /// Flushes and names the file, unless writing it failed.
    fn path(&mut self) -> Option<PathBuf> {
        match self {
            Self::Writing { file, path, .. } => match file.flush() {
                Ok(()) => Some(path.clone()),
                Err(error) => {
                    tracing::warn!(
                        "failed to flush exec transcript {}: {}",
                        path.display(),
                        error
                    );
                    *self = Self::Failed;
                    None
                }
            },
            Self::Capped(path) => Some(path.clone()),
            Self::Unneeded | Self::Failed => None,
        }
    }
}

/// Creates a temporary log file that is kept after it closes.
fn retained_log() -> std::io::Result<(File, PathBuf)> {
    tempfile::Builder::new()
        .prefix(ProductIdentity::BASH_OUTPUT_TEMP_PREFIX)
        .suffix(".log")
        .tempfile()?
        .keep()
        .map_err(|error| error.error)
}

/// Writes complete output to a retained temporary file for later reading.
pub(super) fn persist_full_output(text: &str) -> Option<PathBuf> {
    let written = retained_log().and_then(|(mut file, path)| {
        file.write_all(text.as_bytes())?;
        file.flush()?;
        Ok(path)
    });
    match written {
        Ok(path) => Some(path),
        Err(error) => {
            tracing::warn!("failed to keep full exec output: {}", error);
            None
        }
    }
}
