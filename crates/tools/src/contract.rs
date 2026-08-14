use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use protocol::{SessionId, ToolCall, ToolDefinition, ToolResult, TurnId};
use tokio_util::sync::CancellationToken;

/// Receives replaceable partial snapshots from streaming tools.
pub trait ToolUpdateSink: Send + Sync {
    /// Publishes the latest visible result for one correlated tool call.
    fn publish(&self, update: ToolResult);
}

/// Update sink used when a direct tool caller does not consume partial output.
#[derive(Debug, Clone, Copy, Default)]
pub struct DiscardToolUpdates;

impl ToolUpdateSink for DiscardToolUpdates {
    /// Intentionally discards a partial result while final results remain available.
    fn publish(&self, _update: ToolResult) {}
}

/// Runtime context supplied to every tool invocation.
#[derive(Clone, typed_builder::TypedBuilder)]
pub struct ToolExecutionContext {
    /// Session that owns the invocation.
    pub session_id: SessionId,
    /// Turn that owns the invocation.
    pub turn_id: TurnId,
    /// Working directory selected for the turn.
    pub cwd: PathBuf,

    /// Shared token cancelled when the owning Turn is interrupted.
    pub cancellation: CancellationToken,

    /// Destination for replaceable partial tool snapshots.
    pub updates: Arc<dyn ToolUpdateSink>,
}

impl ToolExecutionContext {
    /// Rejects new work after the owning Turn has been cancelled.
    pub fn ensure_active(&self) -> Result<(), ToolError> {
        if self.cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        Ok(())
    }

    /// Forwards one latest-value partial result to the runtime update channel.
    pub fn publish(&self, update: ToolResult) {
        self.updates.publish(update);
    }
}

/// Typed failures produced by tool registration and execution.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolError {
    /// Owning Turn was cancelled before the tool completed.
    #[error("Operation aborted")]
    Cancelled,
    /// Requested tool name was not registered.
    #[error("tool not found: {0}")]
    NotFound(String),
    /// Tool arguments did not satisfy the tool schema.
    #[error("invalid arguments for {tool}: {message}")]
    InvalidArguments {
        /// Tool that rejected its arguments.
        tool: String,
        /// Deserialization or validation diagnostic.
        message: String,
    },
    /// Two factory participants registered the same tool name.
    #[error("tool already registered: {0}")]
    Duplicate(String),

    /// Registered tool failed during execution.
    #[error("{message}")]
    Execution {
        /// Tool that failed.
        tool: String,
        /// Backend diagnostic.
        message: String,
    },
}

/// Public tool interface aligned with pi's definition-plus-execute contract.
#[async_trait]
pub trait AgentTool: Send + Sync {
    /// Returns the model-facing name, description, and argument schema.
    fn definition(&self) -> ToolDefinition;

    /// Executes one correlated tool call in its turn context.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError>;
}

/// Factory interface used by kernel construction to acquire tool registries.
pub trait ToolFactory: Send + Sync {
    /// Creates one complete registry for a kernel lifecycle.
    fn create(&self) -> Result<ToolRegistry, ToolError>;
}

/// Deterministically ordered registry of heterogeneous agent tools.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn AgentTool>>,
}

impl ToolRegistry {
    /// Registers a tool under the name declared by its definition.
    pub fn register(
        &mut self,
        tool: Arc<dyn AgentTool>,
    ) -> Result<(), ToolError> {
        let name = tool.definition().name;
        if self.tools.contains_key(&name) {
            return Err(ToolError::Duplicate(name));
        }
        self.tools.insert(name, tool);
        Ok(())
    }

    /// Merges another registry while preserving duplicate-name validation.
    pub fn merge(&mut self, other: ToolRegistry) -> Result<(), ToolError> {
        for tool in other.tools.into_values() {
            self.register(tool)?;
        }
        Ok(())
    }

    /// Returns registered tool names in deterministic lexical order.
    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        self.tools.keys().map(String::as_str).collect()
    }

    /// Returns model-facing definitions in deterministic lexical order.
    #[must_use]
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|tool| tool.definition()).collect()
    }

    /// Dispatches a call to the registered tool with the same name.
    pub async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        let tool = self
            .tools
            .get(&call.name)
            .map(Arc::clone)
            .ok_or_else(|| ToolError::NotFound(call.name.clone()))?;
        tool.execute(call, context).await
    }
}
