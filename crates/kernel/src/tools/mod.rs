pub mod builtin;
pub mod executor;
pub mod registry;

use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::{
    Result,
    session::{SessionId, ThreadId},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalRequirement {
    Never,
    Always,
}

#[derive(Debug, Clone)]
pub struct ToolMetadata {
    pub risk_level: RiskLevel,
    pub approval: ApprovalRequirement,
    pub timeout: Duration,
}

impl Default for ToolMetadata {
    fn default() -> Self {
        Self {
            risk_level: RiskLevel::Low,
            approval: ApprovalRequirement::Never,
            timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolContext {
    pub session_id: SessionId,
    pub thread_id: ThreadId,
}

impl ToolContext {
    /// Builds the per-invocation tool context from runtime identifiers.
    pub fn new(session_id: SessionId, thread_id: ThreadId) -> Self {
        Self {
            session_id,
            thread_id,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolOutput {
    pub text: String,
    pub structured: serde_json::Value,
}

impl ToolOutput {
    /// Builds a text-first output while still preserving a structured payload.
    pub fn text(text: impl Into<String>) -> Self {
        let text = text.into();
        Self {
            text: text.clone(),
            structured: serde_json::json!({ "text": text }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallRequest {
    pub id: String,
    pub call_id: Option<String>,
    pub name: String,
    pub arguments: serde_json::Value,
}

impl ToolCallRequest {
    /// Builds a tool call description that can be executed by the runtime.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        Self {
            id: id.into(),
            call_id: None,
            name: name.into(),
            arguments,
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;

    fn description(&self) -> &'static str;

    fn parameters(&self) -> serde_json::Value;

    fn metadata(&self) -> ToolMetadata {
        ToolMetadata::default()
    }

    fn definition(&self) -> llm::completion::ToolDefinition {
        llm::completion::ToolDefinition {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters(),
        }
    }

    /// Executes one tool invocation using JSON arguments and runtime context.
    async fn execute(&self, args: serde_json::Value, context: ToolContext) -> Result<ToolOutput>;
}
