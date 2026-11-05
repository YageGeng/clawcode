//! Model-facing notes that tell the agent when plan mode starts and ends.
//!
//! A note sits in history where its switch happened, so the prompt prefix before
//! it never changes. The same placement runs live and when history is rebuilt
//! from the Store, so both produce identical context.

use std::collections::HashSet;

use protocol::{
    AgentMessage, ContentBlock, EntryId, ExtensionId, ExtensionMessage,
    MessageContent, MessageId, MessageIdentity, MessageTiming, ProductIdentity,
    SessionMode, TimestampMs, TurnId,
};

use super::MODE_CUSTOM_TYPE;
use crate::runtime::KernelError;

/// Tells an owner Session's agent what plan mode allows and how it ends.
const ENTER_NOTE: &str = "Plan mode is active. Research and plan, but change nothing: \
edit, write, and other tools that modify files are refused, and bash accepts only \
read-only commands such as ls, cat, grep, find, and git log. Investigate the code and \
ask the user about anything unclear. When the plan is complete, call exit_plan_mode \
with the full plan as Markdown in its `plan` argument and wait for the user's \
decision. Do not claim to have started the work.";

/// Tells a delegated subagent what plan mode allows; only its owner can leave it.
const ENTER_DELEGATED_NOTE: &str = "Plan mode is active. Research only and change \
nothing: edit, write, and other tools that modify files are refused, and bash accepts \
only read-only commands. Investigate the task and give your findings as your final \
reply to the parent session.";

/// Tells the agent that edits are allowed again.
const EXIT_NOTE: &str = "Plan mode has ended; you may edit files now.";

/// The note for one mode, worded for an owner Session or a delegated subagent.
pub(in crate::runtime) struct ModeNote {
    mode: SessionMode,
    delegated: bool,
}

impl ModeNote {
    /// Chooses the wording for a mode and the kind of Session that enters it.
    pub(in crate::runtime) fn new(mode: SessionMode, delegated: bool) -> Self {
        Self { mode, delegated }
    }

    /// Builds the hidden context message; its identity comes from the stored switch.
    pub(in crate::runtime) fn message(
        &self,
        message_id: &str,
        turn_id: TurnId,
        at: TimestampMs,
    ) -> Result<AgentMessage, KernelError> {
        let text = match (self.mode, self.delegated) {
            (SessionMode::Plan, false) => ENTER_NOTE,
            (SessionMode::Plan, true) => ENTER_DELEGATED_NOTE,
            (SessionMode::Default, _) => EXIT_NOTE,
        };
        Ok(AgentMessage {
            identity: MessageIdentity {
                message_id: MessageId::try_from(message_id).map_err(
                    |error| KernelError::Protocol(error.to_string()),
                )?,
                turn_id,
            },
            timing: MessageTiming::try_from((at, at, at))
                .map_err(|error| KernelError::Protocol(error.to_string()))?,
            content: MessageContent::Extension {
                extension: ExtensionMessage::builder()
                    .extension_id(
                        ExtensionId::try_from(ProductIdentity::SLUG).map_err(
                            |error| KernelError::Protocol(error.to_string()),
                        )?,
                    )
                    .custom_type(MODE_CUSTOM_TYPE.to_string())
                    .blocks(vec![ContentBlock::Text {
                        text: text.to_string(),
                    }])
                    .display(false)
                    .include_in_context(true)
                    .details(serde_json::json!({ "mode": self.mode }))
                    .build(),
            },
        })
    }

    /// Returns the mode a context message states, if it is one of these notes.
    pub(in crate::runtime) fn stated_mode(
        message: &AgentMessage,
    ) -> Option<SessionMode> {
        match &message.content {
            MessageContent::Extension { extension }
                if extension.custom_type == MODE_CUSTOM_TYPE
                    && extension.extension_id.as_str()
                        == ProductIdentity::SLUG =>
            {
                extension
                    .details
                    .as_ref()
                    .and_then(|details| details.get("mode"))
                    .cloned()
                    .and_then(|mode| serde_json::from_value(mode).ok())
            }
            _ => None,
        }
    }

    /// Restates this note's mode after a compaction summary when the context no
    /// longer ends up stating it.
    ///
    /// The summary drops notes it covers, and a switch made while the summary
    /// was written is in neither the summary nor the retained tail; a context
    /// without any note means default mode. `context` starts with the summary,
    /// whose identity the restated note borrows, and the note goes where a new
    /// switch would, so it waits behind any tool calls still due their results.
    pub(in crate::runtime) fn after_compaction(
        &self,
        context: &mut Vec<AgentMessage>,
        notes: &mut ModeNotes,
        compaction_entry_id: &EntryId,
    ) -> Result<(), KernelError> {
        let stated = context
            .iter()
            .rev()
            .find_map(Self::stated_mode)
            .unwrap_or_default();
        let Some(summary) = context.first() else {
            return Ok(());
        };
        if stated == self.mode {
            return Ok(());
        }
        let note = self.message(
            &format!("session-mode-{compaction_entry_id}"),
            summary.identity.turn_id.clone(),
            summary.timing.ended_at_ms,
        )?;
        notes.place(context, note);
        Ok(())
    }
}

/// Notes that wait until the tool calls before them have their results.
///
/// Providers require a tool result to follow its call directly, so a note that
/// arrives mid-batch waits until the batch's results are in, or until any other
/// message is appended (a crash can leave calls without results).
#[derive(Default)]
pub(in crate::runtime) struct ModeNotes {
    deferred: Vec<AgentMessage>,
}

impl ModeNotes {
    /// Adds a note at the end of history, or holds it while tool results are due.
    pub(in crate::runtime) fn place(
        &mut self,
        history: &mut Vec<AgentMessage>,
        note: AgentMessage,
    ) {
        if Self::awaiting_results(history) {
            self.deferred.push(note);
        } else {
            history.push(note);
        }
    }

    /// Appends one message, releasing held notes where they belong.
    pub(in crate::runtime) fn push(
        &mut self,
        history: &mut Vec<AgentMessage>,
        message: AgentMessage,
    ) {
        let tool_result =
            matches!(message.content, MessageContent::ToolResult { .. });
        if !tool_result {
            history.append(&mut self.deferred);
        }
        history.push(message);
        if tool_result && !Self::awaiting_results(history) {
            history.append(&mut self.deferred);
        }
    }

    /// Drops held notes when history is replaced wholesale.
    pub(in crate::runtime) fn clear(&mut self) {
        self.deferred.clear();
    }

    /// Returns whether the last Assistant message has tool calls without results yet.
    fn awaiting_results(history: &[AgentMessage]) -> bool {
        let Some(position) = history.iter().rposition(|message| {
            matches!(message.content, MessageContent::Assistant { .. })
        }) else {
            return false;
        };
        let (assistant, after) = history.split_at(position + 1);
        let Some(MessageContent::Assistant { blocks, .. }) =
            assistant.last().map(|message| &message.content)
        else {
            return false;
        };
        let answered = after
            .iter()
            .filter_map(|message| match &message.content {
                MessageContent::ToolResult { tool_call_id, .. } => {
                    Some(tool_call_id)
                }
                _ => None,
            })
            .collect::<HashSet<_>>();
        blocks.iter().any(|block| {
            matches!(
                block,
                ContentBlock::ToolCall { tool_call_id, .. }
                    if !answered.contains(tool_call_id)
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use protocol::{
        AgentMessage, CompactionSummaryMessage, EntryId, MessageContent,
        MessageId, MessageIdentity, MessageTiming, SessionMode, TimestampMs,
        TurnId,
    };

    use super::{ModeNote, ModeNotes};

    /// Builds the summary message a compaction context starts with.
    fn summary() -> AgentMessage {
        let at = TimestampMs::from(10);
        AgentMessage {
            identity: MessageIdentity {
                message_id: MessageId::try_from("compaction-summary-entry-9")
                    .expect("message id"),
                turn_id: TurnId::try_from("turn-9").expect("turn id"),
            },
            timing: MessageTiming::try_from((at, at, at)).expect("timing"),
            content: MessageContent::CompactionSummary {
                compaction: CompactionSummaryMessage::builder()
                    .entry_id(EntryId::try_from("entry-9").expect("entry id"))
                    .summary("summary".to_string())
                    .tokens_before(100)
                    .read_files(Vec::new())
                    .modified_files(Vec::new())
                    .build(),
            },
        }
    }

    /// Builds the note one earlier switch left in the retained tail.
    fn note(mode: SessionMode) -> AgentMessage {
        ModeNote::new(mode, false)
            .message(
                "session-mode-entry-1",
                TurnId::try_from("turn-1").expect("turn id"),
                TimestampMs::from(1),
            )
            .expect("note")
    }

    /// Compacts `tail` while the Session is in `mode` and returns the context's
    /// last stated mode.
    fn compacted(
        mode: SessionMode,
        tail: Vec<AgentMessage>,
    ) -> Vec<AgentMessage> {
        let mut context = vec![summary()];
        context.extend(tail);
        let mut notes = ModeNotes::default();
        ModeNote::new(mode, false)
            .after_compaction(
                &mut context,
                &mut notes,
                &EntryId::try_from("entry-9").expect("entry id"),
            )
            .expect("restate");
        context
    }

    /// The last note the model reads states the Session's mode.
    fn last_stated(context: &[AgentMessage]) -> Option<SessionMode> {
        context.iter().rev().find_map(ModeNote::stated_mode)
    }

    /// A plan note compacted away is restated; none is added outside plan mode.
    #[test]
    fn compaction_restates_plan_mode_only_when_lost() {
        let restated = compacted(SessionMode::Plan, Vec::new());
        assert_eq!(last_stated(&restated), Some(SessionMode::Plan));
        assert!(compacted(SessionMode::Default, Vec::new()).len() == 1);
        assert_eq!(
            compacted(SessionMode::Plan, vec![note(SessionMode::Plan)]).len(),
            2
        );
    }

    /// An older note of the other mode in the retained tail, as left by a
    /// switch made while the summary was written, does not hide the current mode.
    #[test]
    fn compaction_restates_a_mode_the_tail_contradicts() {
        let entered =
            compacted(SessionMode::Plan, vec![note(SessionMode::Default)]);
        assert_eq!(last_stated(&entered), Some(SessionMode::Plan));
        let left =
            compacted(SessionMode::Default, vec![note(SessionMode::Plan)]);
        assert_eq!(last_stated(&left), Some(SessionMode::Default));
    }
}
