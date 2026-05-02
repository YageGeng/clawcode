use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use snafu::{OptionExt, ResultExt};

use crate::{
    Result,
    collaboration::{
        AgentRuntimeContext, CloseAgentRequest, ListAgentsRequest, SendAgentInputRequest,
        SpawnAgentRequest, WaitAgentRequest,
    },
    context::{StructuredToolOutput, ToolInvocation, ToolMetadata, ToolOutput},
    error::RuntimeSnafu,
    handler::ToolHandler,
};

/// Parses arguments for the built-in `spawn_agent` tool.
#[derive(Debug, Deserialize)]
struct SpawnAgentArgs {
    name: Option<String>,
    task: Option<String>,
    cwd: Option<String>,
    system_prompt: Option<String>,
    current_date: Option<String>,
    timezone: Option<String>,
}

/// Parses arguments for the built-in `send_agent_input` tool.
#[derive(Debug, Deserialize)]
struct SendAgentInputArgs {
    target: String,
    input: String,
    #[serde(default)]
    interrupt: bool,
}

/// Parses arguments for the built-in `wait_agent` tool.
#[derive(Debug, Deserialize)]
struct WaitAgentArgs {
    #[serde(default)]
    targets: Vec<String>,
    timeout_ms: Option<u64>,
}

/// Parses arguments for the built-in `close_agent` tool.
#[derive(Debug, Deserialize)]
struct CloseAgentArgs {
    target: String,
}

/// Registers and optionally starts a mailbox-backed child agent.
pub struct SpawnAgentTool;

impl SpawnAgentTool {
    /// Builds the stable caller context forwarded into the collaboration runtime.
    fn agent_runtime_context(invocation: &ToolInvocation) -> AgentRuntimeContext {
        invocation.context.agent.clone()
    }
}

#[async_trait]
impl ToolHandler for SpawnAgentTool {
    fn name(&self) -> &'static str {
        "spawn_agent"
    }

    fn description(&self) -> &'static str {
        "Spawn a mailbox-backed child agent with its own thread and optional initial task."
    }

    fn prompt_snippet(&self) -> Option<&'static str> {
        Some("Spawn a child agent with its own mailbox-backed thread.")
    }

    /// Hides `spawn_agent` from agents that have reached the configured depth limit.
    fn visible_when(&self) -> Option<fn(&AgentRuntimeContext) -> bool> {
        Some(
            |agent: &AgentRuntimeContext| match agent.max_subagent_depth {
                Some(max) => agent.subagent_depth < max,
                None => true,
            },
        )
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Optional display name for the child agent."
                },
                "task": {
                    "type": "string",
                    "description": "Optional first task to run immediately after spawning."
                },
                "cwd": {
                    "type": "string",
                    "description": "Optional working-directory override for the child agent."
                },
                "system_prompt": {
                    "type": "string",
                    "description": "Optional system-prompt override for the child agent."
                },
                "current_date": {
                    "type": "string",
                    "description": "Optional current-date override exposed to the child agent."
                },
                "timezone": {
                    "type": "string",
                    "description": "Optional timezone override exposed to the child agent."
                }
            },
            "additionalProperties": false
        })
    }

    /// Delegates child-agent creation to the injected collaboration runtime.
    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let args: SpawnAgentArgs = invocation.parse_function_arguments("spawn-agent-parse-args")?;
        let runtime = invocation
            .context
            .collaboration_runtime
            .as_ref()
            .context(RuntimeSnafu {
                message: "collaboration runtime is unavailable".to_string(),
                stage: "spawn-agent-runtime".to_string(),
            })?;
        let response = runtime
            .spawn_agent(SpawnAgentRequest {
                session_id: invocation.context.session_id.clone(),
                thread_id: invocation.context.thread_id.clone(),
                origin: Self::agent_runtime_context(&invocation),
                name: args.name,
                task: args.task,
                cwd: args.cwd,
                system_prompt: args.system_prompt,
                current_date: args.current_date,
                timezone: args.timezone,
            })
            .await?;
        let structured = serde_json::to_value(&response).context(crate::error::JsonSnafu {
            stage: "spawn-agent-response-json".to_string(),
        })?;

        Ok(ToolOutput {
            text: format!(
                "spawned agent {} at {}",
                response.agent.agent_id, response.agent.path
            ),
            structured: StructuredToolOutput::json_value(structured),
        })
    }
}

/// Queues work for an existing mailbox-backed child agent.
pub struct SendAgentInputTool;

#[async_trait]
impl ToolHandler for SendAgentInputTool {
    fn name(&self) -> &'static str {
        "send_agent_input"
    }

    fn description(&self) -> &'static str {
        "Queue more work for a mailbox-backed child agent."
    }

    fn prompt_snippet(&self) -> Option<&'static str> {
        Some("Send another input to an existing child agent.")
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "target": {
                    "type": "string",
                    "description": "Agent id or path to receive the input."
                },
                "input": {
                    "type": "string",
                    "description": "Task or follow-up input for the target agent."
                },
                "interrupt": {
                    "type": "boolean",
                    "description": "When true, place this input at the front of the target queue."
                }
            },
            "required": ["target", "input"],
            "additionalProperties": false
        })
    }

    /// Delegates follow-up input delivery to the collaboration runtime.
    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let args: SendAgentInputArgs =
            invocation.parse_function_arguments("send-agent-input-parse-args")?;
        let runtime = invocation
            .context
            .collaboration_runtime
            .as_ref()
            .context(RuntimeSnafu {
                message: "collaboration runtime is unavailable".to_string(),
                stage: "send-agent-input-runtime".to_string(),
            })?;
        let response = runtime
            .send_agent_input(SendAgentInputRequest {
                session_id: invocation.context.session_id.clone(),
                thread_id: invocation.context.thread_id.clone(),
                origin: invocation.context.agent.clone(),
                target: args.target,
                input: args.input,
                interrupt: args.interrupt,
            })
            .await?;
        let structured = serde_json::to_value(&response).context(crate::error::JsonSnafu {
            stage: "send-agent-input-response-json".to_string(),
        })?;

        Ok(ToolOutput {
            text: format!("queued input for agent {}", response.agent.agent_id),
            structured: StructuredToolOutput::json_value(structured),
        })
    }
}

/// Waits for the next mailbox event emitted by one or more child agents.
pub struct WaitAgentTool;

#[async_trait]
impl ToolHandler for WaitAgentTool {
    fn name(&self) -> &'static str {
        "wait_agent"
    }

    fn description(&self) -> &'static str {
        "Wait for the next mailbox event produced by a child agent."
    }

    fn prompt_snippet(&self) -> Option<&'static str> {
        Some("Wait for mailbox activity from child agents.")
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "targets": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional agent ids or paths to wait on. Empty waits on any child."
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Optional timeout in milliseconds."
                }
            },
            "additionalProperties": false
        })
    }

    /// Extends the outer router timeout so mailbox waits are bounded by the
    /// requested `timeout_ms` rather than the generic short tool timeout.
    fn metadata(&self) -> ToolMetadata {
        ToolMetadata {
            timeout: Duration::from_secs(60 * 60),
            ..ToolMetadata::default()
        }
    }

    /// Delegates mailbox waiting to the collaboration runtime.
    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let args: WaitAgentArgs = invocation.parse_function_arguments("wait-agent-parse-args")?;
        let runtime = invocation
            .context
            .collaboration_runtime
            .as_ref()
            .context(RuntimeSnafu {
                message: "collaboration runtime is unavailable".to_string(),
                stage: "wait-agent-runtime".to_string(),
            })?;
        let response = runtime
            .wait_agent(WaitAgentRequest {
                session_id: invocation.context.session_id.clone(),
                thread_id: invocation.context.thread_id.clone(),
                origin: invocation.context.agent.clone(),
                targets: args.targets,
                timeout_ms: args.timeout_ms,
            })
            .await?;
        let structured = serde_json::to_value(&response).context(crate::error::JsonSnafu {
            stage: "wait-agent-response-json".to_string(),
        })?;

        Ok(ToolOutput {
            text: response
                .event
                .as_ref()
                .map(|event| format!("received {:?} from {}", event.event_kind, event.agent_id))
                .unwrap_or_else(|| "wait timed out".to_string()),
            structured: StructuredToolOutput::json_value(structured),
        })
    }
}

/// Closes one mailbox-backed agent subtree.
pub struct CloseAgentTool;

#[async_trait]
impl ToolHandler for CloseAgentTool {
    fn name(&self) -> &'static str {
        "close_agent"
    }

    fn description(&self) -> &'static str {
        "Close an agent subtree and stop accepting new work for it."
    }

    fn prompt_snippet(&self) -> Option<&'static str> {
        Some("Close a child agent when its work is no longer needed.")
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "target": {
                    "type": "string",
                    "description": "Agent id or path to close."
                }
            },
            "required": ["target"],
            "additionalProperties": false
        })
    }

    /// Delegates close requests to the collaboration runtime.
    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let args: CloseAgentArgs = invocation.parse_function_arguments("close-agent-parse-args")?;
        let runtime = invocation
            .context
            .collaboration_runtime
            .as_ref()
            .context(RuntimeSnafu {
                message: "collaboration runtime is unavailable".to_string(),
                stage: "close-agent-runtime".to_string(),
            })?;
        let response = runtime
            .close_agent(CloseAgentRequest {
                session_id: invocation.context.session_id.clone(),
                thread_id: invocation.context.thread_id.clone(),
                origin: invocation.context.agent.clone(),
                target: args.target,
            })
            .await?;
        let structured = serde_json::to_value(&response).context(crate::error::JsonSnafu {
            stage: "close-agent-response-json".to_string(),
        })?;

        Ok(ToolOutput {
            text: format!("closed agent {}", response.agent.agent_id),
            structured: StructuredToolOutput::json_value(structured),
        })
    }
}

/// Lists the visible child-agent graph for the current session.
pub struct ListAgentsTool;

#[async_trait]
impl ToolHandler for ListAgentsTool {
    fn name(&self) -> &'static str {
        "list_agents"
    }

    fn description(&self) -> &'static str {
        "List the visible mailbox-backed agents in the current session."
    }

    fn prompt_snippet(&self) -> Option<&'static str> {
        Some("List the child agents that currently exist in this session.")
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    /// Delegates graph listing to the collaboration runtime.
    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput> {
        let runtime = invocation
            .context
            .collaboration_runtime
            .as_ref()
            .context(RuntimeSnafu {
                message: "collaboration runtime is unavailable".to_string(),
                stage: "list-agents-runtime".to_string(),
            })?;
        let response = runtime
            .list_agents(ListAgentsRequest {
                session_id: invocation.context.session_id.clone(),
                thread_id: invocation.context.thread_id.clone(),
                origin: invocation.context.agent.clone(),
            })
            .await?;
        let structured = serde_json::to_value(&response).context(crate::error::JsonSnafu {
            stage: "list-agents-response-json".to_string(),
        })?;

        Ok(ToolOutput {
            text: format!("listed {} agents", response.agents.len()),
            structured: StructuredToolOutput::json_value(structured),
        })
    }
}
