//! The history a child starts from: the owner conversation a `fork_turns = all`
//! child inherits (Codex `keep_forked_rollout_item`), then the child's role note.

use protocol::{
    AgentMessage, AgentPath, ContentBlock, ExtensionId, ExtensionMessage,
    MessageContent, ProductIdentity,
};

use crate::runtime::{Kernel, KernelError};

/// Owner conversation copied into a child before its first task.
#[derive(Default)]
pub(in crate::runtime) struct ForkedHistory(
    pub(in crate::runtime) Vec<AgentMessage>,
);

impl ForkedHistory {
    /// Ends the child's starting history with its hidden role note, Codex's
    /// subagent usage hint adapted to children that cannot spawn.
    ///
    /// Like Codex, the note follows the inherited messages instead of entering
    /// the System Prompt, so the inherited prefix stays as the owner sent it
    /// and the child's own prefix is written once and never changes. It also
    /// names the nickname the user sees, so the child answers to it.
    pub(in crate::runtime) fn with_role_note(
        mut self,
        kernel: &Kernel,
        path: &AgentPath,
        nickname: &str,
    ) -> Result<Vec<AgentMessage>, KernelError> {
        let text = format!(
            "You are `{path}` (shown to the user as {nickname}), an agent spawned by `/root` in a team of agents collaborating on the user's goals. Messages before this note, if any, are `/root`'s conversation copied for context: the requests and tasks in them were given to `/root`, not to you, and their tool calls are historical records whose live processes you do not have. Your task is the `new_task` message addressed to `{path}`, and later `new_task` messages bring follow-up tasks. When you finish a task, your final answer is delivered to `/root` automatically, so never wait for your own result. You can use `followup_task` to give another spawned agent a task and trigger its turn (its answer goes to `/root`, not to you), `send_message` to message `/root` or other agents without triggering a turn, `interrupt_agent` to stop another spawned agent's current turn, `list_agents` to see the team, and `wait_agent` to wait for messages, but you cannot spawn agents."
        );
        let extension = ExtensionMessage::builder()
            .extension_id(
                ExtensionId::try_from(ProductIdentity::SLUG).map_err(
                    |error| KernelError::Protocol(error.to_string()),
                )?,
            )
            .custom_type(protocol::SUBAGENT_ROLE_TYPE.to_string())
            .blocks(vec![ContentBlock::Text { text }])
            .display(false)
            .include_in_context(true)
            .build();
        self.0.push(kernel.message_now(
            &kernel.new_turn_id()?,
            MessageContent::Extension { extension },
        )?);
        Ok(self.0)
    }
}

impl From<&[AgentMessage]> for ForkedHistory {
    /// Keeps the conversation and drops tool traffic, reasoning, and hidden notes.
    ///
    /// Summaries become plain user text: clawcode derives them from Compaction
    /// and BranchSummary entries, so copying them as messages would leave a
    /// foreign `entry_id` and look like the child's own compaction checkpoint.
    fn from(history: &[AgentMessage]) -> Self {
        let user_text = |text: String| MessageContent::User {
            blocks: vec![ContentBlock::Text { text }],
        };
        Self(
            history
                .iter()
                .filter_map(|message| {
                    let content = match &message.content {
                        MessageContent::User { .. }
                        | MessageContent::ExpandedUser { .. } => {
                            message.content.clone()
                        }
                        MessageContent::CompactionSummary { compaction } => {
                            user_text(compaction.model_text())
                        }
                        MessageContent::BranchSummary { branch } => {
                            user_text(branch.model_text())
                        }
                        MessageContent::Assistant {
                            blocks, metadata, ..
                        } if metadata.error.is_none() => {
                            let text = blocks
                                .iter()
                                .filter(|block| {
                                    matches!(block, ContentBlock::Text { .. })
                                })
                                .cloned()
                                .collect::<Vec<_>>();
                            if text.is_empty() {
                                return None;
                            }
                            // The child inherits context, never the owner's usage
                            // or provider-native replay of tool calls and reasoning.
                            let mut metadata = metadata.clone();
                            metadata.usage = protocol::ModelUsage::default();
                            MessageContent::Assistant {
                                blocks: text,
                                metadata,
                                model_message: None,
                            }
                        }
                        _ => return None,
                    };
                    Some(AgentMessage {
                        content,
                        ..message.clone()
                    })
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use protocol::{
        AgentMessage, AssistantMetadata, BranchSummaryMessage,
        CompactionSummaryMessage, ContentBlock, EntryId, ExtensionId,
        ExtensionMessage, MessageContent, MessageId, MessageIdentity,
        MessageTiming, ModelUsage, StopReason, TimestampMs, ToolCallId, TurnId,
        Usage,
    };

    use super::ForkedHistory;

    /// Wraps content in a message with fixed identity and timing.
    fn message(id: &str, content: MessageContent) -> AgentMessage {
        let at = TimestampMs::from(1);
        AgentMessage {
            identity: MessageIdentity {
                message_id: MessageId::try_from(id).expect("id"),
                turn_id: TurnId::try_from("turn").expect("turn"),
            },
            timing: MessageTiming::try_from((at, at, at)).expect("timing"),
            content,
        }
    }

    /// Builds assistant metadata with non-zero usage and an optional error.
    fn metadata(error: Option<String>) -> AssistantMetadata {
        AssistantMetadata::builder()
            .provider_id("p".into())
            .model_id("m".into())
            .stop_reason(StopReason::EndTurn)
            .usage(ModelUsage::from(
                Usage::new().input_tokens(7).output_tokens(9),
            ))
            .error(error)
            .build()
    }

    /// Summaries become user text and assistants keep only clean text.
    #[test]
    fn forked_history_rewrites_summaries_and_strips_assistant_state() {
        let compaction = CompactionSummaryMessage::builder()
            .entry_id(EntryId::try_from("c").expect("entry"))
            .summary("earlier".to_string())
            .tokens_before(10)
            .build();
        let branch = BranchSummaryMessage::builder()
            .entry_id(EntryId::try_from("b").expect("entry"))
            .from_id("f".to_string())
            .summary("elsewhere".to_string())
            .build();
        let call = || ContentBlock::ToolCall {
            tool_call_id: ToolCallId::try_from("call").expect("call"),
            name: "read".into(),
            arguments: serde_json::json!({}),
            invalid_arguments: None,
        };
        let history = vec![
            message(
                "u",
                MessageContent::User {
                    blocks: vec![ContentBlock::Text {
                        text: "hello".into(),
                    }],
                },
            ),
            message(
                "c",
                MessageContent::CompactionSummary {
                    compaction: compaction.clone(),
                },
            ),
            message(
                "b",
                MessageContent::BranchSummary {
                    branch: branch.clone(),
                },
            ),
            message(
                "a",
                MessageContent::Assistant {
                    blocks: vec![
                        ContentBlock::Text { text: "hi".into() },
                        call(),
                    ],
                    metadata: metadata(None),
                    model_message: None,
                },
            ),
            message(
                "t",
                MessageContent::Assistant {
                    blocks: vec![call()],
                    metadata: metadata(None),
                    model_message: None,
                },
            ),
            message(
                "e",
                MessageContent::Assistant {
                    blocks: vec![ContentBlock::Text {
                        text: "broken".into(),
                    }],
                    metadata: metadata(Some("failed".into())),
                    model_message: None,
                },
            ),
            message(
                "r",
                MessageContent::ToolResult {
                    tool_call_id: ToolCallId::try_from("call").expect("call"),
                    blocks: vec![ContentBlock::Text { text: "out".into() }],
                    is_error: false,
                    details: None,
                    output: None,
                    cancelled: false,
                },
            ),
            message(
                "x",
                MessageContent::Extension {
                    extension: ExtensionMessage::builder()
                        .extension_id(
                            ExtensionId::try_from("clawcode").expect("id"),
                        )
                        .custom_type("agent_message".into())
                        .blocks(Vec::new())
                        .display(true)
                        .include_in_context(true)
                        .build(),
                },
            ),
        ];
        let forked = ForkedHistory::from(history.as_slice()).0;
        let ids = forked
            .iter()
            .map(|m| m.identity.message_id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["u", "c", "b", "a"]);
        let user = |text: String| MessageContent::User {
            blocks: vec![ContentBlock::Text { text }],
        };
        assert_eq!(forked[1].content, user(compaction.model_text()));
        assert_eq!(forked[2].content, user(branch.model_text()));
        match &forked[3].content {
            MessageContent::Assistant {
                blocks,
                metadata,
                model_message,
            } => {
                assert_eq!(
                    blocks,
                    &vec![ContentBlock::Text { text: "hi".into() }]
                );
                assert_eq!(metadata.usage, ModelUsage::default());
                assert!(model_message.is_none());
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
