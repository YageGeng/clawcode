use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use protocol::{SessionId, TimestampMs};
use serde::{Deserialize, Serialize};

use crate::session::JsonlSessionStore;
use crate::state::SessionState;
use crate::{
    Clock, NewEntry, SessionCreateOptions, SessionForkOptions, SessionMetadata,
    SessionStore, StoreError, StoreFactory,
};

/// Filesystem-backed store factory using pi's cwd-encoded directory layout.
pub struct JsonlStoreFactory {
    sessions_root: PathBuf,
    clock: Arc<dyn Clock>,
}

impl JsonlStoreFactory {
    /// Creates a JSONL store factory rooted at the provided sessions directory.
    #[must_use]
    pub fn new(root: impl Into<PathBuf>, clock: Arc<dyn Clock>) -> Self {
        Self {
            sessions_root: root.into(),
            clock,
        }
    }
}

impl StoreFactory for JsonlStoreFactory {
    /// Creates one v4 session log without any global index file.
    fn create(
        &self,
        options: SessionCreateOptions,
    ) -> Result<Box<dyn SessionStore>, StoreError> {
        let created_at = self.clock.now();
        let directory = self
            .sessions_root
            .join(CwdSessionDirectory::from(options.cwd.as_path()).as_str());
        fs::create_dir_all(&directory)?;
        let path = directory
            .join(format!("{}_{}.jsonl", created_at, options.session_id));
        let header = V4Header::builder()
            .kind(HeaderKind::Header)
            .version(4)
            .id(options.session_id)
            .created_at(created_at)
            .cwd(options.cwd)
            .parent_session_id(options.parent_session_id)
            .build();
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        serde_json::to_writer(&mut file, &header)?;
        file.write_all(b"\n")?;
        file.sync_data()?;

        Ok(Box::new(
            JsonlSessionStore::builder()
                .session_id(header.id)
                .path(path)
                .clock(Arc::clone(&self.clock))
                .state(SessionState::new()?)
                .build(),
        ))
    }

    /// Opens a v4 session, applying valid mutations and repairing a torn final line.
    fn open(&self, path: &Path) -> Result<Box<dyn SessionStore>, StoreError> {
        let content = fs::read_to_string(path)?;
        let mut lines = content.split('\n').collect::<Vec<_>>();
        if lines.last().is_some_and(|line| line.is_empty()) {
            lines.pop();
        }
        let header_line = lines.first().ok_or_else(|| {
            StoreError::InvalidSession("missing header".to_string())
        })?;
        let header: V4Header = serde_json::from_str(header_line)
            .map_err(|error| StoreError::InvalidSession(error.to_string()))?;
        if header.kind != HeaderKind::Header || header.version != 4 {
            return Err(StoreError::InvalidSession(
                "unsupported or missing v4 header".to_string(),
            ));
        }

        let mut state = SessionState::new()?;
        for (index, line) in lines.iter().enumerate().skip(1) {
            let value = match serde_json::from_str::<serde_json::Value>(line) {
                Ok(value) => value,
                Err(_error) if index + 1 == lines.len() => {
                    // A checked prefix avoids turning tail recovery into a panic path.
                    let valid_lines = lines.get(..index).ok_or_else(|| {
                        StoreError::InvalidSession(
                            "invalid torn-tail boundary".to_string(),
                        )
                    })?;
                    Self::repair_torn_tail(path, valid_lines)?;
                    break;
                }
                Err(error) => {
                    return Err(StoreError::InvalidSession(format!(
                        "line {} is invalid JSON: {error}",
                        index + 1
                    )));
                }
            };
            JsonlSessionStore::apply_loaded_mutation(&mut state, value)?;
        }
        if !content.ends_with('\n') {
            let mut file = OpenOptions::new().append(true).open(path)?;
            file.write_all(b"\n")?;
        }

        Ok(Box::new(
            JsonlSessionStore::builder()
                .session_id(header.id)
                .path(path.to_path_buf())
                .clock(Arc::clone(&self.clock))
                .state(state)
                .build(),
        ))
    }

    /// Forks only the selected leaf's ancestor chain into a new session.
    fn fork(
        &self,
        source_path: &Path,
        mut options: SessionForkOptions,
    ) -> Result<Box<dyn SessionStore>, StoreError> {
        let source = self.open(source_path)?;
        let mut branch = Vec::new();
        let mut cursor = Some(options.source_leaf.clone());
        while let Some(entry_id) = cursor {
            let entry = source
                .get_entry(&entry_id)
                .cloned()
                .ok_or_else(|| StoreError::EntryNotFound(entry_id.clone()))?;
            cursor = entry.parent_id.clone();
            branch.push(entry);
        }
        branch.reverse();

        if options.create.parent_session_id.is_none() {
            options.create.parent_session_id =
                Some(source.session_id().clone());
        }
        let mut target = self.create(options.create)?;
        if options.lane.as_str() != "main" {
            target.create_lane(options.lane.clone(), None)?;
        }
        for entry in branch {
            target.append_entry(
                &options.lane,
                NewEntry {
                    id: entry.id,
                    kind: entry.kind,
                    payload: serde_json::Value::Object(entry.payload),
                },
            )?;
        }
        Ok(target)
    }

    /// Lists sessions from v4 logs and resolves their latest name facts without an index.
    fn list(
        &self,
        cwd: Option<&Path>,
    ) -> Result<Vec<SessionMetadata>, StoreError> {
        let mut directories = Vec::new();
        if let Some(cwd) = cwd {
            let directory = self
                .sessions_root
                .join(CwdSessionDirectory::from(cwd).as_str());
            if directory.exists() {
                directories.push(directory);
            }
        } else if self.sessions_root.exists() {
            for entry in fs::read_dir(&self.sessions_root)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    directories.push(entry.path());
                }
            }
        }

        let mut sessions = Vec::new();
        for directory in directories {
            for entry in fs::read_dir(directory)? {
                let entry = entry?;
                let path = entry.path();
                if !entry.file_type()?.is_file()
                    || path.extension().and_then(|value| value.to_str())
                        != Some("jsonl")
                {
                    continue;
                }
                let content = fs::read_to_string(&path)?;
                let Some(header_line) = content.lines().next() else {
                    continue;
                };
                let Ok(header) = serde_json::from_str::<V4Header>(header_line)
                else {
                    continue;
                };
                if header.kind != HeaderKind::Header || header.version != 4 {
                    continue;
                }
                // Name is a global fact, so the last valid mutation determines list display.
                let mut name = None;
                for line in content.lines().skip(1) {
                    let Ok(value) =
                        serde_json::from_str::<serde_json::Value>(line)
                    else {
                        continue;
                    };
                    if value.get("kind").and_then(serde_json::Value::as_str)
                        == Some("fact")
                        && value.get("fact").and_then(serde_json::Value::as_str)
                            == Some("name")
                    {
                        name = value
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .map(ToOwned::to_owned);
                    }
                }
                let modified = entry
                    .metadata()?
                    .modified()?
                    .duration_since(UNIX_EPOCH)
                    .map_or(0_u128, |duration| duration.as_millis());
                sessions.push(
                    SessionMetadata::builder()
                        .id(header.id)
                        .created_at_ms(header.created_at)
                        .cwd(header.cwd)
                        .parent_session_id(header.parent_session_id)
                        .path(path)
                        .modified_at_ms(TimestampMs::from(
                            u64::try_from(modified).unwrap_or(u64::MAX),
                        ))
                        .name(name)
                        .build(),
                );
            }
        }
        sessions.sort_by(|left, right| {
            right.modified_at_ms.cmp(&left.modified_at_ms)
        });
        Ok(sessions)
    }

    /// Deletes matching v4 logs without introducing a global session index.
    fn delete(&self, session_id: &SessionId) -> Result<(), StoreError> {
        for session in self
            .list(None)?
            .into_iter()
            .filter(|session| &session.id == session_id)
        {
            match fs::remove_file(session.path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(StoreError::Io(error)),
            }
        }
        Ok(())
    }
}

impl JsonlStoreFactory {
    /// Atomically replaces a session file with the acknowledged valid prefix.
    fn repair_torn_tail(
        path: &Path,
        valid_lines: &[&str],
    ) -> Result<(), StoreError> {
        let temporary = path.with_extension("jsonl.tmp");
        let mut content = valid_lines.join("\n");
        content.push('\n');
        fs::write(&temporary, content)?;
        fs::rename(temporary, path)?;
        Ok(())
    }
}

/// Encoded directory component derived from an absolute or relative cwd.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CwdSessionDirectory(String);

impl CwdSessionDirectory {
    /// Returns the encoded directory component.
    fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&Path> for CwdSessionDirectory {
    /// Encodes path separators and Windows drive separators using pi's layout.
    fn from(cwd: &Path) -> Self {
        let value = cwd.to_string_lossy();
        let without_root = value.trim_start_matches(['/', '\\']);
        let encoded = without_root
            .chars()
            .map(|character| match character {
                '/' | '\\' | ':' => '-',
                other => other,
            })
            .collect::<String>();
        Self(format!("--{encoded}--"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum HeaderKind {
    Header,
}

#[derive(Debug, Serialize, Deserialize, typed_builder::TypedBuilder)]
#[serde(rename_all = "camelCase")]
struct V4Header {
    kind: HeaderKind,
    version: u8,
    id: SessionId,
    created_at: TimestampMs,
    cwd: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[builder(default)]
    parent_session_id: Option<SessionId>,
}
