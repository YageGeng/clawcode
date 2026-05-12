//! Operation types submitted from the frontend / client to the kernel.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::agent::AgentPath;
use crate::permission::ReviewDecision;
use crate::session::SessionId;

/// Operation submitted from the frontend / client to the kernel.
///
/// Each variant represents a command the kernel should execute.
/// Responses come as streaming [`Event`](crate::event::Event)s.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Create a new session at the given working directory.
    NewSession { cwd: PathBuf },
    /// Load a previously persisted session.
    LoadSession { session_id: SessionId },
    /// Submit a user prompt to an active session.
    Prompt {
        session_id: SessionId,
        /// The user's text input.
        text: String,
    },
    /// Cancel the currently running turn in a session.
    Cancel { session_id: SessionId },
    /// Change the session's approval/sandboxing mode.
    SetMode { session_id: SessionId, mode: String },
    /// Switch the model for a session.
    SetModel {
        session_id: SessionId,
        provider_id: String,
        model_id: String,
    },
    /// Close a session and release its resources.
    CloseSession { session_id: SessionId },
    /// Spawn a sub-agent from a parent session.
    SpawnAgent {
        parent_session: SessionId,
        agent_path: AgentPath,
        role: String,
        prompt: String,
    },
    /// Deliver a message between agents.
    InterAgentMessage {
        from: AgentPath,
        to: AgentPath,
        content: String,
    },
    /// User's response to an exec approval request.
    ExecApprovalResponse {
        /// Matches the `call_id` from ExecApprovalRequested.
        call_id: String,
        /// User's decision.
        decision: ReviewDecision,
    },
    /// User's response to a patch approval request.
    PatchApprovalResponse {
        call_id: String,
        decision: ReviewDecision,
    },
}
