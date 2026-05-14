//! Conversation history management for sessions.

use futures::future::BoxFuture;
use protocol::message::Message;
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
    fn history(&self) -> Vec<Message>;

    /// Estimate total token count of the stored history.
    fn token_count(&self) -> usize;

    /// Clear all history.
    fn clear(&mut self);

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

    fn history(&self) -> Vec<Message> {
        self.messages.clone()
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
        assert!(ctx.history().is_empty());

        let msg = Message::user("hello");
        ctx.push(msg.clone());
        assert_eq!(ctx.history().len(), 1);
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
}
