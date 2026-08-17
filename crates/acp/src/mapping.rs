use std::collections::BTreeMap;

use agent_client_protocol::schema::v2 as wire;
use protocol::{
    AgentEvent, AgentEventPayload, AgentOutcome, ContentBlock, MessageContent,
    ProductIdentity, ToolResultDetails,
};

use crate::content::AcpContentMapper;

/// Conversion failures at the runtime-to-ACP v2 boundary.
#[derive(Debug, thiserror::Error)]
pub enum AcpMappingError {
    /// A typed runtime payload could not be represented as JSON extension data.
    #[error("ACP event serialization failed: {0}")]
    Json(#[from] serde_json::Error),
}

/// Maps kernel events to native ACP v2 updates or reserved extension updates.
#[derive(Debug, Clone, Copy, Default)]
pub struct AcpEventMapper;

impl AcpEventMapper {
    /// Converts one correlated kernel event into one or more ordered ACP updates.
    pub fn map(
        event: AgentEvent,
    ) -> Result<Vec<wire::SessionUpdate>, AcpMappingError> {
        let metadata = Self::metadata(&event)?;
        let updates = match &event.payload {
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
                        if !reasoning.is_empty() {
                            updates.push(wire::SessionUpdate::AgentThought(
                                wire::AgentThought::new(message_id)
                                    .content(reasoning)
                                    .meta(metadata.clone()),
                            ));
                        }
                        updates.extend(blocks.iter().filter_map(|block| {
                            let ContentBlock::ToolCall {
                                tool_call_id,
                                name,
                                arguments,
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
                    MessageContent::ToolResult {
                        tool_call_id,
                        blocks,
                        is_error,
                        details,
                    } => {
                        let content = Self::tool_result_content(
                            blocks,
                            details.as_ref(),
                            &metadata,
                        )?;
                        vec![wire::SessionUpdate::ToolCallUpdate(
                            wire::ToolCallUpdate::new(tool_call_id.to_string())
                                .status(if *is_error {
                                    wire::ToolCallStatus::Failed
                                } else {
                                    wire::ToolCallStatus::Completed
                                })
                                .content(content)
                                .raw_output(serde_json::to_value(message)?)
                                .meta(metadata),
                        )]
                    }
                    MessageContent::System { .. }
                    | MessageContent::BashExecution { .. }
                    | MessageContent::Extension { .. } => {
                        vec![Self::extension_update(&event)?]
                    }
                }
            }
            AgentEventPayload::ToolExecutionStart { call, .. } => {
                vec![wire::SessionUpdate::ToolCallUpdate(
                    wire::ToolCallUpdate::new(call.tool_call_id.to_string())
                        .title(call.name.clone())
                        .status(wire::ToolCallStatus::InProgress)
                        .raw_input(call.arguments.clone())
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
            AgentEventPayload::ToolExecutionEnd { result, .. } => {
                let content = Self::tool_result_content(
                    &result.blocks,
                    result.details.as_ref(),
                    &metadata,
                )?;
                let status = if result.is_error {
                    wire::ToolCallStatus::Failed
                } else {
                    wire::ToolCallStatus::Completed
                };
                vec![wire::SessionUpdate::ToolCallUpdate(
                    wire::ToolCallUpdate::new(result.tool_call_id.to_string())
                        .status(status)
                        .content(content)
                        .raw_output(serde_json::to_value(result)?)
                        .meta(metadata),
                )]
            }
            AgentEventPayload::RunStart { .. } => vec![
                Self::extension_update(&event)?,
                wire::SessionUpdate::StateUpdate(wire::StateUpdate::Running(
                    wire::RunningStateUpdate::new().meta(metadata),
                )),
            ],
            AgentEventPayload::RunEnd { .. } => {
                vec![Self::extension_update(&event)?]
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
                        let available = wire::AvailableCommand::new(
                            command.name.clone(),
                            command.description.clone(),
                        );
                        match &command.argument_hint {
                            Some(hint) => available.input(
                                wire::AvailableCommandInput::Text(
                                    wire::TextCommandInput::new(hint.clone()),
                                ),
                            ),
                            None => available,
                        }
                    })
                    .collect();
                vec![wire::SessionUpdate::AvailableCommandsUpdate(
                    wire::AvailableCommandsUpdate::new(commands).meta(metadata),
                )]
            }
            AgentEventPayload::UsageUpdated {
                usage,
                context_window,
            } => vec![wire::SessionUpdate::UsageUpdate(
                wire::UsageUpdate::new(usage.total_tokens, *context_window)
                    .meta(metadata),
            )],
            AgentEventPayload::AgentSettled { outcome, .. } => vec![
                Self::extension_update(&event)?,
                wire::SessionUpdate::StateUpdate(wire::StateUpdate::Idle(
                    wire::IdleStateUpdate::new()
                        .stop_reason(match outcome {
                            AgentOutcome::Succeeded => {
                                wire::StopReason::EndTurn
                            }
                            AgentOutcome::Cancelled => {
                                wire::StopReason::Cancelled
                            }
                            AgentOutcome::Failed { .. } => {
                                wire::StopReason::Other(
                                    ProductIdentity::ACP_ERROR_STOP_REASON
                                        .to_string(),
                                )
                            }
                        })
                        .meta(metadata),
                )),
            ],
            AgentEventPayload::CompactionStart { .. }
            | AgentEventPayload::CompactionEnd { .. }
            | AgentEventPayload::ExtensionHandlerFailed { .. }
            | AgentEventPayload::SkillDiagnostic { .. }
            | AgentEventPayload::McpElicitationRequested { .. }
            | AgentEventPayload::McpElicitationResolved { .. } => {
                vec![Self::extension_update(&event)?]
            }
            AgentEventPayload::TurnStart { .. }
            | AgentEventPayload::MessageStart { .. }
            | AgentEventPayload::RetryScheduled { .. }
            | AgentEventPayload::RetryStart { .. }
            | AgentEventPayload::RetryEnd { .. }
            | AgentEventPayload::TurnEnd { .. } => {
                // Lifecycle events without a lossless native ACP v2 shape stay
                // available through the reserved extension update.
                vec![Self::extension_update(&event)?]
            }
        };
        Ok(updates)
    }

    /// Builds namespaced ACP metadata with precision-safe Turn timing fields.
    pub fn metadata(event: &AgentEvent) -> Result<wire::Meta, AcpMappingError> {
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
                    product.insert(
                        "assistant".to_string(),
                        serde_json::to_value(metadata)?,
                    );
                }
            }
            AgentEventPayload::UsageUpdated { usage, .. } => {
                product
                    .insert("usage".to_string(), serde_json::to_value(usage)?);
            }
            AgentEventPayload::RunStart { .. }
            | AgentEventPayload::RunEnd { .. }
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
            | AgentEventPayload::CompactionStart { .. }
            | AgentEventPayload::CompactionEnd { .. }
            | AgentEventPayload::ExtensionHandlerFailed { .. }
            | AgentEventPayload::SkillDiagnostic { .. }
            | AgentEventPayload::McpElicitationRequested { .. }
            | AgentEventPayload::McpElicitationResolved { .. }
            | AgentEventPayload::AgentSettled { .. } => {}
        }
        Ok(wire::Meta::from_iter([(
            ProductIdentity::ACP_NAMESPACE.to_string(),
            serde_json::Value::Object(product),
        )]))
    }

    /// Preserves pi-only lifecycle data through ACP v2's custom update variant.
    fn extension_update(
        event: &AgentEvent,
    ) -> Result<wire::SessionUpdate, AcpMappingError> {
        let payload = serde_json::to_value(&event.payload)?;
        let fields = BTreeMap::from([
            ("payload".to_string(), payload),
            (
                "_meta".to_string(),
                serde_json::to_value(Self::metadata(event)?)?,
            ),
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
