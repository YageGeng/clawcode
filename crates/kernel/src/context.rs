//! Conversation history management for sessions.

use futures::future::BoxFuture;
use protocol::message::{AssistantContent, Message};
use provider::factory::Llm;

/// Manages conversation history for a session.
///
/// Implementations range from in-memory `Vec<Message>` to persistent storage
/// with automatic compaction. The compaction interface is reserved for
/// future implementation.
pub trait ContextManager: Send + Sync {
    /// Append a message to the conversation history.
    fn push(&mut self, msg: Message);

    /// Return all messages in the current history, oldest first.
    fn history(&self) -> &[Message];

    /// Estimate total token count of the stored history.
    fn token_count(&self) -> usize;

    /// Clear all history.
    fn clear(&mut self);

    /// Extract the latest displayable assistant text from the conversation history.
    fn last_assistant_text(&self) -> Option<String> {
        self.history().iter().rev().find_map(|message| {
            let Message::Assistant { content, .. } = message else {
                return None;
            };
            content.iter().find_map(|content| match content {
                AssistantContent::Text(text) => Some(text.text.clone()),
                AssistantContent::Reasoning(reasoning) => {
                    let text = reasoning.display_text();
                    if text.is_empty() { None } else { Some(text) }
                }
                AssistantContent::ToolCall(_) | AssistantContent::Image(_) => None,
            })
        })
    }

    // ── Reserved for future compaction ──

    /// Returns `true` when compaction is recommended for this history.
    /// Default implementation returns `false`.
    fn should_compact(&self) -> bool {
        false
    }

    /// Compact the history by summarizing older messages.
    /// Default implementation is a no-op.
    fn compact(&mut self, _llm: &dyn Llm) -> BoxFuture<'_, Result<(), anyhow::Error>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

/// In-memory implementation of [`ContextManager`] backed by `Vec<Message>`.
pub struct InMemoryContext {
    messages: Vec<Message>,
}

impl InMemoryContext {
    /// Create a new empty context.
    #[must_use]
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    /// Create a context from replayed persisted messages.
    #[must_use]
    pub(crate) fn from_messages(messages: Vec<Message>) -> Self {
        Self { messages }
    }
}

impl Default for InMemoryContext {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextManager for InMemoryContext {
    fn push(&mut self, msg: Message) {
        self.messages.push(msg);
    }

    fn history(&self) -> &[Message] {
        &self.messages
    }

    fn token_count(&self) -> usize {
        // Rough estimate: ~4 characters per token
        self.messages
            .iter()
            .map(|m| format!("{m:?}").len() / 4)
            .sum()
    }

    fn clear(&mut self) {
        self.messages.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_memory_context_push_and_history() {
        let mut ctx = InMemoryContext::new();
        let empty_history: &[Message] = ctx.history();
        assert!(empty_history.is_empty());

        let msg = Message::user("hello");
        ctx.push(msg.clone());
        let history: &[Message] = ctx.history();
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn in_memory_context_clear() {
        let mut ctx = InMemoryContext::new();
        ctx.push(Message::user("hello"));
        ctx.clear();
        assert!(ctx.history().is_empty());
    }

    #[test]
    fn in_memory_context_token_count_is_nonzero_after_push() {
        let mut ctx = InMemoryContext::new();
        ctx.push(Message::user("hello world"));
        assert!(ctx.token_count() > 0);
    }

    #[test]
    fn default_should_compact_returns_false() {
        let ctx = InMemoryContext::new();
        assert!(!ctx.should_compact());
    }

    #[test]
    fn default_last_assistant_text_returns_latest_displayable_assistant_content() {
        let mut ctx = InMemoryContext::new();
        ctx.push(Message::assistant("older answer"));
        ctx.push(Message::user("ignored user message"));
        ctx.push(Message::from(AssistantContent::reasoning(
            "latest reasoning",
        )));

        assert_eq!(
            ctx.last_assistant_text(),
            Some("latest reasoning".to_string())
        );
    }
}
