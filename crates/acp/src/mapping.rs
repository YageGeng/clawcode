use std::collections::BTreeMap;

use agent_client_protocol::schema::v2 as wire;
use protocol::{
    AgentEvent, AgentEventPayload, AgentOutcome, CompactionOutcome,
    ContentBlock, MessageContent, ModelUsage, ProductIdentity, RunId,
    ToolResultDetails,
};

use crate::config::ConfigOptionsOf;
use crate::content::AcpContentMapper;
use crate::terminal::{
    ShellStatusOf, TerminalExit, TerminalSnapshot, TerminalText,
};
use crate::tool_call::{ToolKindOf, ToolLocationsOf, ToolStatusOf};

/// Conversion failures at the runtime-to-ACP v2 boundary.
#[derive(Debug, thiserror::Error)]
pub enum AcpMappingError {
    /// A typed runtime payload could not be represented as JSON extension data.
    #[error("ACP event serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    /// A serialized internal event lost the accounting slot required for safe projection.
    #[error("ACP event is missing its usage projection slot")]
    UsageProjection,
}

/// Maps kernel events to native ACP v2 updates or reserved extension updates.
pub struct AcpEventMapper<'a> {
    event: &'a AgentEvent,
    metadata: wire::Meta,
}

impl<'a> AcpEventMapper<'a> {
    /// Converts one correlated kernel event into one or more ordered ACP updates.
    pub fn map(
        event: AgentEvent,
    ) -> Result<Vec<wire::SessionUpdate>, AcpMappingError> {
        AcpEventMapper::new(&event)?.updates()
    }

    /// Projects all updates while borrowing one event and its correlated metadata.
    pub fn updates(&self) -> Result<Vec<wire::SessionUpdate>, AcpMappingError> {
        let event = self.event;
        let metadata = self.metadata.clone();
        let updates = match &event.payload {
            AgentEventPayload::QueueChanged { .. } => {
                vec![self.extension_update()?]
            }
            AgentEventPayload::MessageTextDelta { message_id, delta } => {
                vec![wire::SessionUpdate::AgentMessageChunk(
                    wire::ContentChunk::new(
                        wire::ContentBlock::Text(
                            wire::TextContent::new(delta)
                                .meta(metadata.clone()),
                        ),
                        message_id.to_string(),
                    )
                    .meta(metadata),
                )]
            }
            AgentEventPayload::MessageReasoningDelta { message_id, delta } => {
                vec![wire::SessionUpdate::AgentThoughtChunk(
                    wire::ContentChunk::new(
                        wire::ContentBlock::Text(
                            wire::TextContent::new(delta)
                                .meta(metadata.clone()),
                        ),
                        message_id.to_string(),
                    )
                    .meta(metadata),
                )]
            }
            AgentEventPayload::MessageEnd { message } => {
                // Typed message variants make invalid role/content combinations
                // unrepresentable before the ACP boundary.
                match &message.content {
                    MessageContent::Assistant { blocks, .. } => {
                        let message_id =
                            message.identity.message_id.to_string();
                        let mut updates =
                            vec![wire::SessionUpdate::AgentMessage(
                                wire::AgentMessage::new(message_id.clone())
                                    .content(Self::message_blocks(
                                        blocks, &metadata,
                                    )?)
                                    .meta(metadata.clone()),
                            )];
                        let reasoning = blocks
                            .iter()
                            .filter_map(|block| match block {
                                ContentBlock::Reasoning { text } => {
                                    Some(wire::ContentBlock::Text(
                                        wire::TextContent::new(text)
                                            .meta(metadata.clone()),
                                    ))
                                }
                                ContentBlock::Text { .. }
                                | ContentBlock::Image { .. }
                                | ContentBlock::Audio { .. }
                                | ContentBlock::EmbeddedResource { .. }
                                | ContentBlock::ResourceLink { .. }
                                | ContentBlock::Structured { .. }
                                | ContentBlock::ToolCall { .. } => None,
                            })
                            .collect::<Vec<_>>();
                        // A full thought snapshot must also clear previously streamed reasoning.
                        updates.push(wire::SessionUpdate::AgentThought(
                            wire::AgentThought::new(message_id)
                                .content(reasoning)
                                .meta(metadata.clone()),
                        ));
                        updates.extend(blocks.iter().filter_map(|block| {
                            let ContentBlock::ToolCall {
                                tool_call_id,
                                name,
                                arguments,
                                ..
                            } = block
                            else {
                                return None;
                            };
                            Some(wire::SessionUpdate::ToolCallUpdate(
                                wire::ToolCallUpdate::new(
                                    tool_call_id.to_string(),
                                )
                                .title(name.clone())
                                .status(wire::ToolCallStatus::InProgress)
                                .raw_input(arguments.clone())
                                .meta(metadata.clone()),
                            ))
                        }));
                        updates
                    }
                    MessageContent::User { blocks } => {
                        vec![wire::SessionUpdate::UserMessage(
                            wire::UserMessage::new(
                                message.identity.message_id.to_string(),
                            )
                            .content(Self::message_blocks(blocks, &metadata)?)
                            .meta(metadata),
                        )]
                    }
                    MessageContent::ExpandedUser { expansion } => {
                        vec![wire::SessionUpdate::UserMessage(
                            wire::UserMessage::new(
                                message.identity.message_id.to_string(),
                            )
                            .content(vec![wire::ContentBlock::Text(
                                wire::TextContent::new(
                                    expansion.invocation.original.clone(),
                                )
                                .meta(metadata.clone()),
                            )])
                            .meta(metadata),
                        )]
                    }
                    MessageContent::ToolResult {
                        tool_call_id,
                        blocks,
                        is_error,
                        details,
                        cancelled,
                        ..
                    } => {
                        let status = wire::ToolCallStatus::from(ToolStatusOf {
                            cancelled: *cancelled,
                            is_error: *is_error,
                        });
                        if let Some(bash @ ToolResultDetails::Bash { .. }) =
                            details
                        {
                            // Replay closes the terminal with its saved output.
                            Self::bash_tool_end(
                                tool_call_id.as_str(),
                                blocks,
                                bash,
                                status,
                                serde_json::to_value(message)?,
                                metadata,
                            )
                        } else {
                            let content = Self::tool_result_content(
                                blocks,
                                details.as_ref(),
                                &metadata,
                            )?;
                            vec![wire::SessionUpdate::ToolCallUpdate(
                                wire::ToolCallUpdate::new(
                                    tool_call_id.to_string(),
                                )
                                .status(status)
                                .content(content)
                                .raw_output(serde_json::to_value(message)?)
                                .meta(metadata),
                            )]
                        }
                    }
                    MessageContent::BashExecution { bash } => {
                        let message_id = &message.identity.message_id;
                        let terminal =
                            wire::TerminalUpdate::from(TerminalSnapshot {
                                terminal_id: message_id.as_str(),
                                output: &bash.result.output,
                            })
                            .command(bash.command.clone())
                            .exit_status(wire::TerminalExitStatus::from(
                                TerminalExit {
                                    exit_code: bash.result.exit_code,
                                    signal: bash.result.signal.as_deref(),
                                },
                            ))
                            .meta(metadata.clone());
                        // Older messages have no saved directory; it is never guessed.
                        let terminal = match &bash.cwd {
                            Some(cwd) => terminal
                                .cwd(wire::AbsolutePath::new(cwd.clone())),
                            None => terminal,
                        };
                        vec![
                            // Replay recreates the accepted message under the same
                            // id; live, this replaces the start-time copy.
                            Self::user_bash_message(
                                message_id,
                                &bash.command,
                                bash.exclude_from_context,
                                metadata.clone(),
                            ),
                            wire::SessionUpdate::ToolCallUpdate(
                                Self::user_bash_tool(
                                    message_id,
                                    serde_json::json!({
                                        "excludeFromContext": bash.exclude_from_context,
                                        "disposition": bash.result.disposition,
                                    }),
                                    &metadata,
                                )
                                .title(bash.command.clone())
                                .status(wire::ToolCallStatus::from(
                                    ShellStatusOf(&bash.result),
                                )),
                            ),
                            wire::SessionUpdate::TerminalUpdate(terminal),
                        ]
                    }
                    MessageContent::System { .. }
                    | MessageContent::Extension { .. }
                    | MessageContent::CompactionSummary { .. } => {
                        vec![self.extension_update()?]
                    }
                    MessageContent::SlashCommand { message: command } => {
                        match command {
                            protocol::SlashCommandMessage::Invocation {
                                invocation,
                                ..
                            } => vec![wire::SessionUpdate::UserMessage(
                                wire::UserMessage::new(
                                    message.identity.message_id.to_string(),
                                )
                                .content(vec![wire::ContentBlock::Text(
                                    wire::TextContent::new(
                                        invocation.original.clone(),
                                    )
                                    .meta(metadata.clone()),
                                )])
                                .meta(metadata),
                            )],
                            protocol::SlashCommandMessage::Output(output) => {
                                vec![wire::SessionUpdate::AgentMessage(
                                    wire::AgentMessage::new(
                                        message.identity.message_id.to_string(),
                                    )
                                    .content(Self::message_blocks(
                                        &output.blocks,
                                        &metadata,
                                    )?)
                                    .meta(metadata),
                                )]
                            }
                        }
                    }
                }
            }
            AgentEventPayload::ToolExecutionStart {
                call,
                cwd,
                locations,
                ..
            } => {
                let update =
                    wire::ToolCallUpdate::new(call.tool_call_id.to_string())
                        .title(call.name.clone())
                        .name(call.name.clone())
                        .kind(wire::ToolKind::from(ToolKindOf(call)))
                        .status(wire::ToolCallStatus::InProgress)
                        .raw_input(call.arguments.clone())
                        .meta(metadata.clone());
                let locations = Vec::from(ToolLocationsOf(locations));
                let update = if locations.is_empty() {
                    update
                } else {
                    update.locations(locations)
                };
                if call.name == "bash" {
                    // The terminal id is the tool call id; no size, it is not a PTY.
                    let terminal = wire::TerminalUpdate::new(
                        call.tool_call_id.to_string(),
                    )
                    .cwd(wire::AbsolutePath::new(cwd.clone()))
                    .meta(metadata);
                    let terminal = match call
                        .arguments
                        .get("command")
                        .and_then(serde_json::Value::as_str)
                    {
                        Some(command) => terminal.command(command.to_string()),
                        None => terminal,
                    };
                    vec![
                        wire::SessionUpdate::ToolCallUpdate(update.content(
                            vec![Self::terminal_content(
                                call.tool_call_id.as_str(),
                            )],
                        )),
                        wire::SessionUpdate::TerminalUpdate(terminal),
                    ]
                } else {
                    vec![wire::SessionUpdate::ToolCallUpdate(update)]
                }
            }
            AgentEventPayload::ToolExecutionUpdate { result, .. }
                if matches!(
                    result.details,
                    Some(ToolResultDetails::Bash { .. })
                ) =>
            {
                let output = String::from(TerminalText(&result.blocks));
                vec![wire::SessionUpdate::TerminalUpdate(
                    wire::TerminalUpdate::from(TerminalSnapshot {
                        terminal_id: result.tool_call_id.as_str(),
                        output: &output,
                    })
                    .meta(metadata),
                )]
            }
            AgentEventPayload::ToolExecutionUpdate { result, .. } => {
                vec![wire::SessionUpdate::ToolCallUpdate(
                    wire::ToolCallUpdate::new(result.tool_call_id.to_string())
                        .status(wire::ToolCallStatus::InProgress)
                        .content(Self::tool_result_content(
                            &result.blocks,
                            result.details.as_ref(),
                            &metadata,
                        )?)
                        .raw_output(serde_json::to_value(result)?)
                        .meta(metadata),
                )]
            }
            AgentEventPayload::ToolExecutionEnd { result, .. }
                if matches!(
                    result.details,
                    Some(ToolResultDetails::Bash { .. })
                ) =>
            {
                let status = wire::ToolCallStatus::from(ToolStatusOf {
                    cancelled: result.cancelled,
                    is_error: result.is_error,
                });
                match &result.details {
                    Some(bash) => Self::bash_tool_end(
                        result.tool_call_id.as_str(),
                        &result.blocks,
                        bash,
                        status,
                        serde_json::to_value(result)?,
                        metadata,
                    ),
                    None => Vec::new(),
                }
            }
            AgentEventPayload::ToolExecutionEnd { result, .. } => {
                let content = Self::tool_result_content(
                    &result.blocks,
                    result.details.as_ref(),
                    &metadata,
                )?;
                let status = wire::ToolCallStatus::from(ToolStatusOf {
                    cancelled: result.cancelled,
                    is_error: result.is_error,
                });
                vec![wire::SessionUpdate::ToolCallUpdate(
                    wire::ToolCallUpdate::new(result.tool_call_id.to_string())
                        .status(status)
                        .content(content)
                        .raw_output(serde_json::to_value(result)?)
                        .meta(metadata),
                )]
            }
            AgentEventPayload::RunStart { .. } => vec![
                self.extension_update()?,
                wire::SessionUpdate::StateUpdate(wire::StateUpdate::Running(
                    wire::RunningStateUpdate::new().meta(metadata),
                )),
            ],
            AgentEventPayload::RunEnd { .. } => {
                vec![self.extension_update()?]
            }
            AgentEventPayload::SlashCommandStart { .. } => vec![
                self.extension_update()?,
                wire::SessionUpdate::StateUpdate(wire::StateUpdate::Running(
                    wire::RunningStateUpdate::new().meta(metadata),
                )),
            ],
            AgentEventPayload::SlashCommandEnd {
                status, message, ..
            } => {
                let outcome = match status {
                    protocol::SlashCommandStatus::Succeeded => {
                        AgentOutcome::Succeeded
                    }
                    protocol::SlashCommandStatus::Cancelled => {
                        AgentOutcome::Cancelled
                    }
                    protocol::SlashCommandStatus::Failed => {
                        AgentOutcome::Failed {
                            message: message.clone().unwrap_or_else(|| {
                                "Slash Command failed".to_string()
                            }),
                        }
                    }
                };
                vec![
                    self.extension_update()?,
                    wire::SessionUpdate::StateUpdate(wire::StateUpdate::Idle(
                        wire::IdleStateUpdate::new()
                            .stop_reason(wire::StopReason::from(StopReasonOf(
                                &outcome,
                            )))
                            .meta(metadata),
                    )),
                ]
            }
            AgentEventPayload::SessionTitleChanged { title } => {
                vec![wire::SessionUpdate::SessionInfoUpdate(
                    wire::SessionInfoUpdate::new()
                        .title(title.clone())
                        .meta(metadata),
                )]
            }
            AgentEventPayload::AvailableCommandsChanged { commands } => {
                let commands = commands
                    .iter()
                    .map(|command| {
                        let available = wire::Command::new(
                            command.name.clone(),
                            command.description.clone(),
                        )
                        .meta(wire::Meta::from_iter([(
                            ProductIdentity::ACP_NAMESPACE.to_string(),
                            serde_json::json!({
                                ProductIdentity::ACP_SLASH_COMMAND_METADATA: {
                                    "source": command.source,
                                    "qualifiedName": command.qualified_name,
                                    "aliasKind": command.alias_kind,
                                }
                            }),
                        )]));
                        wire::AvailableCommand::Command(
                            match &command.argument_hint {
                                Some(hint) => available.input(
                                    wire::AvailableCommandInput::Text(
                                        wire::TextCommandInput::new(
                                            hint.clone(),
                                        ),
                                    ),
                                ),
                                None => available,
                            },
                        )
                    })
                    .collect();
                vec![wire::SessionUpdate::AvailableCommandsUpdate(
                    wire::AvailableCommandsUpdate::new(commands).meta(metadata),
                )]
            }
            // Context usage follows Kernel fallback; metadata still preserves reported counters.
            AgentEventPayload::UsageUpdated {
                usage,
                context_window,
            } => match usage.as_ref().context_tokens() {
                Some(used) => vec![wire::SessionUpdate::UsageUpdate(
                    wire::UsageUpdate::new(used, *context_window)
                        .meta(metadata),
                )],
                None => vec![self.extension_update()?],
            },
            AgentEventPayload::AgentSettled { outcome, usage, .. } => vec![
                self.extension_update()?,
                wire::SessionUpdate::StateUpdate(wire::StateUpdate::Idle(
                    wire::IdleStateUpdate::new()
                        .stop_reason(wire::StopReason::from(StopReasonOf(
                            outcome,
                        )))
                        .usage(UsageOf(usage).to_acp_usage())
                        .meta(metadata),
                )),
            ],
            AgentEventPayload::SettingsChanged { settings } => {
                vec![wire::SessionUpdate::ConfigOptionUpdate(
                    wire::ConfigOptionUpdate::new(Vec::from(ConfigOptionsOf(
                        settings,
                    )))
                    .meta(metadata),
                )]
            }
            AgentEventPayload::ActionRequired { .. } => {
                vec![wire::SessionUpdate::StateUpdate(
                    wire::StateUpdate::RequiresAction(
                        wire::RequiresActionStateUpdate::new().meta(metadata),
                    ),
                )]
            }
            AgentEventPayload::ActionResolved { .. } => {
                vec![wire::SessionUpdate::StateUpdate(
                    wire::StateUpdate::Running(
                        wire::RunningStateUpdate::new().meta(metadata),
                    ),
                )]
            }
            AgentEventPayload::UserBashStarted {
                message_id,
                command,
                cwd,
                exclude_from_context,
                ..
            } => vec![
                // The lifecycle event keeps this group non-empty once replay
                // drops its state update, so recovery still sees the start.
                self.extension_update()?,
                wire::SessionUpdate::StateUpdate(wire::StateUpdate::Running(
                    wire::RunningStateUpdate::new().meta(metadata.clone()),
                )),
                Self::user_bash_message(
                    message_id,
                    command,
                    *exclude_from_context,
                    metadata.clone(),
                ),
                wire::SessionUpdate::ToolCallUpdate(
                    Self::user_bash_tool(
                        message_id,
                        serde_json::json!({
                            "excludeFromContext": exclude_from_context,
                        }),
                        &metadata,
                    )
                    .title(command.clone())
                    .status(wire::ToolCallStatus::InProgress),
                ),
                wire::SessionUpdate::TerminalUpdate(
                    wire::TerminalUpdate::new(message_id.to_string())
                        .command(command.clone())
                        .cwd(wire::AbsolutePath::new(cwd.clone()))
                        .meta(metadata),
                ),
            ],
            AgentEventPayload::UserBashOutput {
                message_id, output, ..
            } => vec![wire::SessionUpdate::TerminalUpdate(
                wire::TerminalUpdate::from(TerminalSnapshot {
                    terminal_id: message_id.as_str(),
                    output,
                })
                .meta(metadata),
            )],
            AgentEventPayload::UserBashFinished {
                outcome,
                message_id,
                result_emitted,
                ..
            } => {
                // Like AgentSettled, the lifecycle event carries the end boundary
                // through replay after the idle state is dropped.
                let mut updates = vec![self.extension_update()?];
                if !result_emitted {
                    // No result message closed the terminal and tool; close them
                    // here without inventing an exit code or signal.
                    let status = match outcome {
                        AgentOutcome::Succeeded => {
                            wire::ToolCallStatus::Completed
                        }
                        AgentOutcome::Cancelled => {
                            wire::ToolCallStatus::Cancelled
                        }
                        AgentOutcome::Failed { .. } => {
                            wire::ToolCallStatus::Failed
                        }
                    };
                    updates.push(wire::SessionUpdate::TerminalUpdate(
                        wire::TerminalUpdate::new(message_id.to_string())
                            .exit_status(wire::TerminalExitStatus::new())
                            .meta(metadata.clone()),
                    ));
                    updates.push(wire::SessionUpdate::ToolCallUpdate(
                        wire::ToolCallUpdate::new(message_id.to_string())
                            .status(status)
                            .meta(metadata.clone()),
                    ));
                }
                updates.push(wire::SessionUpdate::StateUpdate(
                    wire::StateUpdate::Idle(
                        wire::IdleStateUpdate::new()
                            .stop_reason(wire::StopReason::from(StopReasonOf(
                                outcome,
                            )))
                            .meta(metadata),
                    ),
                ));
                updates
            }
            AgentEventPayload::CompactionStart { run_id, .. } => vec![
                self.extension_update()?,
                wire::SessionUpdate::CompactionUpdate(
                    wire::CompactionUpdate::new(
                        CompactionIdOf(run_id).to_string(),
                        wire::CompactionStatus::InProgress,
                    )
                    .meta(metadata),
                ),
            ],
            AgentEventPayload::CompactionEnd {
                run_id, outcome, ..
            } => {
                // A reported compaction must always reach a terminal status.
                let id = CompactionIdOf(run_id).to_string();
                let update = match outcome {
                    CompactionOutcome::Completed { result } => {
                        wire::CompactionUpdate::new(
                            id,
                            wire::CompactionStatus::Completed,
                        )
                        .summary(vec![
                            wire::ContentBlock::Text(wire::TextContent::new(
                                result.summary.clone(),
                            )),
                        ])
                    }
                    CompactionOutcome::Failed { message } => {
                        wire::CompactionUpdate::new(
                            id,
                            wire::CompactionStatus::Failed,
                        )
                        .error(message.clone())
                    }
                    CompactionOutcome::Cancelled => {
                        wire::CompactionUpdate::new(
                            id,
                            wire::CompactionStatus::Cancelled,
                        )
                    }
                };
                vec![
                    self.extension_update()?,
                    wire::SessionUpdate::CompactionUpdate(
                        update.meta(metadata),
                    ),
                ]
            }
            AgentEventPayload::ExtensionHandlerFailed {
                extension_id,
                point,
                message,
            } => vec![
                self.extension_update()?,
                Self::warning_notice(
                    "Extension handler failed",
                    format!("{extension_id} ({point}): {message}"),
                    metadata,
                ),
            ],
            AgentEventPayload::SkillDiagnostic { diagnostic } => vec![
                self.extension_update()?,
                Self::warning_notice(
                    "Skill expansion failed",
                    diagnostic.message.clone(),
                    metadata,
                ),
            ],
            AgentEventPayload::McpElicitationRequested { .. }
            | AgentEventPayload::McpElicitationResolved { .. } => {
                vec![self.extension_update()?]
            }
            AgentEventPayload::TurnStart { .. }
            | AgentEventPayload::MessageStart { .. }
            | AgentEventPayload::RetryScheduled { .. }
            | AgentEventPayload::RetryStart { .. }
            | AgentEventPayload::RetryEnd { .. }
            | AgentEventPayload::TurnEnd { .. } => {
                // Lifecycle events without a lossless native ACP v2 shape stay
                // available through the reserved extension update.
                vec![self.extension_update()?]
            }
        };
        Ok(updates)
    }

    /// Captures precision-safe metadata once for every projection of this borrowed event.
    pub fn new(event: &'a AgentEvent) -> Result<Self, AcpMappingError> {
        let mut product = serde_json::Map::from_iter([
            (
                "turnId".to_string(),
                serde_json::to_value(&event.metadata.turn_id)?,
            ),
            (
                "timestampMs".to_string(),
                serde_json::to_value(event.metadata.timestamp_ms)?,
            ),
            (
                "sequence".to_string(),
                serde_json::to_value(event.metadata.sequence)?,
            ),
        ]);
        match &event.payload {
            AgentEventPayload::MessageEnd { message } => {
                product.insert(
                    "messageTiming".to_string(),
                    serde_json::to_value(message.timing)?,
                );
                if let MessageContent::Assistant { metadata, .. } =
                    &message.content
                {
                    let mut assistant = serde_json::to_value(metadata)?;
                    *assistant
                        .get_mut("usage")
                        .ok_or(AcpMappingError::UsageProjection)? =
                        UsageOf(&metadata.usage).metadata();
                    product.insert("assistant".to_string(), assistant);
                }
                match &message.content {
                    MessageContent::ExpandedUser { expansion } => {
                        product.insert(
                            ProductIdentity::ACP_SLASH_COMMAND_METADATA
                                .to_string(),
                            serde_json::json!({
                                "name": expansion.invocation.name,
                                "source": expansion.source,
                                "messageKind": "expansion",
                            }),
                        );
                    }
                    MessageContent::SlashCommand { message } => {
                        let slash_command = match message {
                            protocol::SlashCommandMessage::Invocation {
                                invocation,
                                source,
                            } => serde_json::json!({
                                "name": invocation.name,
                                "source": source,
                                "messageKind": "invocation",
                            }),
                            protocol::SlashCommandMessage::Output(output) => {
                                serde_json::json!({
                                    "name": output.command,
                                    "source": output.source,
                                    "status": output.status,
                                    "messageKind": "output",
                                })
                            }
                        };
                        product.insert(
                            ProductIdentity::ACP_SLASH_COMMAND_METADATA
                                .to_string(),
                            slash_command,
                        );
                    }
                    MessageContent::BashExecution { .. } => {
                        product.insert(
                            ProductIdentity::ACP_ORIGIN_METADATA.to_string(),
                            ProductIdentity::ACP_USER_BASH_ORIGIN.into(),
                        );
                    }
                    MessageContent::System { .. }
                    | MessageContent::User { .. }
                    | MessageContent::Assistant { .. }
                    | MessageContent::ToolResult { .. }
                    | MessageContent::Extension { .. }
                    | MessageContent::CompactionSummary { .. } => {}
                }
            }
            AgentEventPayload::UsageUpdated {
                usage,
                context_window,
            } => {
                product.insert("usage".to_string(), UsageOf(usage).metadata());
                product.insert(
                    "contextWindow".to_string(),
                    context_window.to_string().into(),
                );
            }
            AgentEventPayload::AgentSettled { usage, .. } => {
                product.insert("usage".to_string(), UsageOf(usage).metadata());
            }
            AgentEventPayload::QueueChanged { .. }
            | AgentEventPayload::RunStart { .. }
            | AgentEventPayload::RunEnd { .. }
            | AgentEventPayload::SlashCommandStart { .. }
            | AgentEventPayload::TurnStart { .. }
            | AgentEventPayload::TurnEnd { .. }
            | AgentEventPayload::MessageStart { .. }
            | AgentEventPayload::MessageTextDelta { .. }
            | AgentEventPayload::MessageReasoningDelta { .. }
            | AgentEventPayload::ToolExecutionStart { .. }
            | AgentEventPayload::ToolExecutionUpdate { .. }
            | AgentEventPayload::ToolExecutionEnd { .. }
            | AgentEventPayload::SessionTitleChanged { .. }
            | AgentEventPayload::AvailableCommandsChanged { .. }
            | AgentEventPayload::RetryScheduled { .. }
            | AgentEventPayload::RetryStart { .. }
            | AgentEventPayload::RetryEnd { .. }
            | AgentEventPayload::SettingsChanged { .. }
            | AgentEventPayload::ExtensionHandlerFailed { .. }
            | AgentEventPayload::SkillDiagnostic { .. }
            | AgentEventPayload::McpElicitationRequested { .. }
            | AgentEventPayload::McpElicitationResolved { .. }
            | AgentEventPayload::UserBashFinished { .. }
            | AgentEventPayload::UserBashOutput { .. }
            | AgentEventPayload::ActionRequired { .. }
            | AgentEventPayload::ActionResolved { .. } => {}
            AgentEventPayload::CompactionStart { reason, .. } => {
                product.insert(
                    ProductIdentity::ACP_COMPACTION_METADATA.to_string(),
                    serde_json::json!({ "reason": reason }),
                );
            }
            AgentEventPayload::CompactionEnd {
                reason, outcome, ..
            } => {
                // Only a completed compaction has the persisted card result; its
                // summary already travels in the standard field, so it is left out.
                let result = match outcome {
                    CompactionOutcome::Completed { result } => {
                        let mut value = serde_json::to_value(result)?;
                        if let serde_json::Value::Object(fields) = &mut value {
                            fields.remove("summary");
                            if let Some(usage) = &result.usage {
                                fields.insert(
                                    "usage".into(),
                                    UsageOf(usage).metadata(),
                                );
                            }
                        }
                        Some(value)
                    }
                    CompactionOutcome::Failed { .. }
                    | CompactionOutcome::Cancelled => None,
                };
                product.insert(
                    ProductIdentity::ACP_COMPACTION_METADATA.to_string(),
                    serde_json::json!({ "reason": reason, "result": result }),
                );
            }
            AgentEventPayload::UserBashStarted { .. } => {
                product.insert(
                    ProductIdentity::ACP_ORIGIN_METADATA.to_string(),
                    ProductIdentity::ACP_USER_BASH_ORIGIN.into(),
                );
            }
            AgentEventPayload::SlashCommandEnd { status, .. } => {
                product.insert(
                    ProductIdentity::ACP_SLASH_COMMAND_METADATA.to_string(),
                    serde_json::json!({ "status": status }),
                );
            }
        }
        Ok(Self {
            event,
            metadata: wire::Meta::from_iter([(
                ProductIdentity::ACP_NAMESPACE.to_string(),
                serde_json::Value::Object(product),
            )]),
        })
    }

    /// Clones only the transport metadata required by a separate ACP update owner.
    pub fn metadata(&self) -> wire::Meta {
        self.metadata.clone()
    }

    /// Builds a live-only warning notice from an already sanitized diagnostic.
    pub(crate) fn warning_notice(
        title: &str,
        description: String,
        metadata: wire::Meta,
    ) -> wire::SessionUpdate {
        tracing::info!("produced warning notice: {}", title);
        wire::SessionUpdate::Notice(
            wire::Notice::new(wire::NoticeSeverity::Warning, title)
                .description(description)
                .meta(metadata),
        )
    }

    /// Shows a prefixed shell prompt as the user message accepted for it.
    fn user_bash_message(
        message_id: &protocol::MessageId,
        command: &str,
        exclude_from_context: bool,
        metadata: wire::Meta,
    ) -> wire::SessionUpdate {
        let prefix = if exclude_from_context { "!!" } else { "!" };
        wire::SessionUpdate::UserMessage(
            wire::UserMessage::new(message_id.to_string())
                .content(vec![wire::ContentBlock::Text(
                    wire::TextContent::new(format!("{prefix}{command}"))
                        .meta(metadata.clone()),
                )])
                .meta(metadata),
        )
    }

    /// Names the terminal a tool call displays.
    fn terminal_content(terminal_id: &str) -> wire::ToolCallContent {
        wire::ToolCallContent::Terminal(wire::Terminal::new(
            terminal_id.to_string(),
        ))
    }

    /// Builds the user-tagged execute tool a `!bash` shell renders as.
    fn user_bash_tool(
        message_id: &protocol::MessageId,
        fields: serde_json::Value,
        metadata: &wire::Meta,
    ) -> wire::ToolCallUpdate {
        let mut metadata = metadata.clone();
        if let Some(serde_json::Value::Object(product)) =
            metadata.get_mut(ProductIdentity::ACP_NAMESPACE)
        {
            // The tool is the user's own command, unlike the model's bash tool.
            product.insert(
                ProductIdentity::ACP_ORIGIN_METADATA.to_string(),
                ProductIdentity::ACP_USER_TOOL_ORIGIN.into(),
            );
            if let serde_json::Value::Object(fields) = fields {
                product.extend(fields);
            }
        }
        wire::ToolCallUpdate::new(message_id.to_string())
            .kind(wire::ToolKind::Execute)
            .name("bash".to_string())
            .content(vec![Self::terminal_content(message_id.as_str())])
            .meta(metadata)
    }

    /// Closes a bash tool's terminal with its final output and exit, then ends the tool.
    fn bash_tool_end(
        tool_call_id: &str,
        blocks: &[ContentBlock],
        details: &ToolResultDetails,
        status: wire::ToolCallStatus,
        raw_output: serde_json::Value,
        metadata: wire::Meta,
    ) -> Vec<wire::SessionUpdate> {
        let ToolResultDetails::Bash {
            full_output_path,
            exit_code,
            signal,
            ..
        } = details
        else {
            return Vec::new();
        };
        let output = String::from(TerminalText(blocks));
        let mut content = vec![Self::terminal_content(tool_call_id)];
        if let Some(path) = full_output_path {
            content.push(wire::ToolCallContent::Content(Box::new(
                wire::Content::new(wire::ContentBlock::Text(
                    wire::TextContent::new(format!("Full output: {path}")),
                ))
                .meta(metadata.clone()),
            )));
        }
        vec![
            wire::SessionUpdate::TerminalUpdate(
                wire::TerminalUpdate::from(TerminalSnapshot {
                    terminal_id: tool_call_id,
                    output: &output,
                })
                .exit_status(wire::TerminalExitStatus::from(TerminalExit {
                    exit_code: *exit_code,
                    signal: signal.as_deref(),
                }))
                .meta(metadata.clone()),
            ),
            wire::SessionUpdate::ToolCallUpdate(
                wire::ToolCallUpdate::new(tool_call_id.to_string())
                    .status(status)
                    .content(content)
                    .raw_output(raw_output)
                    .meta(metadata),
            ),
        ]
    }

    /// Preserves pi-only lifecycle data through ACP v2's custom update variant.
    fn extension_update(&self) -> Result<wire::SessionUpdate, AcpMappingError> {
        let event = self.event;
        let mut payload = serde_json::to_value(&event.payload)?;
        // Storage envelopes never become browser accounting payloads.
        let usage_projection = match &event.payload {
            AgentEventPayload::UsageUpdated { usage, .. }
            | AgentEventPayload::AgentSettled { usage, .. } => {
                Some(("/usage", usage))
            }
            AgentEventPayload::CompactionEnd {
                outcome: CompactionOutcome::Completed { result },
                ..
            } => result
                .usage
                .as_ref()
                .map(|usage| ("/outcome/result/usage", usage)),
            _ => None,
        };
        if let Some((path, usage)) = usage_projection {
            *payload
                .pointer_mut(path)
                .ok_or(AcpMappingError::UsageProjection)? =
                UsageOf(usage).metadata();
        }
        let fields = BTreeMap::from([
            ("payload".to_string(), payload),
            ("_meta".to_string(), serde_json::to_value(&self.metadata)?),
        ]);
        Ok(wire::SessionUpdate::Other(wire::OtherSessionUpdate::new(
            ProductIdentity::ACP_EVENT_UPDATE,
            fields,
        )))
    }

    /// Converts model-displayable text blocks while ACP thought/tool updates carry the rest.
    fn message_blocks(
        blocks: &[ContentBlock],
        metadata: &wire::Meta,
    ) -> Result<Vec<wire::ContentBlock>, AcpMappingError> {
        Ok(blocks
            .iter()
            .map(|block| AcpContentMapper::message(block, metadata))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect())
    }

    /// Converts model-visible tool blocks and typed edit diagnostics to native ACP content.
    fn tool_result_content(
        blocks: &[ContentBlock],
        details: Option<&ToolResultDetails>,
        metadata: &wire::Meta,
    ) -> Result<Vec<wire::ToolCallContent>, AcpMappingError> {
        let mut content = blocks
            .iter()
            .map(|block| AcpContentMapper::tool_result(block, metadata))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        if let Some(ToolResultDetails::Edit { path, patch, .. }) = details {
            // Pi stores a standard unified patch; ACP v2's native `git_patch`
            // shape additionally requires the leading file-change header.
            let acp_patch = format!("diff --git {path} {path}\n{patch}");
            content.push(wire::ToolCallContent::Diff(
                wire::Diff::patch(
                    acp_patch,
                    vec![wire::DiffChange::modify(path.clone())],
                )
                .meta(metadata.clone()),
            ));
        }
        Ok(content)
    }
}

/// Borrows a Kernel outcome for conversion into the ACP stop reason.
pub(crate) struct StopReasonOf<'a>(pub(crate) &'a AgentOutcome);

impl From<StopReasonOf<'_>> for wire::StopReason {
    /// Maps success, cancellation, and failure to the ACP idle stop reason.
    fn from(outcome: StopReasonOf<'_>) -> Self {
        match outcome.0 {
            AgentOutcome::Succeeded => Self::EndTurn,
            AgentOutcome::Cancelled => Self::Cancelled,
            AgentOutcome::Failed { message } => Self::Error(
                wire::ErrorStopReason::new()
                    .error(wire::Error::new(-32603, message.clone())),
            ),
        }
    }
}

/// Borrows a run's accumulated model usage for the ACP idle state.
pub(crate) struct UsageOf<'a>(pub(crate) &'a ModelUsage);

impl UsageOf<'_> {
    /// Standard ACP usage is absent when any required counter was not reported.
    pub(crate) fn to_acp_usage(&self) -> Option<wire::Usage> {
        let usage = self.0.as_ref();
        Some(
            wire::Usage::new(
                usage.total_tokens?,
                usage.input_tokens?,
                usage.output_tokens?,
            )
            .thought_tokens(usage.reasoning_tokens)
            .cached_read_tokens(usage.cached_input_tokens)
            .cached_write_tokens(usage.cache_creation_input_tokens),
        )
    }

    /// Preserves unknown counters and integers beyond JavaScript's safe numeric range.
    pub(crate) fn metadata(&self) -> serde_json::Value {
        let usage = self.0.as_ref();
        serde_json::Value::Object(
            [
                ("input_tokens", usage.input_tokens),
                ("output_tokens", usage.output_tokens),
                ("cache_read_tokens", usage.cached_input_tokens),
                ("cache_write_tokens", usage.cache_creation_input_tokens),
                ("reasoning_tokens", usage.reasoning_tokens),
                ("total_tokens", usage.total_tokens),
            ]
            .into_iter()
            .filter_map(|(name, value)| {
                value.map(|value| (name.to_owned(), value.to_string().into()))
            })
            .collect(),
        )
    }
}

/// Borrows a compaction operation id for its ACP compaction entity id.
pub(crate) struct CompactionIdOf<'a>(pub(crate) &'a RunId);

impl std::fmt::Display for CompactionIdOf<'_> {
    /// Derives a Session-unique id from the compaction's own operation id.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "compaction-{}", self.0)
    }
}

/// Owns a disposable tree snapshot so hidden history can be removed before serialization.
pub(crate) struct SessionTreeOf(pub(crate) protocol::SessionTreeSnapshot);

impl SessionTreeOf {
    /// Consumes only the snapshot, preserving stored history and unrelated user JSON.
    pub(crate) fn into_value(
        mut self,
    ) -> Result<serde_json::Value, AcpMappingError> {
        // Strip private payloads in place; serializing first would copy all discarded bytes.
        for entry in &mut self.0.entries {
            let usage_path = match entry.kind.as_str() {
                "message"
                    if entry
                        .payload
                        .pointer("/content/type")
                        .and_then(serde_json::Value::as_str)
                        == Some("assistant") =>
                {
                    if let Some(content) = entry
                        .payload
                        .get_mut("content")
                        .and_then(serde_json::Value::as_object_mut)
                    {
                        content.remove("model_message");
                    }
                    "/content/metadata/usage"
                }
                "compaction" => {
                    // The retained tail restores model context and duplicates private native messages.
                    if let Some(payload) = entry.payload.as_object_mut() {
                        payload.remove("retainedTail");
                    }
                    "/usage"
                }
                _ => continue,
            };
            if let Some(slot) = entry
                .payload
                .pointer_mut(usage_path)
                .filter(|slot| !slot.is_null())
            {
                let usage: ModelUsage = serde_json::from_value(slot.take())?;
                *slot = UsageOf(&usage).metadata();
            }
        }
        Ok(serde_json::to_value(self.0)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Tree inspection must not expose native replay or round token counts through JSON numbers.
    #[test]
    fn tree_projection_hides_native_without_rewriting_user_json() {
        let usage = serde_json::to_value(ModelUsage::from(
            protocol::Usage::new()
                .input_tokens(0)
                .total_tokens(9007199254740993),
        ))
        .expect("usage");
        let arbitrary = json!({"model_message":"user-owned","retainedTail":["user-owned"],"usage":{"number":123}});
        let entries=[
            ("message",json!({"content":{"type":"assistant","blocks":[{"type":"text","text":"answer"}],"model_message":{"cipher":"sensitive-native"},"metadata":{"usage":usage.clone()}}})),
            ("compaction",json!({"summary":"summary","usage":usage,"retainedTail":[{"content":{"model_message":{"cipher":"sensitive-native"}}}]})),
            ("message",json!({"content":{"type":"tool_result","details":arbitrary}})),
        ].into_iter().enumerate().map(|(index,(kind,payload))|protocol::SessionTreeEntry::builder().entry_id(protocol::EntryId::try_from(format!("entry-{index}")).expect("entry ID")).kind(kind.into()).timestamp_ms(protocol::TimestampMs::from(1)).payload(payload).build()).collect();
        let tree = protocol::SessionTreeSnapshot::builder()
            .session_id(protocol::SessionId::try_from("s").expect("session ID"))
            .lane(protocol::LaneId::try_from("main").expect("lane"))
            .entries(entries)
            .build();
        let before = serde_json::to_value(&tree).expect("source");
        // Only the independently owned response snapshot is consumed, never the stored source.
        let projected = SessionTreeOf(tree.clone())
            .into_value()
            .expect("projection");
        assert!(!projected.to_string().contains("sensitive-native"));
        assert_eq!(
            projected["entries"][0]["payload"]["content"]["metadata"]["usage"]
                ["total_tokens"],
            "9007199254740993"
        );
        assert_eq!(
            projected["entries"][1]["payload"]["usage"]["total_tokens"],
            "9007199254740993"
        );
        // Consuming usage must retain reported zero and must not fabricate absent counters.
        assert_eq!(
            projected["entries"][1]["payload"]["usage"]["input_tokens"],
            "0"
        );
        assert!(
            projected["entries"][1]["payload"]["usage"]
                .get("output_tokens")
                .is_none()
        );
        assert!(
            projected["entries"][1]["payload"]
                .get("retainedTail")
                .is_none()
        );
        assert_eq!(
            projected["entries"][2]["payload"]["content"]["details"],
            arbitrary
        );
        assert_eq!(
            serde_json::to_value(&tree).expect("unchanged source"),
            before
        );
    }
}
