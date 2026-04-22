use llm::{completion::Message, usage::Usage};

use crate::context::{TurnContext, TurnContextItem};

/// One finalized turn retained in prompt-visible history.
#[derive(Debug, Clone)]
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

        if messages.len() > limit {
            messages = messages.split_off(messages.len() - limit);
        }
        messages
    }
}
