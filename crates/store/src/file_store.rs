use std::io;
use std::path::{Path, PathBuf};

use protocol::{SessionId, SessionInfo};

use super::agent_graph::{AgentEdgeStatus, AgentGraphStore, fold_agent_edges};
use super::manifest::{
    SessionManifestStatus, active_manifest_record, append_manifest_record,
    archived_manifest_record, closed_manifest_record, read_latest_manifest,
    resolve_manifest_path, titled_manifest_record,
};
use super::record::{
    AgentEdgeRecord, CreateSessionParams, PersistedPayload, SessionMetaRecord,
    timestamp_now,
};
use super::recorder::{FileSessionRecorder, SessionRecorder};
use super::replay::{ReplayedSession, replay_session_file};

/// File-backed session store rooted at a data-home directory.
pub struct FileSessionStore {
    data_home: PathBuf,
}

impl FileSessionStore {
    /// Create a store from an optional data home path.
    pub fn new(data_home: Option<&str>) -> Self {
        let data_home = data_home
            .map(expand_tilde)
            .unwrap_or_else(default_data_home);
        Self { data_home }
    }

    /// Create a store using the default data-home directory.
    pub fn new_default() -> Self {
        Self {
            data_home: default_data_home(),
        }
    }

    /// Build the date-partitioned session JSONL path for a new session.
    fn session_file_path(&self, session_id: &SessionId) -> PathBuf {
        let (year, month, day) = current_date_parts();
        let filename =
            format!("session-{}-{session_id}.jsonl", timestamp_now());
        self.data_home
            .join("sessions")
            .join(year)
            .join(month)
            .join(day)
            .join(filename)
    }

    /// Resolve the persisted JSONL path for a live or closed session id.
    fn session_path_for_id(
        &self,
        session_id: &SessionId,
    ) -> io::Result<Option<PathBuf>> {
        let manifest = read_latest_manifest(&self.data_home)?;
        Ok(manifest.get(session_id).and_then(|record| {
            if record.status == SessionManifestStatus::Archived {
                None
            } else {
                Some(resolve_manifest_path(&self.data_home, &record.path))
            }
        }))
    }

    /// Return the JSONL path for a session or a not-found IO error.
    fn require_session_path(
        &self,
        session_id: &SessionId,
    ) -> io::Result<PathBuf> {
        self.session_path_for_id(session_id)?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("session not found in manifest: {session_id}"),
            )
        })
    }

    /// Append one agent edge record to the parent session JSONL file.
    async fn append_agent_edge(
        &self,
        parent_session_id: &SessionId,
        edge: AgentEdgeRecord,
    ) -> io::Result<()> {
        let path = self.require_session_path(parent_session_id)?;
        let recorder = FileSessionRecorder::new(path);
        recorder.append(&[PersistedPayload::AgentEdge(edge)]).await
    }
}

#[async_trait::async_trait]
impl super::traits::SessionStore for FileSessionStore {
    async fn create_session(
        &self,
        params: CreateSessionParams,
    ) -> io::Result<Box<dyn SessionRecorder>> {
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
        Ok(Box::new(recorder))
    }

    fn load_session(
        &self,
        session_id: &SessionId,
    ) -> io::Result<Option<(ReplayedSession, Box<dyn SessionRecorder>)>> {
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
        recorder: &dyn SessionRecorder,
    ) -> io::Result<()> {
        recorder.flush().await?;
        let manifest = read_latest_manifest(&self.data_home)?;
        if let Some(record) = manifest.get(session_id) {
            append_manifest_record(
                &self.data_home,
                &closed_manifest_record(record),
            )?;
        }
        Ok(())
    }

    async fn archive_session(
        &self,
        session_id: &SessionId,
        recorder: &dyn SessionRecorder,
    ) -> io::Result<()> {
        recorder.flush().await?;
        let manifest = read_latest_manifest(&self.data_home)?;
        if let Some(record) = manifest.get(session_id) {
            append_manifest_record(
                &self.data_home,
                &archived_manifest_record(record),
            )?;
        }
        Ok(())
    }

    fn record_session_title(
        &self,
        session_id: &SessionId,
        title: &str,
    ) -> io::Result<()> {
        let title = title.trim();
        if title.is_empty() {
            return Ok(());
        }
        let manifest = read_latest_manifest(&self.data_home)?;
        let Some(record) = manifest.get(session_id) else {
            return Ok(());
        };
        if record.title.is_some() {
            return Ok(());
        }
        append_manifest_record(
            &self.data_home,
            &titled_manifest_record(record, title),
        )
    }

    fn list_sessions(
        &self,
        cwd: Option<&Path>,
    ) -> io::Result<Vec<SessionInfo>> {
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
                    .title(record.title.clone())
                    .updated_at(Some(record.updated_at.clone()))
                    .build(),
            );
        }
        sessions
            .sort_by(|left, right| left.session_id.0.cmp(&right.session_id.0));
        Ok(sessions)
    }
}

#[async_trait::async_trait]
impl AgentGraphStore for FileSessionStore {
    async fn upsert_agent_edge(
        &self,
        parent_session_id: SessionId,
        child_session_id: SessionId,
        child_agent_path: protocol::AgentPath,
        child_role: Option<String>,
        status: AgentEdgeStatus,
    ) -> io::Result<()> {
        let edge = AgentEdgeRecord::builder()
            .parent_session_id(parent_session_id.clone())
            .child_session_id(child_session_id)
            .child_agent_path(child_agent_path)
            .child_role(child_role.unwrap_or_default())
            .status(status.into())
            .build();
        self.append_agent_edge(&parent_session_id, edge).await
    }

    async fn set_agent_edge_status(
        &self,
        parent_session_id: &SessionId,
        child_session_id: &SessionId,
        status: AgentEdgeStatus,
    ) -> io::Result<()> {
        let Some(current) = self
            .list_agent_children(parent_session_id, None)?
            .into_iter()
            .find(|edge| &edge.child_session_id == child_session_id)
        else {
            return Ok(());
        };

        let edge = AgentEdgeRecord::builder()
            .parent_session_id(parent_session_id.clone())
            .child_session_id(child_session_id.clone())
            .child_agent_path(current.child_agent_path)
            .child_role(current.child_role.unwrap_or_default())
            .status(status.into())
            .build();
        self.append_agent_edge(parent_session_id, edge).await
    }

    fn list_agent_children(
        &self,
        parent_session_id: &SessionId,
        status: Option<AgentEdgeStatus>,
    ) -> io::Result<Vec<super::agent_graph::AgentEdge>> {
        let Some(path) = self.session_path_for_id(parent_session_id)? else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("session not found in manifest: {parent_session_id}"),
            ));
        };
        let replayed = replay_session_file(&path)?;
        Ok(fold_agent_edges(
            parent_session_id.clone(),
            replayed.agent_edges,
            status,
        ))
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
    use crate::agent_graph::{AgentEdgeStatus, AgentGraphStore};
    use crate::record::{CreateSessionParams, MessageRecord, PersistedPayload};
    use crate::traits::SessionStore;
    use protocol::AgentPath;
    use protocol::Usage;
    use protocol::message::Message;

    /// Build minimal root session creation parameters for store tests.
    fn root_params(session_id: SessionId) -> CreateSessionParams {
        CreateSessionParams::builder()
            .session_id(session_id)
            .agent_path(AgentPath::root())
            .cwd(PathBuf::from("/tmp/project"))
            .provider_id("provider".to_string())
            .model_id("model".to_string())
            .base_system_prompt(String::new())
            .build()
    }

    #[tokio::test]
    async fn create_load_and_list_session_roundtrips_messages() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(Some(
            temp.path().to_str().expect("temp path"),
        ));
        let session_id = SessionId::from("session-1");
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
            .expect("create session");
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
    async fn message_usage_is_written_top_level_without_replay_total() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(Some(
            temp.path().to_str().expect("temp path"),
        ));
        let session_id = SessionId::from("session-usage");
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
            .expect("create session");

        recorder
            .append(&[
                PersistedPayload::Message(
                    MessageRecord::builder()
                        .turn_id("turn-1".to_string())
                        .message(Message::assistant("one"))
                        .usage(Usage {
                            input_tokens: 10,
                            output_tokens: 2,
                            total_tokens: 12,
                            cached_input_tokens: 0,
                            cache_creation_input_tokens: 0,
                        })
                        .build(),
                ),
                PersistedPayload::Message(
                    MessageRecord::builder()
                        .turn_id("turn-2".to_string())
                        .message(Message::assistant("two"))
                        .usage(Usage {
                            input_tokens: 7,
                            output_tokens: 3,
                            total_tokens: 10,
                            cached_input_tokens: 0,
                            cache_creation_input_tokens: 0,
                        })
                        .build(),
                ),
            ])
            .await
            .expect("append messages");

        let session_path = store
            .session_path_for_id(&session_id)
            .expect("lookup path")
            .expect("session path");
        let text =
            std::fs::read_to_string(session_path).expect("read session jsonl");
        let message_records: Vec<serde_json::Value> = text
            .lines()
            .filter_map(|line| {
                let value: serde_json::Value =
                    serde_json::from_str(line).expect("json record");
                (value["payload"]["type"] == "message").then_some(value)
            })
            .collect();

        assert_eq!(message_records[0]["usage"]["input_tokens"], 10);
        assert!(
            message_records[0]["payload"]["payload"]
                .get("usage")
                .is_none()
        );

        let (replayed, _) = store
            .load_session(&session_id)
            .expect("load result")
            .expect("loaded session");
        assert_eq!(
            replayed.messages,
            vec![Message::assistant("one"), Message::assistant("two")]
        );
    }

    #[tokio::test]
    async fn record_session_title_persists_title_in_manifest_listing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(Some(
            temp.path().to_str().expect("temp path"),
        ));
        let session_id = SessionId::from("session-title");
        store
            .create_session(root_params(session_id.clone()))
            .await
            .expect("create session");

        store
            .record_session_title(&session_id, "Implement /sessions table")
            .expect("record title");

        let listed = store.list_sessions(None).expect("list sessions");
        assert_eq!(
            listed[0].title.as_deref(),
            Some("Implement /sessions table")
        );
    }

    #[tokio::test]
    async fn close_session_keeps_session_available_for_resume() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(Some(
            temp.path().to_str().expect("temp path"),
        ));
        let session_id = SessionId::from("session-closed");
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
            .expect("create session");

        store
            .close_session(&session_id, recorder.as_ref())
            .await
            .expect("close session");

        let listed = store.list_sessions(None).expect("list sessions");
        assert_eq!(listed.len(), 1);

        let loaded = store.load_session(&session_id).expect("load result");
        assert!(loaded.is_some());
    }

    #[tokio::test]
    async fn agent_graph_store_lists_latest_open_children() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(Some(
            temp.path().to_str().expect("temp path"),
        ));
        let parent = SessionId::from("parent");
        let child = SessionId::from("child");
        let path = AgentPath("/root/child".to_string());

        store
            .create_session(root_params(parent.clone()))
            .await
            .expect("create");
        store
            .upsert_agent_edge(
                parent.clone(),
                child.clone(),
                path,
                Some("coder".to_string()),
                AgentEdgeStatus::Open,
            )
            .await
            .expect("open edge");

        let open = store
            .list_agent_children(&parent, Some(AgentEdgeStatus::Open))
            .expect("list");

        assert_eq!(open.len(), 1);
        assert_eq!(open[0].child_session_id, child);
        assert_eq!(open[0].child_role.as_deref(), Some("coder"));
    }

    #[tokio::test]
    async fn agent_graph_store_closed_child_is_not_returned_as_open() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(Some(
            temp.path().to_str().expect("temp path"),
        ));
        let parent = SessionId::from("parent");
        let child = SessionId::from("child");
        let path = AgentPath("/root/child".to_string());

        store
            .create_session(root_params(parent.clone()))
            .await
            .expect("create");
        store
            .upsert_agent_edge(
                parent.clone(),
                child.clone(),
                path,
                Some("coder".to_string()),
                AgentEdgeStatus::Open,
            )
            .await
            .expect("open edge");
        store
            .set_agent_edge_status(&parent, &child, AgentEdgeStatus::Closed)
            .await
            .expect("close edge");

        let open = store
            .list_agent_children(&parent, Some(AgentEdgeStatus::Open))
            .expect("list open");

        assert!(open.is_empty());
    }

    #[tokio::test]
    async fn agent_graph_store_missing_child_status_update_is_noop() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = FileSessionStore::new(Some(
            temp.path().to_str().expect("temp path"),
        ));
        let parent = SessionId::from("parent");
        let missing = SessionId::from("missing-child");

        store
            .create_session(root_params(parent.clone()))
            .await
            .expect("create");

        store
            .set_agent_edge_status(&parent, &missing, AgentEdgeStatus::Closed)
            .await
            .expect("missing edge close should be a no-op");

        let children = store
            .list_agent_children(&parent, None)
            .expect("list children");
        assert!(children.is_empty());
    }
}
