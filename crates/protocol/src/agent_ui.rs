//! UI-only agent metadata used by ACP `_meta` extensions.

use serde::{Deserialize, Serialize};

use crate::{AgentPath, AgentStatus, SessionId};

/// Metadata required by frontends to show and switch agent sessions.
#[derive(
    Clone,
    Debug,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct AgentUiMetadata {
    /// ACP/kernel session id for the represented agent.
    pub session_id: SessionId,
    /// Parent session id, absent only for the root agent.
    #[builder(default, setter(strip_option))]
    pub parent_session_id: Option<SessionId>,
    /// Canonical agent path used by the runtime.
    pub agent_path: AgentPath,
    /// Human-friendly nickname shown in the picker.
    #[builder(default, setter(strip_option))]
    pub nickname: Option<String>,
    /// Role name shown next to the nickname.
    #[builder(default, setter(strip_option))]
    pub role: Option<String>,
    /// Latest known runtime status.
    pub status: AgentStatus,
    /// True when this entry represents the main/root agent.
    pub is_root: bool,
}

/// Kind of UI metadata patch carried through ACP `_meta`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentUiEventKind {
    Snapshot,
    Upsert,
    Status,
}

/// Versioned UI metadata patch sent to clients.
#[derive(
    Clone,
    Debug,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    typed_builder::TypedBuilder,
)]
pub struct AgentUiMetadataPatch {
    /// Extension payload version.
    pub version: u32,
    /// Patch semantics for the contained entries.
    pub event: AgentUiEventKind,
    /// Agent entries included in this patch.
    pub agents: Vec<AgentUiMetadata>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentPath, AgentStatus, SessionId};

    /// Verifies UI agent metadata serializes with stable snake_case fields.
    #[test]
    fn agent_ui_metadata_serializes_root_and_child_fields() {
        let entry = AgentUiMetadata::builder()
            .session_id(SessionId::from("child-session"))
            .parent_session_id(SessionId::from("root-session"))
            .agent_path(AgentPath::root().join("inspect"))
            .nickname("finder".to_string())
            .role("worker".to_string())
            .status(AgentStatus::Running)
            .is_root(false)
            .build();

        let value =
            serde_json::to_value(entry).expect("metadata should serialize");

        assert_eq!(value["session_id"], "child-session");
        assert_eq!(value["parent_session_id"], "root-session");
        assert_eq!(value["agent_path"], "/root/inspect");
        assert_eq!(value["nickname"], "finder");
        assert_eq!(value["role"], "worker");
        assert_eq!(value["status"], "running");
        assert_eq!(value["is_root"], false);
    }
}
