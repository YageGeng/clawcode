use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use protocol::{AgentPath, SessionId};
use serde::{Deserialize, Serialize};

use super::record::timestamp_now;

const MANIFEST_FILE: &str = "session_manifest.jsonl";

/// Append-only manifest entry for locating session rollout files.
#[derive(Clone, Debug, Serialize, Deserialize, typed_builder::TypedBuilder)]
pub(crate) struct SessionManifestRecord {
    /// Session id mapped by this entry.
    pub session_id: SessionId,
    /// Path to the session JSONL file, relative to data home when possible.
    pub path: PathBuf,
    /// Optional parent session id for subagent discovery.
    #[builder(default, setter(strip_option))]
    pub parent_session_id: Option<SessionId>,
    /// Agent path for display and routing after restore.
    pub agent_path: AgentPath,
    /// Working directory used for fast session listing.
    #[serde(default)]
    pub cwd: PathBuf,
    /// Current lifecycle status.
    pub status: SessionManifestStatus,
    /// Last update time in UTC-ish process timestamp format.
    pub updated_at: String,
}

/// Manifest lifecycle status.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SessionManifestStatus {
    /// Session is available for loading.
    Active,
    /// Session was closed by the user.
    Closed,
    /// Session was archived out of the active session tree.
    Archived,
}

/// Return the manifest path under the configured data home.
pub(crate) fn manifest_path(data_home: &Path) -> PathBuf {
    data_home.join(MANIFEST_FILE)
}

/// Append one manifest record to the append-only manifest file.
pub(crate) fn append_manifest_record(
    data_home: &Path,
    record: &SessionManifestRecord,
) -> io::Result<()> {
    std::fs::create_dir_all(data_home)?;
    let path = manifest_path(data_home);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    let mut line = serde_json::to_string(record).map_err(io::Error::other)?;
    line.push('\n');
    file.write_all(line.as_bytes())?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

/// Build an active manifest record for a session file.
pub(crate) fn active_manifest_record(
    data_home: &Path,
    session_id: SessionId,
    path: PathBuf,
    agent_path: AgentPath,
    cwd: PathBuf,
) -> SessionManifestRecord {
    SessionManifestRecord::builder()
        .session_id(session_id)
        .path(relative_to_data_home(data_home, path))
        .agent_path(agent_path)
        .cwd(cwd)
        .status(SessionManifestStatus::Active)
        .updated_at(timestamp_now())
        .build()
}

/// Build a closed manifest record for an existing manifest entry.
pub(crate) fn closed_manifest_record(record: &SessionManifestRecord) -> SessionManifestRecord {
    SessionManifestRecord::builder()
        .session_id(record.session_id.clone())
        .path(record.path.clone())
        .agent_path(record.agent_path.clone())
        .cwd(record.cwd.clone())
        .status(SessionManifestStatus::Closed)
        .updated_at(timestamp_now())
        .build()
}

/// Read the manifest and return the latest record per session id.
pub(crate) fn read_latest_manifest(
    data_home: &Path,
) -> io::Result<HashMap<SessionId, SessionManifestRecord>> {
    let path = manifest_path(data_home);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
        Err(err) => return Err(err),
    };
    let mut records = HashMap::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // Skip corrupt manifest lines so later valid entries can still recover sessions.
        let Ok(record) = serde_json::from_str::<SessionManifestRecord>(line) else {
            tracing::warn!(line, "skipping corrupt session manifest line");
            continue;
        };
        records.insert(record.session_id.clone(), record);
    }
    Ok(records)
}

/// Convert a manifest path into an absolute path under data home when needed.
pub(crate) fn resolve_manifest_path(data_home: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    data_home.join(path)
}

/// Store paths relative to data home to keep manifests portable across home moves.
fn relative_to_data_home(data_home: &Path, path: PathBuf) -> PathBuf {
    path.strip_prefix(data_home)
        .map(Path::to_path_buf)
        .unwrap_or(path)
}
