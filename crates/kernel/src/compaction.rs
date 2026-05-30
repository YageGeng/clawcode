use anyhow::Context as _;
use protocol::message::{AssistantContent, Message, UserContent};
use protocol::one_or_many::OneOrMany;
use provider::completion::request::CompletionRequest;
use provider::factory::{Llm, LlmCompletion};

use crate::context::CompactionOutput;

const SUMMARY_MARKER_PREFIX: &str = "Another model previously summarized the conversation. Use this summary as authoritative context for older turns:";

const COMPACTION_SYSTEM_PROMPT: &str = r#"You are an anchored context summarization assistant for coding sessions.

Summarize only the conversation history you are given. The newest turns may be kept verbatim outside your summary, so focus on the older context that still matters for continuing the work.

If the prompt includes a <previous-summary> block, treat it as the current anchored summary. Update it with the new history by preserving still-true details, removing stale details, and merging in new facts.

Always follow the exact output structure requested by the user prompt. Keep every section, preserve exact file paths and identifiers when known, and prefer terse bullets over paragraphs.

Do not answer the conversation itself. Do not mention that you are summarizing, compacting, or merging context. Respond in the same language as the conversation."#;

const COMPACTION_USER_PROMPT: &str = r#"Create or update the anchored summary for the earlier conversation.

Output exactly these sections:

## Goal
## Constraints & Preferences
## Progress
- Done:
- In Progress:
- Blocked:
## Key Decisions
## Next Steps
## Critical Context
## Relevant Files"#;

/// Builds and executes manual context compaction requests.
#[derive(Debug, Clone)]
pub(crate) struct ContextCompactor {
    retained_turns: usize,
}

impl ContextCompactor {
    /// Create a compactor using the configured retained user turn count.
    pub(crate) fn new(retained_turns: usize) -> Self {
        Self { retained_turns }
    }

    /// Compact a history snapshot and return replacement live history.
    pub(crate) async fn compact_history(
        &self,
        llm: &dyn Llm,
        history: &[Message],
    ) -> anyhow::Result<Option<CompactionOutput>> {
        let input = self.select_input(history);
        if input.summary_messages.is_empty() {
            return Ok(None);
        }

        let summary = self.generate_summary_with_retries(llm, &input).await?;
        let marker = Self::summary_marker(&summary);
        let mut replacement_history =
            Vec::with_capacity(input.retained_tail.len() + 1);
        replacement_history.push(Message::user(marker));
        replacement_history.extend(input.retained_tail.clone());

        Ok(Some(CompactionOutput {
            summary,
            replacement_history,
            retained_message_count: input.retained_tail.len(),
        }))
    }

    /// Select older summary input and the verbatim retained tail.
    fn select_input(&self, history: &[Message]) -> CompactionInput {
        let tail_start = self.retained_tail_start(history);
        let (summary_slice, tail_slice) = history.split_at(tail_start);
        let previous_summary = summary_slice
            .iter()
            .rev()
            .find_map(Self::extract_summary_marker);
        let summary_messages = if previous_summary.is_some() {
            summary_slice
                .iter()
                .skip_while(|message| {
                    Self::extract_summary_marker(message).is_none()
                })
                .skip(1)
                .cloned()
                .collect()
        } else {
            summary_slice.to_vec()
        };

        CompactionInput {
            previous_summary,
            summary_messages,
            retained_tail: tail_slice.to_vec(),
        }
    }

    /// Return the first message index of the retained tail.
    fn retained_tail_start(&self, history: &[Message]) -> usize {
        if self.retained_turns == 0 {
            return history.len();
        }

        let mut seen_user_turns = 0usize;
        for (index, message) in history.iter().enumerate().rev() {
            if Self::is_user_turn_start(message) {
                seen_user_turns += 1;
                if seen_user_turns == self.retained_turns {
                    return index;
                }
            }
        }
        0
    }

    /// Generate a summary, retrying the initial request three additional times.
    async fn generate_summary_with_retries(
        &self,
        llm: &dyn Llm,
        input: &CompactionInput,
    ) -> anyhow::Result<String> {
        let mut last_error = None;
        for attempt in 0..4 {
            match self.generate_summary_once(llm, input).await {
                Ok(summary) if !summary.trim().is_empty() => {
                    return Ok(summary.trim().to_string());
                }
                Ok(_) => {
                    last_error = Some(anyhow::anyhow!("empty summary"));
                }
                Err(error) => {
                    last_error = Some(error);
                }
            }

            if attempt < 3 {
                Self::sleep_before_retry(attempt).await;
            }
        }

        Err(anyhow::anyhow!(
            "context compaction failed after 4 attempts: {}",
            last_error
                .map(|error| error.to_string())
                .unwrap_or_else(|| "unknown error".to_string())
        ))
    }

    /// Issue one non-streaming model request for the selected compaction input.
    async fn generate_summary_once(
        &self,
        llm: &dyn Llm,
        input: &CompactionInput,
    ) -> anyhow::Result<String> {
        let mut messages = Vec::with_capacity(input.summary_messages.len() + 2);
        messages.push(Message::system(COMPACTION_SYSTEM_PROMPT));
        messages.extend(input.summary_messages.clone());
        messages.push(Message::user(input.prompt_text()));

        let request = CompletionRequest::builder()
            .model(Some(llm.model_id().to_string()))
            .chat_history(
                OneOrMany::many(messages)
                    .context("compaction request must contain messages")?,
            )
            .build();
        let completion = llm.completion(request).await?;
        Self::completion_text(&completion)
            .context("compaction response did not contain assistant text")
    }

    /// Sleep between retry attempts using the fixed first-version backoff schedule.
    async fn sleep_before_retry(attempt: usize) {
        let millis = match attempt {
            0 => 200,
            1 => 500,
            _ => 1000,
        };
        tokio::time::sleep(std::time::Duration::from_millis(millis)).await;
    }

    /// Extract all text-like assistant content from a completion response.
    fn completion_text(completion: &LlmCompletion) -> Option<String> {
        let parts = completion
            .choice
            .iter()
            .filter_map(|content| match content {
                AssistantContent::Text(text) => Some(text.text.clone()),
                AssistantContent::Reasoning(reasoning) => {
                    let text = reasoning.display_text();
                    if text.is_empty() { None } else { Some(text) }
                }
                AssistantContent::ToolCall(_) | AssistantContent::Image(_) => {
                    None
                }
            })
            .collect::<Vec<_>>();
        (!parts.is_empty()).then(|| parts.join("\n"))
    }

    /// Return the model-visible summary marker message text.
    fn summary_marker(summary: &str) -> String {
        format!("{SUMMARY_MARKER_PREFIX}\n\n{summary}")
    }

    /// Extract a previous summary from a marker user message.
    fn extract_summary_marker(message: &Message) -> Option<String> {
        let text = Self::user_text(message)?;
        text.strip_prefix(SUMMARY_MARKER_PREFIX)
            .map(|summary| summary.trim().to_string())
    }

    /// Return whether the message starts a normal user turn.
    fn is_user_turn_start(message: &Message) -> bool {
        Self::user_text(message).is_some()
            && Self::extract_summary_marker(message).is_none()
    }

    /// Extract the first text item from a user message.
    fn user_text(message: &Message) -> Option<&str> {
        let Message::User { content } = message else {
            return None;
        };
        content.iter().find_map(|content| match content {
            UserContent::Text(text) => Some(text.text.as_str()),
            UserContent::ToolResult(_)
            | UserContent::Image(_)
            | UserContent::Document(_) => None,
        })
    }
}

/// Selected history slices for one compaction request.
#[derive(Debug, Clone)]
struct CompactionInput {
    previous_summary: Option<String>,
    summary_messages: Vec<Message>,
    retained_tail: Vec<Message>,
}

impl CompactionInput {
    /// Build the final user instruction sent after the selected history.
    fn prompt_text(&self) -> String {
        match &self.previous_summary {
            Some(summary) => format!(
                "<previous-summary>\n{summary}\n</previous-summary>\n\n{COMPACTION_USER_PROMPT}"
            ),
            None => COMPACTION_USER_PROMPT.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use protocol::message::Message;
    use protocol::one_or_many::OneOrMany;
    use provider::completion::CompletionError;
    use provider::completion::request::CompletionRequest;
    use provider::factory::{DynLlmStream, Llm, LlmCompletion};
    use provider::message::AssistantContent;
    use provider::wasm_compat::WasmBoxedFuture;

    use super::*;

    #[derive(Debug)]
    struct RetryingLlm {
        failures_before_success: AtomicUsize,
        calls: AtomicUsize,
    }

    impl RetryingLlm {
        /// Create a test LLM that fails a fixed number of requests before succeeding.
        fn new(failures_before_success: usize) -> Self {
            Self {
                failures_before_success: AtomicUsize::new(
                    failures_before_success,
                ),
                calls: AtomicUsize::new(0),
            }
        }

        /// Return the number of completion attempts observed by the test double.
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl Llm for RetryingLlm {
        /// Return a stable provider id for tests.
        fn provider_id(&self) -> &str {
            "test"
        }

        /// Return a stable model id for tests.
        fn model_id(&self) -> &str {
            "test-model"
        }

        /// Fail the configured number of attempts, then return a text summary.
        fn completion(
            &self,
            _request: CompletionRequest,
        ) -> WasmBoxedFuture<'_, Result<LlmCompletion, CompletionError>>
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let remaining = self.failures_before_success.load(Ordering::SeqCst);
            if remaining > 0 {
                self.failures_before_success
                    .store(remaining - 1, Ordering::SeqCst);
                return Box::pin(async {
                    Err(CompletionError::ProviderError("fail".to_string()))
                });
            }
            Box::pin(async {
                Ok(LlmCompletion {
                    choice: OneOrMany::one(AssistantContent::text(
                        "summary text",
                    )),
                    usage: Default::default(),
                    raw_response: serde_json::json!({}),
                    message_id: None,
                })
            })
        }

        /// Streaming is not used by manual compaction tests.
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

    /// Compaction retries transient failures and retains the configured recent user turns.
    #[tokio::test]
    async fn compactor_retries_three_failures_and_keeps_configured_tail() {
        let llm = RetryingLlm::new(3);
        let compactor = ContextCompactor::new(2);
        let history = vec![
            Message::user("old user"),
            Message::assistant("old answer"),
            Message::user("tail user 1"),
            Message::assistant("tail answer 1"),
            Message::user("tail user 2"),
        ];

        let output = compactor
            .compact_history(&llm, &history)
            .await
            .expect("compaction should succeed")
            .expect("history should be compacted");

        assert_eq!(llm.calls(), 4);
        assert_eq!(output.summary, "summary text");
        assert_eq!(
            output.replacement_history,
            vec![
                Message::user(
                    "Another model previously summarized the conversation. Use this summary as authoritative context for older turns:\n\nsummary text"
                ),
                Message::user("tail user 1"),
                Message::assistant("tail answer 1"),
                Message::user("tail user 2"),
            ]
        );
        assert_eq!(output.retained_message_count, 3);
    }

    /// Compaction does not count tool-result user-role messages as user turn starts.
    #[tokio::test]
    async fn compactor_retained_turns_ignore_tool_result_messages() {
        let llm = RetryingLlm::new(0);
        let compactor = ContextCompactor::new(2);
        let history = vec![
            Message::user("old user"),
            Message::assistant("old answer"),
            Message::user("tail user 1"),
            Message::assistant("tail answer 1"),
            Message::user("tail user 2"),
            Message::tool_result("tool-call", "tool output"),
        ];

        let output = compactor
            .compact_history(&llm, &history)
            .await
            .expect("compaction should succeed")
            .expect("history should be compacted");

        assert_eq!(
            output.replacement_history,
            vec![
                Message::user(
                    "Another model previously summarized the conversation. Use this summary as authoritative context for older turns:\n\nsummary text"
                ),
                Message::user("tail user 1"),
                Message::assistant("tail answer 1"),
                Message::user("tail user 2"),
                Message::tool_result("tool-call", "tool output"),
            ]
        );
    }

    /// Compaction returns an error after the initial attempt and three retries fail.
    #[tokio::test]
    async fn compactor_returns_error_after_initial_attempt_plus_three_retries()
    {
        let llm = RetryingLlm::new(4);
        let compactor = ContextCompactor::new(2);
        let history = vec![
            Message::user("old user"),
            Message::assistant("old answer"),
            Message::user("tail user 1"),
            Message::assistant("tail answer 1"),
            Message::user("tail user 2"),
        ];

        let error = compactor
            .compact_history(&llm, &history)
            .await
            .expect_err("all failed attempts should return an error");

        assert_eq!(llm.calls(), 4);
        assert!(error.to_string().contains("failed after 4 attempts"));
    }
}
