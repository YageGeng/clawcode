//! Codex multi-agent v2 tools over the mailbox and the owner's roster.

use std::hash::BuildHasher;
use std::sync::{Arc, Weak};

use ::tools::{AgentTool, ToolError, ToolExecutionContext};
use async_trait::async_trait;
use protocol::{
    AgentMail, AgentName, AgentNickname, AgentPath, ContentBlock, MailKind,
    SessionOwnership, SessionTitle, ThinkingLevel, ToolCall, ToolCallId,
    ToolDefinition, ToolPromptContribution, ToolResult, ToolResultDetails,
};

use super::SubagentError;
use super::fork::ForkedHistory;
use super::path::{AgentTarget, AgentTree};
use super::roster::{ChildEntry, ChildStatus};
use crate::runtime::{
    Kernel, KernelError, MailDelivery, MailPlacement, OwnedSessionRequest,
    Session,
};

/// Builds one tool reply. A refusal (`Err`) stays correctable text for the
/// model but is marked as an error, as Codex returns `RespondToModel` failures
/// with `success: false`.
fn reply(
    call_id: ToolCallId,
    outcome: Result<String, String>,
    details: Option<ToolResultDetails>,
) -> ToolResult {
    let (text, is_error) = match outcome {
        Ok(text) => (text, false),
        Err(text) => (text, true),
    };
    let result = ToolResult::builder()
        .tool_call_id(call_id)
        .blocks(vec![ContentBlock::Text { text }])
        .is_error(is_error);
    match details {
        Some(details) => result.details(details).build(),
        None => result.build(),
    }
}

/// Reads one optional string argument.
fn string_arg<'a>(call: &'a ToolCall, name: &str) -> Option<&'a str> {
    call.arguments.get(name).and_then(serde_json::Value::as_str)
}

/// Validated `spawn_agent` arguments.
#[derive(typed_builder::TypedBuilder)]
struct SpawnArgs {
    name: AgentName,
    message: String,
    fork: bool,
    #[builder(default)]
    model: Option<String>,
    #[builder(default)]
    thinking_level: Option<ThinkingLevel>,
}

impl TryFrom<&ToolCall> for SpawnArgs {
    type Error = String;

    /// Checks every argument once; the error is correctable text for the model.
    fn try_from(call: &ToolCall) -> Result<Self, Self::Error> {
        let name = AgentName::try_from(
            string_arg(call, "task_name")
                .ok_or("spawn_agent needs task_name and message.")?,
        )?;
        let message = string_arg(call, "message")
            .ok_or("spawn_agent needs task_name and message.")?;
        if message.trim().is_empty() {
            return Err("message must not be empty".into());
        }
        // Codex accepts legacy turn counts as a full fork.
        let fork = match string_arg(call, "fork_turns").map(str::trim) {
            None | Some("") => true,
            Some(value) if value.eq_ignore_ascii_case("none") => false,
            Some(value)
                if value.eq_ignore_ascii_case("all")
                    || value.parse::<std::num::NonZeroUsize>().is_ok() =>
            {
                true
            }
            Some(_) => return Err("fork_turns must be `none` or `all`".into()),
        };
        let thinking_level = string_arg(call, "reasoning_effort")
            .map(|effort| match effort {
                "none" => Ok(ThinkingLevel::Off),
                "minimal" => Ok(ThinkingLevel::Minimal),
                "low" => Ok(ThinkingLevel::Low),
                "medium" => Ok(ThinkingLevel::Medium),
                "high" => Ok(ThinkingLevel::High),
                "xhigh" => Ok(ThinkingLevel::XHigh),
                other => Err(format!(
                    "reasoning_effort `{other}` must be one of none, minimal, low, medium, high, xhigh"
                )),
            })
            .transpose()?;
        Ok(Self::builder()
            .name(name)
            .message(message.to_string())
            .fork(fork)
            .model(string_arg(call, "model").map(str::to_string))
            .thinking_level(thinking_level)
            .build())
    }
}

/// Spawns one named child that starts on a `new_task` mail.
pub(super) struct SpawnAgentTool {
    pub(super) session: Weak<Session>,
    pub(super) kernel: Kernel,
}

impl SpawnAgentTool {
    /// Admits, creates, and wakes one child; returns the model-facing reply
    /// and the child's details, or a correctable refusal.
    async fn spawn(
        &self,
        session: &Arc<Session>,
        args: SpawnArgs,
        context: &ToolExecutionContext,
    ) -> Result<Result<(String, ToolResultDetails), String>, KernelError> {
        let model = match args
            .model
            .as_deref()
            .map(|reference| session.model.resolve_reference(reference))
            .transpose()
        {
            Ok(model) => model,
            Err(text) => return Ok(Err(text)),
        };
        let tree = AgentTree::of(&self.kernel, &context.session_id, session)?;
        let path = AgentPath::child(&args.name);
        let spawning = tree.owner().roster.spawning.lock().await;
        let admission = tree.with_children(|children| {
            if children.contains_key(&args.name) {
                return Err(format!(
                    "{} already exists; use followup_task.",
                    args.name
                ));
            }
            if let Some(text) = tree.over_limit(children, None) {
                return Err(text);
            }
            Ok(children
                .values()
                .map(|entry| entry.nickname.clone())
                .collect::<std::collections::HashSet<_>>())
        })?;
        let used = match admission {
            Ok(used) => used,
            Err(text) => {
                tracing::info!("refused spawn of {}: {}", path, text);
                return Ok(Err(text));
            }
        };
        let index = usize::try_from(
            std::collections::hash_map::RandomState::new().hash_one(&args.name),
        )
        .unwrap_or_default();
        let nickname = AgentNickname::allocate(&used, index).into_inner();
        let history = if args.fork {
            ForkedHistory::from(session.transcript.history()?.as_slice())
        } else {
            ForkedHistory::default()
        }
        .with_role_note(&self.kernel, &path)?;
        tracing::info!(
            "started spawn of {} ({}) with fork {}",
            path,
            nickname,
            args.fork
        );
        let title = SessionTitle::try_from(nickname.clone())
            .map_err(|error| KernelError::Protocol(error.to_string()))?;
        let child = self
            .kernel
            .create_owned_session(
                OwnedSessionRequest::builder()
                    .ownership(SessionOwnership {
                        session_id: tree.owner_id().clone(),
                        name: args.name.clone(),
                        nickname: nickname.clone(),
                    })
                    .title(title)
                    .model(model)
                    .thinking_level(args.thinking_level)
                    .history(history)
                    .build(),
            )
            .await?;
        tree.with_children(|children| {
            children.insert(
                args.name.clone(),
                ChildEntry {
                    session_id: child.clone(),
                    nickname,
                    status: ChildStatus::PendingInit,
                },
            );
        })?;
        drop(spawning);
        let task = AgentMail::builder()
            .author(tree.caller().clone())
            .recipient(path.clone())
            .kind(MailKind::NewTask)
            .payload(args.message)
            .build();
        if let Err(error) = self
            .kernel
            .deliver_mail(
                MailDelivery::builder()
                    .recipient(child.clone())
                    .mail(task)
                    .link_trace(Some(context.trace_id.clone()))
                    .build(),
            )
            .await
        {
            tracing::warn!("failed to start spawned {}: {}", path, error);
            tree.with_children(|children| children.remove(&args.name))?;
            return Err(error);
        }
        tracing::info!("completed spawn of {} as session {}", path, child);
        Ok(Ok((
            serde_json::json!({ "task_name": path.as_ref() }).to_string(),
            ToolResultDetails::Subagent { session_id: child },
        )))
    }
}

#[async_trait]
impl AgentTool for SpawnAgentTool {
    /// Advertises Codex v2 `spawn_agent` without nesting.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: protocol::SPAWN_AGENT_TOOL.into(),
            description: "Spawns an agent to work on the specified task. If you spawn_agent with task_name \"task_3\" the agent will have canonical task name `/root/task_3`; refer to it as `task_3` or `/root/task_3`. The spawned agent can send you and other running agents messages, and its final answer will be provided to you when it finishes. It cannot spawn agents of its own. Passing `fork_turns=\"none\"` will not pass any surrounding context to the spawned agent, whereas `fork_turns=\"all\"` (the default) provides it with all surrounding context.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "task_name": {
                        "type": "string",
                        "description": "Task name for the new agent. Use lowercase letters, digits, and underscores."
                    },
                    "message": {
                        "type": "string",
                        "description": "Initial plain-text task for the new agent."
                    },
                    "fork_turns": {
                        "type": "string",
                        "description": "Parent history to inherit. Defaults to `all`; use `none` to start without parent history. Only `all` and `none` are supported."
                    },
                    "model": {
                        "type": "string",
                        "description": "Model override for the new agent as provider/model. Omit unless an explicit override is needed."
                    },
                    "reasoning_effort": {
                        "type": "string",
                        "enum": ["none", "minimal", "low", "medium", "high", "xhigh"],
                        "description": "Reasoning effort override for the new agent. Omit to inherit the parent effort."
                    }
                },
                "required": ["task_name", "message"]
            }),
        }
    }

    /// Spawns a child, or replies with correctable usage text.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        context.ensure_active()?;
        let session =
            self.session.upgrade().ok_or(SubagentError::SessionGone {
                tool: protocol::SPAWN_AGENT_TOOL,
            })?;
        let spawned =
            match SpawnArgs::try_from(&call) {
                Ok(args) => self.spawn(&session, args, context).await.map_err(
                    |source| SubagentError::Kernel {
                        tool: protocol::SPAWN_AGENT_TOOL,
                        source,
                    },
                )?,
                Err(text) => Err(text),
            };
        let (outcome, details) = match spawned {
            Ok((text, details)) => (Ok(text), Some(details)),
            Err(text) => (Err(text), None),
        };
        Ok(reply(call.tool_call_id, outcome, details))
    }
}

/// Lists `/root` and its children with Codex statuses.
pub(super) struct ListAgentsTool {
    pub(super) session: Weak<Session>,
    pub(super) kernel: Kernel,
}

impl ListAgentsTool {
    /// Renders the tree as Codex's `{"agents": [...]}` JSON, filtered by path prefix.
    fn list(
        &self,
        session: &Arc<Session>,
        prefix: Option<&str>,
        context: &ToolExecutionContext,
    ) -> Result<String, KernelError> {
        let tree = AgentTree::of(&self.kernel, &context.session_id, session)?;
        let root_status = ChildStatus::Completed(None)
            .report(tree.is_running(tree.owner_id()))?;
        let mut agents = vec![(AgentPath::root(), root_status)];
        let children = tree.with_children(|children| {
            children
                .iter()
                .map(|(name, entry)| {
                    let running = tree.is_running(&entry.session_id);
                    Ok((AgentPath::child(name), entry.status.report(running)?))
                })
                .collect::<Result<Vec<_>, serde_json::Error>>()
        })??;
        agents.extend(children);
        let agents = agents
            .into_iter()
            .filter(|(path, _)| {
                prefix.is_none_or(|prefix| path.as_ref().starts_with(prefix))
            })
            .map(|(path, status)| {
                serde_json::json!({ "agent_name": path, "agent_status": status })
            })
            .collect::<Vec<_>>();
        Ok(serde_json::json!({ "agents": agents }).to_string())
    }
}

#[async_trait]
impl AgentTool for ListAgentsTool {
    /// Advertises Codex v2 `list_agents`.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: protocol::LIST_AGENTS_TOOL.into(),
            description: "List live agents in the current root thread tree. Optionally filter by task-path prefix.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path_prefix": {
                        "type": "string",
                        "description": "Task-path prefix filter without a trailing slash. Omit to list all live agents."
                    }
                }
            }),
        }
    }

    /// States the team's addressing for every agent (Codex's root usage hint).
    /// Children keep this tool, so `/root` and its children read one identical
    /// System Prompt and share its cached prefix; a child learns who it is from
    /// its role note instead.
    fn prompt_contribution(&self) -> ToolPromptContribution {
        ToolPromptContribution {
            snippet: None,
            guidelines: vec![
                "Agents are addressed by task path: `/root` is the primary agent working with the user, and the agents it spawns are `/root/<task_name>`; you are `/root` unless your context says you were spawned as another agent".into(),
                "Messages between agents arrive as user text starting with `Message Type:` (`new_task`, `message`, or `result`), then `Task name:` (the recipient), `Sender:`, and `Payload:`; a `result` is the final answer a spawned agent sends `/root` when it finishes a task".into(),
            ],
        }
    }

    /// Lists the caller's tree; children see their owner's tree.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        context.ensure_active()?;
        let session =
            self.session.upgrade().ok_or(SubagentError::SessionGone {
                tool: protocol::LIST_AGENTS_TOOL,
            })?;
        let text = self
            .list(&session, string_arg(&call, "path_prefix"), context)
            .map_err(|source| SubagentError::Kernel {
                tool: protocol::LIST_AGENTS_TOOL,
                source,
            })?;
        Ok(reply(call.tool_call_id, Ok(text), None))
    }
}

/// Validated `target` + `message` arguments of the mailbox tools.
struct MessageArgs<'a> {
    target: &'a str,
    message: &'a str,
}

impl<'a> TryFrom<&'a ToolCall> for MessageArgs<'a> {
    type Error = String;

    /// Requires both fields and a non-blank message.
    fn try_from(call: &'a ToolCall) -> Result<Self, Self::Error> {
        let (Some(target), Some(message)) =
            (string_arg(call, "target"), string_arg(call, "message"))
        else {
            return Err(format!("{} needs target and message.", call.name));
        };
        if message.trim().is_empty() {
            return Err("message must not be empty".into());
        }
        Ok(Self { target, message })
    }
}

/// Sends mail to another agent: queue-only `message`, or a waking `new_task`.
pub(super) struct MailTool {
    pub(super) session: Weak<Session>,
    pub(super) kernel: Kernel,
    /// `Message` for `send_message`, `NewTask` for `followup_task`.
    pub(super) kind: MailKind,
}

impl MailTool {
    /// Resolves the target, checks the rules, and delivers one mail, or
    /// returns a correctable refusal.
    async fn send(
        &self,
        session: &Arc<Session>,
        args: MessageArgs<'_>,
        context: &ToolExecutionContext,
    ) -> Result<Result<String, String>, KernelError> {
        let tree = AgentTree::of(&self.kernel, &context.session_id, session)?;
        let target = match tree.resolve(args.target)? {
            Ok(target) => target,
            Err(text) => return Ok(Err(text)),
        };
        let (path, recipient) = match target {
            AgentTarget::Root if self.kind == MailKind::NewTask => {
                return Ok(Err("root is not a spawned agent".into()));
            }
            AgentTarget::Root => (AgentPath::root(), tree.owner_id().clone()),
            AgentTarget::Child { path, session_id } => (path, session_id),
        };
        if &path == tree.caller() {
            return Ok(Err("an agent cannot message itself".into()));
        }
        // A follow-up that wakes an idle child takes a running slot.
        let _spawning = if self.kind == MailKind::NewTask {
            let guard = tree.owner().roster.spawning.lock().await;
            let refused = tree.with_children(|children| {
                tree.over_limit(children, Some(path.name()))
            })?;
            if let Some(text) = refused {
                tracing::info!("refused follow-up to {}: {}", path, text);
                return Ok(Err(text));
            }
            Some(guard)
        } else {
            None
        };
        let mail = AgentMail::builder()
            .author(tree.caller().clone())
            .recipient(path.clone())
            .kind(self.kind)
            .payload(args.message.to_string())
            .build();
        let placement = self
            .kernel
            .deliver_mail(
                MailDelivery::builder()
                    .recipient(recipient)
                    .mail(mail)
                    .link_trace(Some(context.trace_id.clone()))
                    .cancellation(Some(context.cancellation.clone()))
                    .build(),
            )
            .await?;
        if placement == MailPlacement::Woken {
            // The woken Run holds its slot until it settles.
            tree.with_children(|children| {
                if let Some(entry) = children.get_mut(path.name()) {
                    entry.status = ChildStatus::PendingInit;
                }
            })?;
        }
        Ok(Ok(if self.kind == MailKind::NewTask {
            format!("Task sent to {path}.")
        } else {
            format!("Message sent to {path}.")
        }))
    }

    /// Returns the tool name and the descriptions of the tool and its message.
    fn spec(&self) -> (&'static str, &'static str, &'static str) {
        if self.kind == MailKind::NewTask {
            (
                protocol::FOLLOWUP_TASK_TOOL,
                "Send a follow-up task to an existing non-root target agent and trigger a turn if it is idle. If the target is already running, deliver the task promptly at message boundaries or after the pending tool call completes.",
                "Follow-up task for the target agent.",
            )
        } else {
            (
                protocol::SEND_MESSAGE_TOOL,
                "Send a message to an existing agent. The message will be delivered promptly. Does not trigger a new turn.",
                "Message for the target agent.",
            )
        }
    }
}

#[async_trait]
impl AgentTool for MailTool {
    /// Advertises Codex v2 `send_message` or `followup_task`.
    fn definition(&self) -> ToolDefinition {
        let (name, description, message) = self.spec();
        ToolDefinition {
            name: name.into(),
            description: description.into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "Target agent: `/root`, `/root/<task_name>`, or `<task_name>`."
                    },
                    "message": { "type": "string", "description": message }
                },
                "required": ["target", "message"]
            }),
        }
    }

    /// Delivers the mail, or replies with correctable usage text.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        context.ensure_active()?;
        let (tool, _, _) = self.spec();
        let session = self
            .session
            .upgrade()
            .ok_or(SubagentError::SessionGone { tool })?;
        let outcome = match MessageArgs::try_from(&call) {
            Ok(args) => self
                .send(&session, args, context)
                .await
                .map_err(|source| SubagentError::Kernel { tool, source })?,
            Err(text) => Err(text),
        };
        Ok(reply(call.tool_call_id, outcome, None))
    }
}

/// Shortest, default, and longest `wait_agent` timeouts (Codex defaults).
const MIN_WAIT_MS: u64 = 10_000;
const DEFAULT_WAIT_MS: u64 = 30_000;
const MAX_WAIT_MS: u64 = 3_600_000;

/// Waits for mailbox activity, new user input, or a timeout.
pub(super) struct WaitAgentTool {
    pub(super) session: Weak<Session>,
}

impl WaitAgentTool {
    /// Resolves the summary text of one wait; content arrives as mail afterward.
    async fn wait(
        session: &Session,
        requested: Option<u64>,
        context: &ToolExecutionContext,
    ) -> Result<String, ToolError> {
        let timeout =
            requested.map_or(DEFAULT_WAIT_MS, |ms| ms.max(MIN_WAIT_MS));
        let execution = &session.execution;
        let mut activity = execution.mailbox();
        let start = *activity.borrow_and_update();
        let pending =
            execution
                .pending_mail()
                .map_err(|error| ToolError::Execution {
                    tool: protocol::WAIT_AGENT_TOOL.into(),
                    message: error.to_string(),
                })?;
        let outcome = if pending {
            "Wait completed."
        } else {
            let deadline =
                tokio::time::sleep(std::time::Duration::from_millis(timeout));
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    biased;
                    () = context.cancellation.cancelled() => return Err(ToolError::Cancelled),
                    changed = activity.changed() => {
                        if changed.is_err() {
                            break "Wait timed out.";
                        }
                        let now = *activity.borrow_and_update();
                        if now.mail != start.mail {
                            break "Wait completed.";
                        }
                        if now.input != start.input {
                            break "Wait interrupted by new input.";
                        }
                    }
                    () = &mut deadline => break "Wait timed out.",
                }
            }
        };
        tracing::info!("finished wait_agent: {}", outcome);
        Ok(match requested {
            Some(ms) if ms < MIN_WAIT_MS => format!(
                "{outcome}\n\nRequested timeout of {ms}ms was clamped to the minimum of {MIN_WAIT_MS}ms."
            ),
            _ => outcome.to_string(),
        })
    }
}

#[async_trait]
impl AgentTool for WaitAgentTool {
    /// Advertises Codex v2 `wait_agent`.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: protocol::WAIT_AGENT_TOOL.into(),
            description: "Wait for a mailbox update from any live agent, including queued messages and final-status notifications. The wait also ends early when new user input arrives. Does not return the content; returns either a summary of which agents have updates, an interruption summary for new input, or a timeout summary.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "timeout_ms": {
                        "type": "integer",
                        "description": "Milliseconds to wait. Defaults to 30000; clamped to at least 10000; at most 3600000."
                    }
                }
            }),
        }
    }

    /// Waits, or replies with correctable usage text for an invalid timeout.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        context.ensure_active()?;
        let session =
            self.session.upgrade().ok_or(SubagentError::SessionGone {
                tool: protocol::WAIT_AGENT_TOOL,
            })?;
        let requested = call
            .arguments
            .get("timeout_ms")
            .and_then(serde_json::Value::as_u64);
        let outcome = match requested {
            Some(ms) if ms > MAX_WAIT_MS => {
                Err(format!("timeout_ms must be at most {MAX_WAIT_MS}"))
            }
            _ => Ok(Self::wait(&session, requested, context).await?),
        };
        Ok(reply(call.tool_call_id, outcome, None))
    }
}

/// Cancels a child's current turn and reports its previous status.
pub(super) struct InterruptAgentTool {
    pub(super) session: Weak<Session>,
    pub(super) kernel: Kernel,
}

impl InterruptAgentTool {
    /// Resolves the child, captures its status, then cancels its Run, or
    /// returns a correctable refusal.
    fn interrupt(
        &self,
        session: &Arc<Session>,
        target: &str,
        context: &ToolExecutionContext,
    ) -> Result<Result<String, String>, KernelError> {
        let tree = AgentTree::of(&self.kernel, &context.session_id, session)?;
        let (path, child) = match tree.resolve(target)? {
            Err(text) => return Ok(Err(text)),
            Ok(AgentTarget::Root) => {
                return Ok(Err("root is not a spawned agent".into()));
            }
            Ok(AgentTarget::Child { path, session_id }) => (path, session_id),
        };
        let running = tree.is_running(&child);
        let previous = tree
            .with_children(|children| {
                children
                    .get(path.name())
                    .map(|entry| entry.status.report(running))
            })?
            .transpose()?
            .unwrap_or_else(|| serde_json::json!("not_found"));
        if running {
            self.kernel.cancel_session(&child)?;
        }
        tracing::info!("interrupted {} (was {})", path, previous);
        Ok(Ok(
            serde_json::json!({ "previous_status": previous }).to_string()
        ))
    }
}

#[async_trait]
impl AgentTool for InterruptAgentTool {
    /// Advertises Codex v2 `interrupt_agent`.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: protocol::INTERRUPT_AGENT_TOOL.into(),
            description: "Interrupt an agent's current turn, if any, and return its previous status. The agent remains available for messages and follow-up tasks.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "target": {
                        "type": "string",
                        "description": "Child agent to interrupt: `/root/<task_name>` or `<task_name>`."
                    }
                },
                "required": ["target"]
            }),
        }
    }

    /// Interrupts the child, or replies with correctable usage text.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        context.ensure_active()?;
        let session =
            self.session.upgrade().ok_or(SubagentError::SessionGone {
                tool: protocol::INTERRUPT_AGENT_TOOL,
            })?;
        let outcome = match string_arg(&call, "target") {
            Some(target) => {
                self.interrupt(&session, target, context)
                    .map_err(|source| SubagentError::Kernel {
                        tool: protocol::INTERRUPT_AGENT_TOOL,
                        source,
                    })?
            }
            None => Err("interrupt_agent needs target.".into()),
        };
        Ok(reply(call.tool_call_id, outcome, None))
    }
}
