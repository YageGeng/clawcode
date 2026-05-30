//! Conversation history management for sessions.

use futures::future::BoxFuture;
use protocol::message::{AssistantContent, Message};
use provider::factory::Llm;

use crate::compaction::ContextCompactor;

/// Options used when compacting a context history snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactionOptions {
    /// Number of recent user turns to keep verbatim after compaction.
    pub retained_turns: usize,
}

/// Result of a successful manual context compaction.
#[derive(Debug, Clone, PartialEq)]
pub struct CompactionOutput {
    /// Summary text returned by the model.
    pub summary: String,
    /// Replacement live history to persist in the checkpoint.
    pub replacement_history: Vec<Message>,
    /// Count of original messages retained verbatim in replacement history.
    pub retained_message_count: usize,
}

/// Manages conversation history for a session.
///
/// Implementations range from in-memory `Vec<Message>` to persistent storage.
/// Compaction methods compute replacement history but do not apply it; callers
/// must persist any checkpoint before replacing the live history.
pub trait ContextManager: Send + Sync {
    /// Append a message to the conversation history.
    fn push(&mut self, msg: Message);

    /// Return all messages in the current history, oldest first.
    fn history(&self) -> &[Message];

    /// Estimate total token count of the stored history.
    fn token_count(&self) -> usize;

    /// Clear all history.
    fn clear(&mut self);

    /// Replace all history with the provided messages.
    fn replace(&mut self, messages: Vec<Message>);

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
                AssistantContent::ToolCall(_) | AssistantContent::Image(_) => {
                    None
                }
            })
        })
    }

    /// Returns `true` when compaction is recommended for this history.
    /// Default implementation returns `false`.
    fn should_compact(&self) -> bool {
        false
    }

    /// Compute a compaction output without mutating the current history.
    ///
    /// Callers are responsible for persisting the returned checkpoint data
    /// before applying `replacement_history` via [`ContextManager::replace`].
    /// Default implementation is a no-op.
    fn compact<'a>(
        &'a self,
        _llm: &'a dyn Llm,
        _options: CompactionOptions,
    ) -> BoxFuture<'a, Result<Option<CompactionOutput>, anyhow::Error>> {
        Box::pin(std::future::ready(Ok(None)))
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

    fn replace(&mut self, messages: Vec<Message>) {
        self.messages = messages;
    }

    fn compact<'a>(
        &'a self,
        llm: &'a dyn Llm,
        options: CompactionOptions,
    ) -> BoxFuture<'a, Result<Option<CompactionOutput>, anyhow::Error>> {
        Box::pin(async move {
            ContextCompactor::new(options.retained_turns)
                .compact_history(llm, self.history())
                .await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use protocol::one_or_many::OneOrMany;
    use provider::completion::CompletionError;
    use provider::completion::request::CompletionRequest;
    use provider::factory::{DynLlmStream, LlmCompletion};
    use provider::message::AssistantContent;
    use provider::wasm_compat::WasmBoxedFuture;

    #[derive(Debug)]
    struct SummaryLlm;

    impl Llm for SummaryLlm {
        /// Return a stable provider id for context compaction tests.
        fn provider_id(&self) -> &str {
            "test"
        }

        /// Return a stable model id for context compaction tests.
        fn model_id(&self) -> &str {
            "test-model"
        }

        /// Return a fixed summary for non-streaming compaction requests.
        fn completion(
            &self,
            _request: CompletionRequest,
        ) -> WasmBoxedFuture<'_, Result<LlmCompletion, CompletionError>>
        {
            Box::pin(async {
                Ok(LlmCompletion {
                    choice: OneOrMany::one(AssistantContent::text("summary")),
                    usage: Default::default(),
                    raw_response: serde_json::json!({}),
                    message_id: None,
                })
            })
        }

        /// Streaming is not used by context compaction tests.
        fn stream(
            &self,
            _request: CompletionRequest,
        ) -> WasmBoxedFuture<'_, Result<DynLlmStream, CompletionError>>
        {
            Box::pin(async {
                Err(CompletionError::ProviderError("stream unused".to_string()))
            })
        }
    }

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
    fn default_last_assistant_text_returns_latest_displayable_assistant_content()
     {
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

    /// Replacing an in-memory context swaps all previously stored messages.
    #[test]
    fn in_memory_context_replace_swaps_history() {
        let mut ctx = InMemoryContext::new();
        ctx.push(Message::user("old"));

        ctx.replace(vec![Message::user("summary"), Message::assistant("tail")]);

        assert_eq!(
            ctx.history(),
            &[Message::user("summary"), Message::assistant("tail")]
        );
    }

    /// InMemoryContext compaction returns replacement history without mutating live history.
    #[tokio::test]
    async fn in_memory_context_compact_returns_output_without_replacing_history()
     {
        let mut ctx = InMemoryContext::new();
        ctx.push(Message::user("old"));
        ctx.push(Message::assistant("old answer"));
        ctx.push(Message::user("tail"));
        let llm = SummaryLlm;

        let output = ctx
            .compact(&llm, CompactionOptions { retained_turns: 1 })
            .await
            .expect("compact should succeed")
            .expect("history should compact");

        assert_eq!(ctx.history().len(), 3);
        assert_eq!(output.summary, "summary");
        assert_eq!(
            output.replacement_history,
            vec![
                Message::user(
                    "Another model previously summarized the conversation. Use this summary as authoritative context for older turns:\n\nsummary"
                ),
                Message::user("tail"),
            ]
        );
    }
}
