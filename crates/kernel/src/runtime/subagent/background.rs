use super::*;
use protocol::{MessageId, SessionId};
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

/// Reporters and answer deduplication owned by one live Session.
#[derive(typed_builder::TypedBuilder)]
pub(in crate::runtime) struct SessionBackgroundWork {
    #[builder(default)]
    pub(in crate::runtime) shutdown: CancellationToken,
    #[builder(default)]
    tasks: Mutex<JoinSet<()>>,
    #[builder(default)]
    reported: Mutex<HashMap<SessionId, HashSet<MessageId>>>,
    #[builder(default)]
    spawning: tokio::sync::Mutex<()>,
}

impl Default for SessionBackgroundWork {
    /// Creates one owner-local reporter registry and named-spawn admission lock.
    fn default() -> Self {
        Self::builder().build()
    }
}

impl SessionBackgroundWork {
    /// Tracks reporter tasks and reaps settled tasks without blocking the tool call.
    fn spawn(&self, reporter: Reporter) -> Result<(), KernelError> {
        let mut tasks = self
            .tasks
            .lock()
            .map_err(|_lock_error| KernelError::Poisoned)?;
        if self.shutdown.is_cancelled() {
            tracing::warn!(
                "refused subagent reporter for closing session {}",
                reporter.owner
            );
            return Err(KernelError::SessionClosing);
        }
        while let Some(result) = tasks.try_join_next() {
            if let Err(error) = result {
                tracing::warn!("subagent reporter task failed: {}", error);
            }
        }
        tasks.spawn(reporter.run());
        Ok(())
    }

    /// Waits outside the synchronous lock after all child work has been cancelled.
    pub(in crate::runtime) async fn join(&self) -> Result<(), KernelError> {
        let mut tasks = std::mem::take(
            &mut *self
                .tasks
                .lock()
                .map_err(|_lock_error| KernelError::Poisoned)?,
        );
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                tracing::warn!(
                    "subagent reporter task failed during shutdown: {}",
                    error
                );
            }
        }
        Ok(())
    }

    /// Claims one stable answer identity across reporters that share an answering run.
    fn mark_reported(
        &self,
        child: &SessionId,
        message: &MessageId,
    ) -> Result<bool, KernelError> {
        Ok(self
            .reported
            .lock()
            .map_err(|_lock_error| KernelError::Poisoned)?
            .entry(child.clone())
            .or_default()
            .insert(message.clone()))
    }
}

/// A validated name that is safe to interpolate in the report envelope.
struct SubagentName(String);

impl TryFrom<&str> for SubagentName {
    type Error = ();
    /// Accepts only the bounded ASCII name grammar shared by tools and UI reports.
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if (1..=64).contains(&value.len())
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            Ok(Self(value.into()))
        } else {
            Err(())
        }
    }
}

/// Delivers independent child work and returns its answer as an owner's follow-up.
#[derive(typed_builder::TypedBuilder)]
struct Reporter {
    kernel: Kernel,
    owner: SessionId,
    child: SessionId,
    name: String,
    message: Vec<ContentBlock>,
    when_busy: WhenBusy,
    background: Arc<SessionBackgroundWork>,
}

impl Reporter {
    /// Runs both delivery phases without inheriting the owner's Turn cancellation.
    async fn run(self) {
        // shortcut: reporters and deduplication live in memory; persist lane records when crash recovery is required.
        if self.background.shutdown.is_cancelled() {
            tracing::info!(
                "skipped subagent report for {}: shutdown",
                self.child
            );
            return;
        }
        tracing::info!(
            "started subagent report delivery {} -> {}",
            self.owner,
            self.child
        );
        let answer = match Box::pin(
            self.kernel.submit(
                SubmitRequest::builder()
                    .session_id(self.child.clone())
                    .content(self.message)
                    .when_busy(self.when_busy)
                    .build(),
            ),
        )
        .await
        {
            Ok(answer) => answer,
            Err(error) => {
                tracing::warn!(
                    "failed subagent delivery {} -> {}: {}",
                    self.owner,
                    self.child,
                    error
                );
                return;
            }
        };
        let report = match answer.outcome {
            AnswerOutcome::Cancelled => {
                tracing::info!(
                    "skipped subagent report for {}: cancelled",
                    self.child
                );
                return;
            }
            AnswerOutcome::Failed(message) => {
                format!("[subagent {} failed: {}]", self.name, message)
            }
            AnswerOutcome::Completed => {
                if let Some(message) = &answer.message_id {
                    match self.background.mark_reported(&self.child, message) {
                        Ok(true) => {}
                        Ok(false) => {
                            tracing::info!(
                                "skipped subagent report for {}: already reported",
                                self.child
                            );
                            return;
                        }
                        Err(error) => {
                            tracing::warn!(
                                "failed subagent report deduplication for {}: {}",
                                self.child,
                                error
                            );
                            return;
                        }
                    }
                }
                let truncated = ::tools::truncate_head(
                    &answer.text,
                    ::tools::DEFAULT_MAX_LINES,
                    ::tools::TOOL_TEXT_BYTES,
                );
                let suffix = if truncated.truncated {
                    "\n\n[subagent answer truncated; open the subagent session for the full answer]"
                } else {
                    ""
                };
                format!(
                    "[subagent {} answered, no reply needed] {}{}",
                    self.name, truncated.content, suffix
                )
            }
        };
        if self.background.shutdown.is_cancelled() {
            tracing::info!(
                "skipped subagent report for {}: shutdown",
                self.child
            );
            return;
        }
        tracing::info!(
            "subagent {} answered for owner {}",
            self.child,
            self.owner
        );
        match Box::pin(
            self.kernel.submit(
                SubmitRequest::builder()
                    .session_id(self.owner.clone())
                    .content(vec![ContentBlock::Text { text: report }])
                    .when_busy(WhenBusy::FollowUp)
                    .build(),
            ),
        )
        .await
        {
            Ok(answer)
                if answer.outcome == super::super::AnswerOutcome::Cancelled =>
            {
                tracing::info!(
                    "subagent report {} -> {} was withdrawn before delivery",
                    self.child,
                    self.owner
                )
            }
            Ok(_) => tracing::info!(
                "completed subagent report {} -> {}",
                self.child,
                self.owner
            ),
            Err(error) => tracing::warn!(
                "failed subagent report {} -> {}: {}",
                self.child,
                self.owner,
                error
            ),
        }
    }
}

/// Validated management commands; execution never reinterprets raw JSON or action strings.
enum BackgroundAction<'a> {
    Spawn {
        name: SubagentName,
        message: &'a str,
        when_busy: WhenBusy,
    },
    Send {
        name: SubagentName,
        message: &'a str,
        when_busy: WhenBusy,
    },
    Stop(SubagentName),
    Status(Option<SubagentName>),
}

impl<'a> TryFrom<&'a ToolCall> for BackgroundAction<'a> {
    type Error = String;

    /// Validates user-correctable arguments before looking up or changing any Session.
    fn try_from(call: &'a ToolCall) -> Result<Self, Self::Error> {
        let action = call
            .arguments
            .get("action")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if !["spawn", "send", "stop", "status"].contains(&action) {
            return Err(
                "Unknown action; use spawn, send, stop, or status.".into()
            );
        }
        let name = call
            .arguments
            .get("name")
            .and_then(serde_json::Value::as_str);
        if action != "status" && name.is_none_or(str::is_empty) {
            return Err(format!("{action} needs a name."));
        }
        let name =
            name.map(SubagentName::try_from).transpose().map_err(|()| {
                "Invalid subagent name: use 1-64 letters, digits, '_' or '-'."
                    .to_string()
            })?;
        let message = call
            .arguments
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        if matches!(action, "spawn" | "send") && message.trim().is_empty() {
            return Err(format!("{action} needs a message."));
        }
        let when_busy = if call
            .arguments
            .get("followUp")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            WhenBusy::FollowUp
        } else {
            WhenBusy::Steer
        };
        match (action, name) {
            ("spawn", Some(name)) => Ok(Self::Spawn {
                name,
                message,
                when_busy,
            }),
            ("send", Some(name)) => Ok(Self::Send {
                name,
                message,
                when_busy,
            }),
            ("stop", Some(name)) => Ok(Self::Stop(name)),
            ("status", name) => Ok(Self::Status(name)),
            _ => Err(format!("{action} needs a name.")),
        }
    }
}

impl BackgroundAction<'_> {
    /// Returns the optional target name without exposing unchecked input to execution.
    fn name(&self) -> Option<&str> {
        match self {
            Self::Spawn { name, .. }
            | Self::Send { name, .. }
            | Self::Stop(name)
            | Self::Status(Some(name)) => Some(&name.0),
            Self::Status(None) => None,
        }
    }

    /// Applies one typed action and starts a reporter only for accepted spawn/send commands.
    async fn execute(
        self,
        kernel: &Kernel,
        session: &Arc<Session>,
        context: &ToolExecutionContext,
    ) -> Result<(String, Option<SessionId>), KernelError> {
        let mut children = kernel
            .store_factory
            .headers(Some(&session.cwd))?
            .into_iter()
            .filter_map(|child| match child.ownership {
                Some(SessionOwnership::Background { session_id, name })
                    if session_id == context.session_id =>
                {
                    Some((name, child.id))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        children.sort_by(|a, b| a.0.cmp(&b.0));
        let child = self
            .name()
            .and_then(|name| {
                children.iter().find(|(candidate, _)| candidate == name)
            })
            .map(|(_, id)| id.clone());
        let (name, child, body, when_busy, reply) = match (self, child) {
            (Self::Status(None), _) => {
                let mut lines = Vec::new();
                for (name, child) in children {
                    let state = if kernel.session_runtime(&child)?.running {
                        "working"
                    } else {
                        "idle"
                    };
                    lines.push(format!("{name}: {state}"));
                }
                return Ok((
                    if lines.is_empty() {
                        "No subagents.".into()
                    } else {
                        lines.join("\n")
                    },
                    None,
                ));
            }
            (Self::Spawn { name, .. }, Some(child)) => {
                return Ok((
                    format!("{} already exists; use send.", name.0),
                    Some(child),
                ));
            }
            (
                Self::Send { name, .. }
                | Self::Stop(name)
                | Self::Status(Some(name)),
                None,
            ) => return Ok((format!("No subagent named {}.", name.0), None)),
            (Self::Status(Some(name)), Some(child)) => {
                let state = if kernel.session_runtime(&child)?.running {
                    "working"
                } else {
                    "idle"
                };
                return Ok((format!("{}: {state}", name.0), Some(child)));
            }
            (Self::Stop(name), Some(child)) => {
                match kernel.session(&child) {
                    Ok(runtime) => {
                        runtime.execution.cancel()?;
                        kernel.clear_pending_messages(&child)?;
                    }
                    Err(KernelError::SessionNotFound(_)) => {}
                    Err(error) => return Err(error),
                }
                tracing::info!(
                    "stopped background subagent {} for owner {}",
                    child,
                    context.session_id
                );
                return Ok((format!("Stopped {}.", name.0), Some(child)));
            }
            (
                Self::Spawn {
                    name,
                    message,
                    when_busy,
                },
                None,
            ) => {
                let title = SessionTitle::try_from(name.0.clone()).map_err(
                    |error| KernelError::Protocol(error.to_string()),
                )?;
                let child = kernel
                    .create_owned_session(
                        SessionOwnership::Background {
                            session_id: context.session_id.clone(),
                            name: name.0.clone(),
                        },
                        title,
                    )
                    .await?;
                let body = format!(
                    "You are the subagent \"{}\". Answer the main agent's requests.\n\n{message}",
                    name.0
                );
                let reply = format!("Started {}.", name.0);
                (name.0, child, body, when_busy, reply)
            }
            (
                Self::Send {
                    name,
                    message,
                    when_busy,
                },
                Some(child),
            ) => {
                match kernel.session(&child) {
                    Ok(_) => {}
                    Err(KernelError::SessionNotFound(_)) => {
                        Box::pin(kernel.resume_session(
                            child.clone(),
                            session.cwd.clone(),
                        ))
                        .await?;
                    }
                    Err(error) => return Err(error),
                }
                let reply = format!("Sent to {}.", name.0);
                (name.0, child, message.to_string(), when_busy, reply)
            }
        };
        session.background.spawn(
            Reporter::builder()
                .kernel(kernel.clone())
                .owner(context.session_id.clone())
                .child(child.clone())
                .name(name)
                .message(vec![ContentBlock::Text { text: body }])
                .when_busy(when_busy)
                .background(Arc::clone(&session.background))
                .build(),
        )?;
        Ok((reply, Some(child)))
    }
}

/// Manages named persistent child Sessions without blocking on their answers.
pub(super) struct BackgroundSubagentTool {
    pub(super) session: Weak<Session>,
    pub(super) kernel: Kernel,
}

#[async_trait]
impl AgentTool for BackgroundSubagentTool {
    /// Advertises the same spawn/send/stop/status contract as the reference extension.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition { name: "background_subagent".into(), description: "Manage persistent subagents that work in the background. Actions: spawn (name, message), send (name, message; followUp: true queues it after the current answer instead of steering), stop (name: aborts its current work), status (name, or all subagents without one). Answers are reported back to you when they arrive.".into(), parameters: serde_json::json!({"type":"object","properties":{"action":{"type":"string","enum":["spawn","send","stop","status"]},"name":{"type":"string"},"message":{"type":"string"},"followUp":{"type":"boolean"}},"required":["action"]}) }
    }

    /// Applies one management action and returns user-correctable errors as plain replies.
    async fn execute(
        &self,
        call: ToolCall,
        context: &ToolExecutionContext,
    ) -> Result<ToolResult, ToolError> {
        context.ensure_active()?;
        let session =
            self.session.upgrade().ok_or(SubagentError::SessionGone {
                tool: "background_subagent",
            })?;
        let (text, child) = match BackgroundAction::try_from(&call) {
            Ok(action) => {
                // Serialize the name lookup with creation, including a cooperative yield
                // before the header is written. Status/send/stop do not wait on startup.
                let _spawn_guard =
                    if matches!(action, BackgroundAction::Spawn { .. }) {
                        Some(session.background.spawning.lock().await)
                    } else {
                        None
                    };
                context.ensure_active()?;
                action
                    .execute(&self.kernel, &session, context)
                    .await
                    .map_err(|source| SubagentError::Kernel {
                        tool: "background_subagent",
                        source,
                    })?
            }
            Err(message) => (message, None),
        };
        let result = ToolResult::builder()
            .tool_call_id(call.tool_call_id)
            .blocks(vec![ContentBlock::Text { text }])
            .is_error(false);
        Ok(match child {
            Some(session_id) => result
                .details(ToolResultDetails::Subagent { session_id })
                .build(),
            None => result.build(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parsed actions retain placement semantics and reject invalid input before any Session mutation.
    #[test]
    fn background_actions_validate_before_execution() {
        for (arguments, expected) in [
            (
                serde_json::json!({"action":"unknown"}),
                "Unknown action; use spawn, send, stop, or status.",
            ),
            (serde_json::json!({"action":"stop"}), "stop needs a name."),
            (
                serde_json::json!({"action":"send","name":"reader","message":"  "}),
                "send needs a message.",
            ),
            (
                serde_json::json!({"action":"status","name":"bad name"}),
                "Invalid subagent name: use 1-64 letters, digits, '_' or '-'.",
            ),
        ] {
            let call = ToolCall {
                tool_call_id: protocol::ToolCallId::try_from("test")
                    .expect("id"),
                name: "background_subagent".into(),
                arguments,
            };
            assert_eq!(
                BackgroundAction::try_from(&call).err().as_deref(),
                Some(expected)
            );
        }
        let call = ToolCall {
            tool_call_id: protocol::ToolCallId::try_from("test").expect("id"),
            name: "background_subagent".into(),
            arguments: serde_json::json!({"action":"send","name":"reader","message":"next","followUp":true}),
        };
        assert!(matches!(
            BackgroundAction::try_from(&call),
            Ok(BackgroundAction::Send {
                when_busy: WhenBusy::FollowUp,
                message: "next",
                ..
            })
        ));
        let call = ToolCall {
            arguments: serde_json::json!({"action":"status"}),
            ..call
        };
        assert!(matches!(
            BackgroundAction::try_from(&call),
            Ok(BackgroundAction::Status(None))
        ));
    }
}
