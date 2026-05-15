use std::io;
use std::path::{Path, PathBuf};

use protocol::{SessionId, SessionInfo};

use super::manifest::{
    SessionManifestStatus, active_manifest_record, append_manifest_record, closed_manifest_record,
    read_latest_manifest, resolve_manifest_path,
};
use super::record::{CreateSessionParams, PersistedPayload, SessionMetaRecord, timestamp_now};
use super::recorder::{FileSessionRecorder, SessionRecorder};
use super::replay::{ReplayedSession, replay_session_file};

/// File-backed session store rooted at a data-home directory.
pub struct FileSessionStore {
    data_home: PathBuf,
    enabled: bool,
}

impl FileSessionStore {
    /// Create a store from the enabled flag and optional data home path.
    pub fn new(enabled: bool, data_home: Option<&str>) -> Self {
        let data_home = data_home
            .map(expand_tilde)
            .unwrap_or_else(default_data_home);
        Self { data_home, enabled }
    }

    /// Create a store using the default data-home directory.
    pub fn new_default() -> Self {
        Self {
            data_home: default_data_home(),
            enabled: true,
        }
    }

    /// Build the date-partitioned session JSONL path for a new session.
    fn session_file_path(&self, session_id: &SessionId) -> PathBuf {
        let (year, month, day) = current_date_parts();
        let filename = format!("session-{}-{session_id}.jsonl", timestamp_now());
        self.data_home
            .join("sessions")
            .join(year)
            .join(month)
            .join(day)
            .join(filename)
    }
}

#[async_trait::async_trait]
impl super::traits::SessionStore for FileSessionStore {
    async fn create_session(
        &self,
        params: CreateSessionParams,
    ) -> io::Result<Option<Box<dyn SessionRecorder>>> {
        if !self.enabled {
            return Ok(None);
        }
        let path = self.session_file_path(&params.session_id);
        let recorder = FileSessionRecorder::new(path.clone());
        let cwd = params.cwd;
        let parent_session_id = params.parent_session_id;
        let agent_role = params.agent_role;
        let agent_nickname = params.agent_nickname;
        let meta = SessionMetaRecord::builder()
            .session_id(params.session_id.clone())
            .agent_path(params.agent_path.clone())
            .cwd(cwd.clone())
            .provider_id(params.provider_id)
            .model_id(params.model_id)
            .base_system_prompt(params.base_system_prompt)
            .created_at(timestamp_now())
            .parent_session_id(parent_session_id.clone())
            .agent_role(agent_role.clone())
            .agent_nickname(agent_nickname.clone())
            .build();
        recorder
            .append(&[PersistedPayload::SessionMeta(meta)])
            .await?;
        let manifest = active_manifest_record(
            &self.data_home,
            params.session_id,
            path,
            params.agent_path,
            cwd,
            parent_session_id,
        );
        append_manifest_record(&self.data_home, &manifest)?;
        Ok(Some(Box::new(recorder)))
    }

    fn load_session(
        &self,
        session_id: &SessionId,
    ) -> io::Result<Option<(ReplayedSession, Box<dyn SessionRecorder>)>> {
        if !self.enabled {
            return Ok(None);
        }
        let manifest = read_latest_manifest(&self.data_home)?;
        let Some(record) = manifest.get(session_id) else {
            return Ok(None);
        };
        if record.status == SessionManifestStatus::Archived {
            return Ok(None);
        }
        let path = resolve_manifest_path(&self.data_home, &record.path);
        let replayed = replay_session_file(&path)?;
        let recorder = FileSessionRecorder::new(path);
        Ok(Some((replayed, Box::new(recorder))))
    }

    async fn close_session(
        &self,
        session_id: &SessionId,
        recorder: Option<&dyn SessionRecorder>,
    ) -> io::Result<()> {
        if !self.enabled {
            return Ok(());
        }
        if let Some(recorder) = recorder {
            recorder.flush().await?;
        }
        let manifest = read_latest_manifest(&self.data_home)?;
        if let Some(record) = manifest.get(session_id) {
            append_manifest_record(&self.data_home, &closed_manifest_record(record))?;
        }
        Ok(())
    }

    fn list_sessions(&self, cwd: Option<&Path>) -> io::Result<Vec<SessionInfo>> {
        if !self.enabled {
            return Ok(Vec::new());
        }
        let manifest = read_latest_manifest(&self.data_home)?;
        let mut sessions = Vec::new();
        for record in manifest.values() {
            if record.status == SessionManifestStatus::Archived {
                continue;
            }
            let session_cwd = if record.cwd.as_os_str().is_empty() {
                let path = resolve_manifest_path(&self.data_home, &record.path);
                let Ok(replayed) = replay_session_file(&path) else {
                    continue;
                };
                replayed.meta.cwd
            } else {
                record.cwd.clone()
            };
            if let Some(cwd) = cwd
                && session_cwd != cwd
            {
                continue;
            }
            sessions.push(
                SessionInfo::builder()
                    .session_id(record.session_id.clone())
                    .cwd(session_cwd)
                    .updated_at(Some(record.updated_at.clone()))
                    .build(),
            );
        }
        sessions.sort_by(|left, right| left.session_id.0.cmp(&right.session_id.0));
        Ok(sessions)
    }
}

// ── helpers ──

fn default_data_home() -> PathBuf {
    if let Some(value) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(value).join("clawcode");
    }

    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local")
        .join("share")
        .join("clawcode")
}

fn expand_tilde(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(value)
}

fn current_date_parts() -> (String, String, String) {
    let now = chrono::Local::now();
    (
        now.format("%Y").to_string(),
        now.format("%m").to_string(),
        now.format("%d").to_string(),
    )
}

#[cfg(test)]
mod store_tests {
    use super::*;
    use crate::record::{CreateSessionParams, MessageRecord, PersistedPayload};
    use crate::traits::SessionStore;
    use protocol::AgentPath;
    use protocol::message::Message;

    #[tokio::test]
    async fn create_load_and_list_session_roundtrips_messages() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore {
            data_home: temp.path().to_path_buf(),
            enabled: true,
        };
        let session_id = SessionId("session-1".to_string());
        let recorder = store
            .create_session(
                CreateSessionParams::builder()
                    .session_id(session_id.clone())
                    .agent_path(AgentPath::root())
                    .cwd(PathBuf::from("/tmp/project"))
                    .provider_id("provider".to_string())
                    .model_id("model".to_string())
                    .base_system_prompt("base".to_string())
                    .build(),
            )
            .await
            .expect("create session")
            .expect("recorder");
        recorder
            .append(&[PersistedPayload::Message(
                MessageRecord::builder()
                    .turn_id("turn-1".to_string())
                    .message(Message::user("hello"))
                    .build(),
            )])
            .await
            .expect("append message");

        let (replayed, _) = store
            .load_session(&session_id)
            .expect("load result")
            .expect("loaded session");
        assert_eq!(replayed.messages, vec![Message::user("hello")]);

        let listed = store.list_sessions(None).expect("list sessions");
        assert_eq!(listed.len(), 1);
    }

    #[tokio::test]
    async fn close_session_keeps_session_available_for_resume() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore {
            data_home: temp.path().to_path_buf(),
            enabled: true,
        };
        let session_id = SessionId("session-closed".to_string());
        let recorder = store
            .create_session(
                CreateSessionParams::builder()
                    .session_id(session_id.clone())
                    .agent_path(AgentPath::root())
                    .cwd(PathBuf::from("/tmp/project"))
                    .provider_id("provider".to_string())
                    .model_id("model".to_string())
                    .base_system_prompt(String::new())
                    .build(),
            )
            .await
            .expect("create session")
            .expect("recorder");

        store
            .close_session(&session_id, Some(recorder.as_ref()))
            .await
            .expect("close session");

        let listed = store.list_sessions(None).expect("list sessions");
        assert_eq!(listed.len(), 1);

        let loaded = store.load_session(&session_id).expect("load result");
        assert!(loaded.is_some());
    }
}
