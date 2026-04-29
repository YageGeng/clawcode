use llm::{
    completion::{
        Message,
        message::{AssistantContent, ToolCall, UserContent},
    },
    usage::Usage,
};
use serde::{Deserialize, Serialize};

use crate::context::{TurnContext, TurnContextItem};

/// One finalized turn retained in prompt-visible history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedTurn {
    pub user_text: String,
    pub transcript: Vec<Message>,
    pub usage: Usage,
    pub context_item: TurnContextItem,
}

#[derive(Debug, Clone)]
struct ActiveTurn {
    user_text: String,
    transcript: Vec<Message>,
}

/// Canonical owner of thread history and the current context baseline.
#[derive(Debug, Clone, Default)]
pub struct ContextManager {
    turns: Vec<CompletedTurn>,
    active_turn: Option<ActiveTurn>,
    reference_context_item: Option<TurnContextItem>,
}

impl ContextManager {
    /// Creates an empty history manager.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the latest durable context snapshot used as the diff baseline.
    pub fn reference_context_item(&self) -> Option<TurnContextItem> {
        self.reference_context_item.clone()
    }

    /// Overrides the durable reference snapshot, primarily for reconstruction and tests.
    pub fn set_reference_context_item(&mut self, item: Option<TurnContextItem>) {
        self.reference_context_item = item;
    }

    /// Starts an active turn and seeds it with the submitted user message.
    pub fn begin_turn(&mut self, user_text: String, user_message: Message) {
        self.active_turn = Some(ActiveTurn {
            user_text,
            transcript: vec![user_message],
        });
    }

    /// Appends one message into the active turn transcript.
    pub fn append_message(&mut self, message: Message) {
        if let Some(active_turn) = self.active_turn.as_mut() {
            active_turn.transcript.push(message);
        } else {
            tracing::warn!("append_message called without an active turn — message dropped");
        }
    }

    /// Finalizes the active turn and advances the durable reference snapshot.
    pub fn finalize_turn(&mut self, usage: Usage, turn_context: &TurnContext) {
        let active_turn = self
            .active_turn
            .take()
            .expect("active turn must exist before finalize_turn");
        let context_item = turn_context.to_turn_context_item();
        self.turns.push(CompletedTurn {
            user_text: active_turn.user_text,
            transcript: active_turn.transcript,
            usage,
            context_item: context_item.clone(),
        });
        self.reference_context_item = Some(context_item);
    }

    /// Discards the active turn after a failed execution.
    pub fn discard_turn(&mut self) {
        self.active_turn = None;
    }

    /// Appends one already-finalized turn into history and advances the baseline.
    pub fn append_turn(&mut self, turn: CompletedTurn) {
        self.reference_context_item = Some(turn.context_item.clone());
        self.turns.push(turn);
    }

    /// Returns every completed turn retained in history.
    pub fn completed_turns(&self) -> &[CompletedTurn] {
        &self.turns
    }

    /// Builds the full initial context bundle when no baseline exists yet.
    pub fn initial_context_items(&self, turn_context: &TurnContext) -> Vec<Message> {
        let mut items = Vec::new();

        if let Some(system_prompt) = turn_context.system_prompt.as_deref() {
            items.push(Message::assistant(format!(
                "<initial_context><field>system_prompt</field><value>{system_prompt}</value></initial_context>"
            )));
        }
        if let Some(cwd) = turn_context.cwd.as_deref() {
            items.push(Message::assistant(format!(
                "<initial_context><field>cwd</field><value>{cwd}</value></initial_context>"
            )));
        }
        if let Some(current_date) = turn_context.current_date.as_deref() {
            items.push(Message::assistant(format!(
                "<initial_context><field>current_date</field><value>{current_date}</value></initial_context>"
            )));
        }
        if let Some(timezone) = turn_context.timezone.as_deref() {
            items.push(Message::assistant(format!(
                "<initial_context><field>timezone</field><value>{timezone}</value></initial_context>"
            )));
        }

        items
    }

    /// Builds settings-only update items against the current reference baseline.
    pub fn settings_diff_items(&self, turn_context: &TurnContext) -> Vec<Message> {
        let next = turn_context.to_turn_context_item();
        self.reference_context_item
            .as_ref()
            .map(|baseline| baseline.diff_messages(&next))
            .unwrap_or_default()
    }

    /// Rebuilds context history and the latest baseline from completed turns.
    pub fn reconstruct_from_completed_turns(turns: &[CompletedTurn]) -> Self {
        let mut history = Self::new();
        history.turns = turns.to_vec();
        history.reference_context_item = turns.last().map(|turn| turn.context_item.clone());
        history
    }

    /// Returns the most recent prompt-visible messages, including any active turn.
    pub fn prompt_messages(&self, limit: usize) -> Vec<Message> {
        if limit == 0 {
            return Vec::new();
        }

        let mut messages = self
            .turns
            .iter()
            .flat_map(|turn| turn.transcript.clone())
            .collect::<Vec<_>>();
        if let Some(active_turn) = self.active_turn.as_ref() {
            messages.extend(active_turn.transcript.clone());
        }

        if messages.len() <= limit {
            return messages;
        }

        let split_index = messages.len() - limit;
        let (dropped, kept) = messages.split_at(split_index);

        let mut prompt_messages = dropped
            .iter()
            .filter(|message| {
                Self::is_tool_turn_message(message)
                    || Self::is_tool_result_for_calls(message, &messages)
            })
            .cloned()
            .collect::<Vec<_>>();
        prompt_messages.extend_from_slice(kept);

        prompt_messages
    }

    /// Collects all call IDs from one assistant tool-call message.
    fn tool_call_ids_from_message(message: &Message) -> Vec<String> {
        let Message::Assistant { content, .. } = message else {
            return Vec::new();
        };

        content
            .iter()
            .filter_map(|item| match item {
                AssistantContent::ToolCall(ToolCall {
                    id: _,
                    call_id: Some(call_id),
                    ..
                }) => Some(call_id),
                AssistantContent::ToolCall(ToolCall {
                    id, call_id: None, ..
                }) => Some(id),
                _ => None,
            })
            .cloned()
            .collect()
    }

    /// Returns true when this user message contains at least one matching tool result.
    fn is_tool_result_for_calls(message: &Message, messages: &[Message]) -> bool {
        let Message::User { content, .. } = message else {
            return false;
        };

        content.iter().any(|item| match item {
            UserContent::ToolResult(tool_result) => {
                let tool_call_id = tool_result
                    .call_id
                    .as_deref()
                    .unwrap_or(tool_result.id.as_str());
                messages.iter().any(|message| {
                    Self::tool_call_ids_from_message(message)
                        .iter()
                        .any(|id| id == tool_call_id)
                })
            }
            _ => false,
        })
    }

    /// Determines whether this assistant message must be preserved for DeepSeek thinking mode.
    ///
    /// DeepSeek tool-call rounds require the call + context replay even when older
    /// turns are dropped by the prompt window.
    fn is_tool_turn_message(message: &Message) -> bool {
        let Message::Assistant { content, .. } = message else {
            return false;
        };

        // Preserve only assistant turns that include at least one tool call payload.
        content
            .iter()
            .any(|item| matches!(item, AssistantContent::ToolCall(_)))
    }
}

#[cfg(test)]
mod tests {
    use crate::{SessionId, TurnContext};
    use llm::{
        completion::Message, completion::message::AssistantContent, one_or_many::OneOrMany,
        usage::Usage,
    };

    use super::ContextManager;

    /// Keeps a tool-call assistant message with reasoning when all earlier messages
    /// are trimmed by the prompt window size.
    #[test]
    fn prompt_messages_keeps_tool_turn_reasoning_when_old_messages_are_truncated() {
        let mut history = ContextManager::new();
        let session_id = SessionId::new();
        let thread_id = crate::ThreadId::new();
        let context = TurnContext::new(session_id, thread_id).with_timezone("Asia/Shanghai");

        history.begin_turn(
            "older user question".to_string(),
            Message::user("older user question"),
        );
        history.append_message(Message::Assistant {
            id: Some("tool-turn-message".to_string()),
            content: OneOrMany::many(vec![
                AssistantContent::reasoning("old reasoning"),
                AssistantContent::tool_call("call_1", "read_file", serde_json::json!({})),
            ])
            .expect("assistant content should be constructible"),
        });
        history.finalize_turn(Usage::new(), &context);

        history.begin_turn(
            "latest user question".to_string(),
            Message::user("latest user question"),
        );
        history.append_message(Message::assistant("latest answer"));
        history.finalize_turn(Usage::new(), &context);

        let prompt = history.prompt_messages(1);

        assert_eq!(prompt.len(), 2);
        assert!(matches!(
            prompt[0],
            Message::Assistant {
                id: Some(ref id),
                ..
            } if id == "tool-turn-message"
        ));
        assert_eq!(prompt[1], Message::assistant("latest answer"));
    }

    /// Keeps a tool-call-only assistant message when prompt messages are truncated.
    #[test]
    fn prompt_messages_keeps_tool_turn_without_reasoning_when_old_messages_are_truncated() {
        let mut history = ContextManager::new();
        let session_id = SessionId::new();
        let thread_id = crate::ThreadId::new();
        let context = TurnContext::new(session_id, thread_id).with_timezone("Asia/Shanghai");

        history.begin_turn(
            "older user question".to_string(),
            Message::user("older user question"),
        );
        history.append_message(Message::Assistant {
            id: Some("tool-turn-message".to_string()),
            content: OneOrMany::many(vec![AssistantContent::tool_call(
                "call_1",
                "read_file",
                serde_json::json!({"path": "cli.log"}),
            )])
            .expect("assistant content should be constructible"),
        });
        history.finalize_turn(Usage::new(), &context);

        history.begin_turn(
            "latest user question".to_string(),
            Message::user("latest user question"),
        );
        history.append_message(Message::assistant("latest answer"));
        history.finalize_turn(Usage::new(), &context);

        let prompt = history.prompt_messages(1);

        assert_eq!(prompt.len(), 2);
        assert!(matches!(
            prompt[0],
            Message::Assistant {
                id: Some(ref id),
                ..
            } if id == "tool-turn-message"
        ));
        assert_eq!(prompt[1], Message::assistant("latest answer"));
    }
}
