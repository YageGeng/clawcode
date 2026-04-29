use std::{
    fs,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use crate::event::TurnEvent;

/// Metadata about one persisted session discovered on disk.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// UUID extracted from the session filename.
    pub id: String,
    /// Full path to the JSONL file.
    pub path: PathBuf,
    /// Approximate creation time derived from filesystem metadata.
    pub created_at: chrono::NaiveDateTime,
    /// Approximate turn count (line count of the JSONL file).
    pub turn_count: usize,
}

/// Lists all persisted sessions found under the standard data directory.
///
/// Recursively scans `~/.local/share/clawcode/sessions/` for `*.jsonl` files
/// and returns them sorted newest-first.
pub fn list_sessions() -> std::io::Result<Vec<SessionInfo>> {
    let data_dir = sessions_root();
    if !data_dir.exists() {
        return Ok(Vec::new());
    }

    let mut sessions = Vec::new();
    collect_jsonl_files(&data_dir, &mut sessions)?;
    sessions.sort_by_key(|s| std::cmp::Reverse(s.created_at));
    Ok(sessions)
}

/// Loads a session JSONL file and replays all events, returning the raw `TurnEvent` list.
///
/// Callers can reconstruct in-memory state from the returned events.
pub fn load_session_events(path: &Path) -> std::io::Result<Vec<TurnEvent>> {
    let file = fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let event: TurnEvent = serde_json::from_str(&line)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        events.push(event);
    }

    Ok(events)
}

/// Finds a session file whose filename contains the given UUID.
///
/// Matches the last `-`-delimited segment of the file stem (before `.jsonl`)
/// against `id` for an exact UUID comparison.
pub fn find_session_by_id(id: &str) -> Option<PathBuf> {
    let root = sessions_root();
    if !root.is_dir() {
        return None;
    }
    find_session_in_dir(&root, id)
}

fn find_session_in_dir(dir: &Path, id: &str) -> Option<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!("failed to read sessions directory {}: {e}", dir.display());
            return None;
        }
    };

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!("failed to read directory entry in {}: {e}", dir.display());
                continue;
            }
        };
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_session_in_dir(&path, id) {
                return Some(found);
            }
        } else if let Some(ext) = path.extension()
            && ext == "jsonl"
        {
            let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            // Extract the UUID segment (last `-`-delimited part) from filenames
            // like "session-20260429T164942-uuid.jsonl".
            let file_uuid = stem.rsplit('-').next().unwrap_or("");
            if file_uuid == id {
                return Some(path);
            }
        }
    }
    None
}

/// Returns the standard sessions root directory.
pub fn sessions_root() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("clawcode")
        .join("sessions")
}

fn collect_jsonl_files(dir: &Path, results: &mut Vec<SessionInfo>) -> std::io::Result<()> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl_files(&path, results)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl")
            && let Ok(info) = parse_session_info(&path)
        {
            results.push(info);
        }
    }
    Ok(())
}

fn parse_session_info(path: &Path) -> std::io::Result<SessionInfo> {
    let filename = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");

    // Extract UUID from filename like "session-20260429T120000-uuid.jsonl"
    let id = filename.rsplit('-').next().unwrap_or(filename).to_string();

    let turn_count = BufReader::new(fs::File::open(path)?).lines().count();

    let metadata = path.metadata()?;
    let system_time = metadata
        .created()
        .or_else(|_| metadata.modified())
        .unwrap_or(std::time::SystemTime::now());
    let created_at = chrono::DateTime::<chrono::Utc>::from(system_time).naive_utc();

    Ok(SessionInfo {
        id,
        path: path.to_path_buf(),
        created_at,
        turn_count,
    })
}
