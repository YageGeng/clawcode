use std::collections::HashMap;
use std::io;

use async_trait::async_trait;
use protocol::{AgentPath, SessionId};

use crate::record::{AgentEdgeRecord, AgentEdgeStatusRecord};

/// Durable lifecycle status for an agent graph edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentEdgeStatus {
    /// The parent-child edge is active and should be restored.
    Open,
    /// The child edge has been explicitly closed.
    Closed,
}

impl From<AgentEdgeStatusRecord> for AgentEdgeStatus {
    /// Convert persisted edge status into the graph-store status model.
    fn from(status: AgentEdgeStatusRecord) -> Self {
        match status {
            AgentEdgeStatusRecord::Open => Self::Open,
            AgentEdgeStatusRecord::Closed => Self::Closed,
        }
    }
}

impl From<AgentEdgeStatus> for AgentEdgeStatusRecord {
    /// Convert graph-store edge status into the persisted status model.
    fn from(status: AgentEdgeStatus) -> Self {
        match status {
            AgentEdgeStatus::Open => Self::Open,
            AgentEdgeStatus::Closed => Self::Closed,
        }
    }
}

/// Durable parent-child edge returned by the graph store.
#[derive(Clone, Debug, typed_builder::TypedBuilder)]
pub struct AgentEdge {
    /// Parent session id that owns the child edge.
    pub parent_session_id: SessionId,
    /// Child session id referenced by the edge.
    pub child_session_id: SessionId,
    /// Child agent path used for routing after restore.
    pub child_agent_path: AgentPath,
    /// Optional role name for the child agent.
    #[builder(default, setter(strip_option))]
    pub child_role: Option<String>,
    /// Latest durable edge status.
    pub status: AgentEdgeStatus,
}

/// Persistence boundary for durable agent topology.
#[async_trait]
pub trait AgentGraphStore: Send + Sync {
    /// Append or refresh a parent-child edge status.
    async fn upsert_agent_edge(
        &self,
        parent_session_id: SessionId,
        child_session_id: SessionId,
        child_agent_path: AgentPath,
        child_role: Option<String>,
        status: AgentEdgeStatus,
    ) -> io::Result<()>;

    /// Append a status update for an existing parent-child edge.
    async fn set_agent_edge_status(
        &self,
        parent_session_id: &SessionId,
        child_session_id: &SessionId,
        status: AgentEdgeStatus,
    ) -> io::Result<()>;

    /// Return the latest child edges for a parent, optionally filtered by status.
    fn list_agent_children(
        &self,
        parent_session_id: &SessionId,
        status: Option<AgentEdgeStatus>,
    ) -> io::Result<Vec<AgentEdge>>;
}

/// Fold append-only edge records so the latest record for each child wins.
pub(crate) fn fold_agent_edges(
    parent_session_id: SessionId,
    records: Vec<AgentEdgeRecord>,
    status_filter: Option<AgentEdgeStatus>,
) -> Vec<AgentEdge> {
    let mut latest = HashMap::<SessionId, AgentEdge>::new();
    for record in records
        .into_iter()
        .filter(|record| record.parent_session_id == parent_session_id)
    {
        let child_role = if record.child_role.is_empty() {
            None
        } else {
            Some(record.child_role)
        };
        let builder = AgentEdge::builder()
            .parent_session_id(record.parent_session_id)
            .child_session_id(record.child_session_id.clone())
            .child_agent_path(record.child_agent_path)
            .status(record.status.into());
        // `strip_option` keeps normal callers ergonomic, so set the role only when present.
        let edge = match child_role {
            Some(child_role) => builder.child_role(child_role).build(),
            None => builder.build(),
        };
        latest.insert(record.child_session_id, edge);
    }
    let mut edges = latest.into_values().collect::<Vec<_>>();
    if let Some(status) = status_filter {
        edges.retain(|edge| edge.status == status);
    }
    edges.sort_by(|left, right| left.child_session_id.0.cmp(&right.child_session_id.0));
    edges
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::AgentEdgeRecord;
    use protocol::{AgentPath, SessionId};

    #[test]
    fn latest_edge_status_wins_when_child_is_closed() {
        let parent = SessionId::from("parent");
        let child = SessionId::from("child");
        let path = AgentPath("/root/child".to_string());
        let records = vec![
            edge_record(&parent, &child, &path, "reviewer", AgentEdgeStatus::Open),
            edge_record(&parent, &child, &path, "reviewer", AgentEdgeStatus::Closed),
        ];

        let children = fold_agent_edges(parent.clone(), records, Some(AgentEdgeStatus::Open));

        assert!(children.is_empty());
    }

    #[test]
    fn folded_edges_preserve_latest_role_and_path() {
        let parent = SessionId::from("parent");
        let child = SessionId::from("child");
        let first_path = AgentPath("/root/old".to_string());
        let latest_path = AgentPath("/root/new".to_string());
        let records = vec![
            edge_record(&parent, &child, &first_path, "", AgentEdgeStatus::Open),
            edge_record(
                &parent,
                &child,
                &latest_path,
                "coder",
                AgentEdgeStatus::Open,
            ),
        ];

        let children = fold_agent_edges(parent.clone(), records, None);

        assert_eq!(children.len(), 1);
        assert_eq!(children[0].child_agent_path, latest_path);
        assert_eq!(children[0].child_role.as_deref(), Some("coder"));
    }

    fn edge_record(
        parent: &SessionId,
        child: &SessionId,
        path: &AgentPath,
        role: &str,
        status: AgentEdgeStatus,
    ) -> AgentEdgeRecord {
        AgentEdgeRecord::builder()
            .parent_session_id(parent.clone())
            .child_session_id(child.clone())
            .child_agent_path(path.clone())
            .child_role(role.to_string())
            .status(status.into())
            .build()
    }
}
