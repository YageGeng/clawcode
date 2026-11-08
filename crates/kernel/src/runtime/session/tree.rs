use protocol::{
    AgentMessage, EntryId, IdKind, MessageContent, SessionId, SessionTreeEntry,
    SessionTreeSnapshot,
};
use store::{EntryKind, NewEntry, SessionEntry};
use tokio_util::sync::CancellationToken;

use super::replay::SummaryEntry;
use super::settings::SessionSettingsUpdate;
use super::transcript::{
    TranscriptNavigationCommit, TranscriptNavigationSnapshot,
};
use crate::runtime::compaction::{
    BRANCH_SUMMARY_PREAMBLE, CompactionFileOperations, EstimatedTokens,
    SummaryGeneration, SummaryProtocol,
};
use crate::runtime::plan::SessionPlanState;
use crate::runtime::{Kernel, KernelError, tools};

/// One tree navigation and whether to summarize the branch it abandons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNavigationRequest {
    /// Entry to navigate to.
    pub target: EntryId,
    /// Whether to summarize the abandoned branch.
    pub summarize: bool,
    /// Extra focus for the generated summary; extensions may override it.
    pub custom_instructions: Option<String>,
}

/// Immutable topology and context captured before one tree navigation.
#[derive(typed_builder::TypedBuilder)]
struct TreeNavigationPlan {
    old_leaf_id: Option<EntryId>,
    target_id: EntryId,
    new_leaf_id: Option<EntryId>,
    common_ancestor_id: Option<EntryId>,
    entries_to_summarize: Vec<SessionEntry>,
}

impl TreeNavigationPlan {
    /// Resolves Pi navigation topology from one consistent Store snapshot.
    fn capture(
        snapshot: TranscriptNavigationSnapshot,
    ) -> Result<Self, KernelError> {
        let TranscriptNavigationSnapshot {
            old_leaf_id,
            target,
            old_branch,
            target_branch,
        } = snapshot;
        let target_id = target.id.clone();
        let common_length = old_branch
            .iter()
            .zip(&target_branch)
            .take_while(|(old, target)| old.id == target.id)
            .count();
        let common_ancestor_id = common_length
            .checked_sub(1)
            .and_then(|index| old_branch.get(index))
            .map(|entry| entry.id.clone());
        let entries_to_summarize = old_branch
            .into_iter()
            .skip(common_length)
            .collect::<Vec<_>>();
        // Pi reopens user input in the editor, so its parent becomes the new leaf.
        let new_leaf_id = if target.kind == EntryKind::Message
            && serde_json::from_value::<AgentMessage>(
                serde_json::Value::Object(target.payload.clone()),
            )
            .is_ok_and(|message| {
                matches!(
                    message.content,
                    MessageContent::User { .. }
                        | MessageContent::ExpandedUser { .. }
                )
            }) {
            target.parent_id
        } else {
            Some(target.id)
        };
        Ok(Self::builder()
            .old_leaf_id(old_leaf_id)
            .target_id(target_id)
            .new_leaf_id(new_leaf_id)
            .common_ancestor_id(common_ancestor_id)
            .entries_to_summarize(entries_to_summarize)
            .build())
    }

    /// Projects abandoned entries into the public extension tree shape.
    fn protocol_entries(&self) -> Vec<SessionTreeEntry> {
        self.entries_to_summarize
            .iter()
            .map(|entry| {
                SessionTreeEntry::builder()
                    .entry_id(entry.id.clone())
                    .parent_id(entry.parent_id.clone())
                    .kind(entry.kind.as_str().to_string())
                    .timestamp_ms(entry.timestamp_ms)
                    .payload(serde_json::Value::Object(entry.payload.clone()))
                    .build()
            })
            .collect()
    }

    /// Selects abandoned-branch messages for a pi-style summary, newest first
    /// within `budget` tokens (zero means unlimited), returned oldest first
    /// together with cumulative file operations.
    fn summary_input(
        &self,
        budget: u64,
    ) -> Result<(Vec<AgentMessage>, CompactionFileOperations), KernelError>
    {
        let mut file_operations = CompactionFileOperations::default();
        // Nested kernel-generated summaries carry files even when their
        // messages fall outside the budget, so they are parsed up front and
        // reused by the selection walk.
        let mut branch_summaries = self
            .entries_to_summarize
            .iter()
            .map(|entry| {
                (entry.kind == EntryKind::BranchSummary)
                    .then(|| entry.branch_summary_message())
                    .transpose()
            })
            .collect::<Result<Vec<_>, _>>()?;
        for message in branch_summaries.iter().flatten() {
            if let MessageContent::BranchSummary { branch } = &message.content {
                file_operations.extend_branch_summary_files(branch);
            }
        }
        let mut selected = Vec::new();
        let mut total_tokens = 0_u64;
        for (index, entry) in self.entries_to_summarize.iter().enumerate().rev()
        {
            let (message, is_summary) = match entry.kind {
                EntryKind::Message => (
                    serde_json::from_value::<AgentMessage>(
                        serde_json::Value::Object(entry.payload.clone()),
                    )?,
                    false,
                ),
                EntryKind::Compaction => {
                    (entry.compaction_checkpoint()?.0, true)
                }
                EntryKind::BranchSummary => {
                    match branch_summaries.get_mut(index).and_then(Option::take)
                    {
                        Some(message) => (message, true),
                        None => continue,
                    }
                }
                EntryKind::ModelChange
                | EntryKind::ThinkingLevelChange
                | EntryKind::ActiveToolsChange
                | EntryKind::Custom => continue,
            };
            let tokens = message.estimated_tokens();
            // Tool calls carry the context of their results, and
            // model-invisible records never reach the summary.
            if !is_summary
                && (matches!(
                    message.content,
                    MessageContent::ToolResult { .. }
                ) || tokens == 0)
            {
                continue;
            }
            file_operations.extend(std::slice::from_ref(&message));
            if budget > 0 && total_tokens.saturating_add(tokens) > budget {
                // A summary is important context, so it still fits while
                // the selection is below 90% of the budget.
                if is_summary
                    && total_tokens.saturating_mul(10)
                        < budget.saturating_mul(9)
                {
                    selected.push(message);
                }
                break;
            }
            total_tokens = total_tokens.saturating_add(tokens);
            selected.push(message);
        }
        selected.reverse();
        Ok((selected, file_operations))
    }
}

impl Kernel {
    /// Reports a moved active branch to Session sinks while the caller still
    /// holds the Session operation, so no Run can start before observers drop
    /// views captured on the old branch.
    ///
    /// The navigation is already committed, so failures are logged rather
    /// than turned into a navigation error.
    async fn report_branch_navigated(
        &self,
        session_id: &SessionId,
        session: &crate::runtime::Session,
        old_leaf_id: Option<EntryId>,
        new_leaf_id: Option<EntryId>,
    ) {
        let event =
            protocol::TurnId::try_from(self.id_generator.next(IdKind::Turn))
                .map_err(|error| KernelError::Protocol(error.to_string()))
                .and_then(|turn_id| {
                    Ok(protocol::AgentEvent {
                        metadata: protocol::EventMetadata {
                            turn_id,
                            timestamp_ms: self.clock.now(),
                            sequence: session.execution.next_sequence()?,
                        },
                        payload: protocol::AgentEventPayload::BranchNavigated {
                            old_leaf_id,
                            new_leaf_id,
                        },
                    })
                });
        let reported = match event {
            Ok(event) => self
                .session_sink(session_id)
                .emit(event)
                .await
                .map_err(|error| error.to_string()),
            Err(error) => Err(error.to_string()),
        };
        if let Err(error) = reported {
            tracing::warn!(
                "failed to report branch navigation for Session {}: {}",
                session_id,
                error
            );
        }
    }

    /// Moves the main lane without generating an abandoned-branch summary.
    pub async fn navigate_session(
        &self,
        session_id: &SessionId,
        target: Option<EntryId>,
    ) -> Result<SessionTreeSnapshot, KernelError> {
        match target {
            Some(target) => {
                self.navigate_session_with_summary(
                    session_id,
                    TreeNavigationRequest {
                        target,
                        summarize: false,
                        custom_instructions: None,
                    },
                )
                .await
            }
            None => {
                let session = self.session(session_id)?;
                let _run_guard = session.acquire_operation().await?;
                let old_leaf_id = session.tools.update(|next| {
                    let old = session.transcript.clear_navigation()?;
                    next.selection = protocol::ToolSelection::default();
                    // The root has no switches, so the mode follows it back to Default.
                    session
                        .plan
                        .replace_mode(protocol::SessionMode::Default)?;
                    Ok(old)
                })?;
                session.notify_mcp_change(
                    protocol::McpSessionChange::Selection,
                    None,
                )?;
                self.report_branch_navigated(
                    session_id,
                    &session,
                    old_leaf_id.clone(),
                    None,
                )
                .await;
                let context = self.extension_context(&session, None, None)?;
                session
                    .extensions
                    .runtime_ref()
                    .emit_session_tree(
                        &protocol::SessionTreeEvent::builder()
                            .old_leaf_id(old_leaf_id)
                            .new_leaf_id(None)
                            .from_extension(false)
                            .build(),
                        &context,
                    )
                    .await;
                self.session_tree(session_id)
            }
        }
    }

    /// Navigates one tree target with optional Pi-compatible branch summarization.
    pub async fn navigate_session_with_summary(
        &self,
        session_id: &SessionId,
        request: TreeNavigationRequest,
    ) -> Result<SessionTreeSnapshot, KernelError> {
        let TreeNavigationRequest {
            target,
            summarize,
            custom_instructions,
        } = request;
        let session = self.session(session_id)?;
        let _run_guard = session.acquire_operation().await?;
        let plan = TreeNavigationPlan::capture(
            session.transcript.navigation_snapshot(&target)?,
        )?;
        if plan.old_leaf_id.as_ref() == Some(&plan.target_id) {
            return self.session_tree(session_id);
        }
        let context = self.extension_context(&session, None, None)?;
        let decision = session
            .extensions
            .runtime_ref()
            .emit_session_before_tree(
                &protocol::SessionBeforeTreeEvent::builder()
                    .target_id(plan.target_id.clone())
                    .old_leaf_id(plan.old_leaf_id.clone())
                    .common_ancestor_id(plan.common_ancestor_id.clone())
                    .entries_to_summarize(plan.protocol_entries())
                    .user_wants_summary(summarize)
                    .custom_instructions(custom_instructions.clone())
                    .replace_instructions(false)
                    .label(None)
                    .build(),
                &context,
            )
            .await;
        if decision.cancel {
            return Err(KernelError::ExtensionBlocked(
                "session tree navigation cancelled".to_string(),
            ));
        }

        let extension_summary = summarize.then_some(decision.summary).flatten();
        let from_extension = extension_summary.is_some();
        let generated_summary = if summarize
            && extension_summary.is_none()
            && !plan.entries_to_summarize.is_empty()
        {
            let budget = session
                .model
                .active()?
                .profile()
                .context_tokens
                .saturating_sub(self.branch_summary.reserve_tokens);
            let (messages, file_operations) = plan.summary_input(budget)?;
            if messages.is_empty() {
                None
            } else {
                let run_id = self.new_run_id()?;
                let turn_id = self.new_turn_id()?;
                let cancellation = CancellationToken::new();
                // The branch summary is a top-level Run with a trace of its
                // own; its start line ties that trace to the Session.
                let generated = self
                    .new_trace_span(&run_id)?
                    .instrument(async {
                        tracing::info!(
                            "started branch summary for session {}",
                            session_id
                        );
                        self.generate_compaction_summary(
                            SummaryGeneration::builder()
                                .session(&session)
                                .run_id(&run_id)
                                .turn_id(&turn_id)
                                .timestamp(self.clock.now())
                                .messages(messages)
                                .cancellation(&cancellation)
                                .protocol(SummaryProtocol::Branch {
                                    // Hook instructions override the caller's.
                                    custom_instructions: decision
                                        .custom_instructions
                                        .as_deref()
                                        .or(custom_instructions.as_deref()),
                                    replace_instructions: decision
                                        .replace_instructions
                                        == Some(true),
                                })
                                .build(),
                        )
                        .await
                    })
                    .await?;
                // Pi frames the summary for the returning conversation and
                // keeps cumulative files in both the text and the details.
                Some((
                    format!(
                        "{BRANCH_SUMMARY_PREAMBLE}{}{}",
                        generated.text,
                        file_operations.summary_suffix()
                    ),
                    serde_json::json!({
                        "readFiles": file_operations.read_files(),
                        "modifiedFiles": file_operations.modified_files(),
                    }),
                    generated.usage,
                ))
            }
        } else {
            None
        };
        let summary = match (&extension_summary, generated_summary) {
            (Some(summary), _) => Some((
                summary.summary.clone(),
                summary.details.clone(),
                summary.usage,
            )),
            (None, Some((summary, details, usage))) => {
                Some((summary, Some(details), Some(usage)))
            }
            (None, None) => None,
        };
        let summary = summary
            .map(|(summary, details, usage)| -> Result<NewEntry, KernelError> {
                Ok(NewEntry {
                    id: EntryId::try_from(
                        self.id_generator.next(IdKind::Entry),
                    )
                    .map_err(|error| {
                        KernelError::Protocol(error.to_string())
                    })?,
                    kind: EntryKind::BranchSummary,
                    payload: serde_json::json!({
                        "fromId": plan.new_leaf_id.as_ref().map(EntryId::as_str).unwrap_or("root"),
                        "summary": summary,
                        "details": details,
                        "usage": usage,
                        "fromHook": from_extension,
                    }),
                })
            })
            .transpose()?;
        let previous_mode = session.plan.mode()?;
        // Apply the lane move, optional summary, label, history rebuild, and
        // durability checkpoint under one transcript lock after all awaits.
        let committed = session.tools.update(|next| {
            // Validate the target selection before moving the lane, under the same lock order as loads.
            let entries =
                session.transcript.branch_at(plan.new_leaf_id.as_ref())?;
            let selection =
                tools::ToolSelectionUpdate::restore(&entries, next)?;
            let committed = session.transcript.commit_navigation(
                TranscriptNavigationCommit::builder()
                    .new_leaf_id(plan.new_leaf_id.clone())
                    .target_id(plan.target_id.clone())
                    .summary(summary)
                    .label(decision.label)
                    .build(),
            )?;
            next.selection = selection;
            // The mode is a branch setting, so it follows the lane to its new leaf.
            session
                .plan
                .replace_mode(SessionPlanState::branch_mode(&entries))?;
            Ok(committed)
        })?;
        session
            .notify_mcp_change(protocol::McpSessionChange::Selection, None)?;
        // The new branch may end in another mode; clients show the current one.
        if session.plan.mode()? != previous_mode {
            SessionSettingsUpdate::builder()
                .kernel(self)
                .session_id(session_id)
                .session(&session)
                .build()
                .publish()
                .await;
        }
        let summary_entry = committed.summary_entry.map(|entry| {
            SessionTreeEntry::builder()
                .entry_id(entry.id)
                .parent_id(entry.parent_id)
                .kind(entry.kind.as_str().to_string())
                .timestamp_ms(entry.timestamp_ms)
                .payload(serde_json::Value::Object(entry.payload))
                .build()
        });
        self.report_branch_navigated(
            session_id,
            &session,
            plan.old_leaf_id.clone(),
            committed.new_leaf_id.clone(),
        )
        .await;
        let context = self.extension_context(&session, None, None)?;
        session
            .extensions
            .runtime_ref()
            .emit_session_tree(
                &protocol::SessionTreeEvent::builder()
                    .new_leaf_id(committed.new_leaf_id)
                    .old_leaf_id(plan.old_leaf_id)
                    .summary_entry(summary_entry)
                    .from_extension(from_extension)
                    .build(),
                &context,
            )
            .await;
        self.session_tree(session_id)
    }
}

#[cfg(test)]
mod tests {
    use protocol::{
        AssistantMetadata, ContentBlock, MessageIdentity, MessageTiming,
        ModelUsage, Sequence, StopReason, TimestampMs, TurnId,
    };

    use super::*;

    /// Wraps one content value in a fixed identity and timing.
    fn message(content: MessageContent) -> AgentMessage {
        let timestamp = TimestampMs::from(1);
        AgentMessage {
            identity: MessageIdentity {
                message_id: protocol::MessageId::try_from("message")
                    .expect("message id"),
                turn_id: TurnId::try_from("turn").expect("turn id"),
            },
            timing: MessageTiming::try_from((timestamp, timestamp, timestamp))
                .expect("timing"),
            content,
        }
    }

    /// A user message whose text has `length` characters.
    fn user(length: usize) -> AgentMessage {
        message(MessageContent::User {
            blocks: vec![ContentBlock::Text {
                text: "u".repeat(length),
            }],
        })
    }

    /// An Assistant with `blocks`.
    fn assistant(blocks: Vec<ContentBlock>) -> AgentMessage {
        message(MessageContent::Assistant {
            blocks,
            metadata: AssistantMetadata::builder()
                .provider_id("p".to_string())
                .model_id("m".to_string())
                .stop_reason(StopReason::EndTurn)
                .usage(ModelUsage::default())
                .build(),
            model_message: None,
        })
    }

    /// An Assistant whose text has `length` characters.
    fn answer(length: usize) -> AgentMessage {
        assistant(vec![ContentBlock::Text {
            text: "a".repeat(length),
        }])
    }

    /// Persists one message as a store entry.
    fn entry(sequence: u64, message: &AgentMessage) -> SessionEntry {
        let serde_json::Value::Object(payload) =
            serde_json::to_value(message).expect("serialize message")
        else {
            unreachable!("messages serialize as objects");
        };
        stored(sequence, EntryKind::Message, payload)
    }

    /// Builds a store entry with an explicit kind and payload.
    fn stored(
        sequence: u64,
        kind: EntryKind,
        payload: serde_json::Map<String, serde_json::Value>,
    ) -> SessionEntry {
        SessionEntry::builder()
            .id(EntryId::try_from(format!("entry-{sequence}")).expect("id"))
            .sequence(Sequence::try_from(sequence).expect("sequence"))
            .timestamp_ms(TimestampMs::from(sequence))
            .kind(kind)
            .payload(payload)
            .build()
    }

    /// A persisted branch-summary entry.
    fn branch_entry(sequence: u64, payload: serde_json::Value) -> SessionEntry {
        let serde_json::Value::Object(payload) = payload else {
            unreachable!("payloads are objects");
        };
        stored(sequence, EntryKind::BranchSummary, payload)
    }

    /// A navigation plan abandoning `entries`.
    fn plan(entries: Vec<SessionEntry>) -> TreeNavigationPlan {
        TreeNavigationPlan::builder()
            .old_leaf_id(None)
            .target_id(EntryId::try_from("target").expect("target"))
            .new_leaf_id(None)
            .common_ancestor_id(None)
            .entries_to_summarize(entries)
            .build()
    }

    /// Selection keeps the newest messages that fit the budget, oldest first.
    #[test]
    fn summary_input_keeps_newest_messages_within_budget() {
        let messages = [user(40), answer(40), user(8), answer(8)];
        let entries = messages
            .iter()
            .enumerate()
            .map(|(index, message)| entry(index as u64 + 1, message))
            .collect();

        let (selected, _) = plan(entries).summary_input(8).expect("input");

        assert_eq!(selected, messages[2..].to_vec());
    }

    /// A zero budget selects every message, like pi's `tokenBudget > 0` check.
    #[test]
    fn zero_budget_means_unlimited() {
        let messages = [user(40), answer(40), user(8), answer(8)];
        let entries = messages
            .iter()
            .enumerate()
            .map(|(index, message)| entry(index as u64 + 1, message))
            .collect();

        let (selected, _) = plan(entries).summary_input(0).expect("input");

        assert_eq!(selected.len(), 4);
    }

    /// A summary entry over the budget still fits while selection is under 90%.
    #[test]
    fn oversized_summary_entry_fits_under_ninety_percent() {
        let entries = || {
            vec![
                branch_entry(
                    1,
                    serde_json::json!({
                        "fromId": "root",
                        "summary": "s".repeat(48),
                        "details": null,
                        "fromHook": false,
                    }),
                ),
                entry(2, &user(8)),
                entry(3, &answer(8)),
            ]
        };

        let (roomy, _) = plan(entries()).summary_input(15).expect("input");
        let (tight, _) = plan(entries()).summary_input(4).expect("input");

        assert_eq!(roomy.len(), 3);
        assert_eq!(tight.len(), 2);
    }

    /// Tool results and model-invisible messages never enter the summary input.
    #[test]
    fn summary_input_skips_tool_results_and_invisible_messages() {
        let hidden = message(MessageContent::Extension {
            extension: protocol::ExtensionMessage {
                extension_id: protocol::ExtensionId::try_from("ext")
                    .expect("extension id"),
                custom_type: "note".to_string(),
                blocks: vec![ContentBlock::Text {
                    text: "hidden".to_string(),
                }],
                display: true,
                include_in_context: false,
                details: None,
            },
        });
        let result = message(MessageContent::ToolResult {
            tool_call_id: protocol::ToolCallId::try_from("call")
                .expect("tool call id"),
            blocks: vec![ContentBlock::Text {
                text: "result".to_string(),
            }],
            is_error: false,
            details: None,
            output: None,
            cancelled: false,
        });
        let entries = vec![
            entry(1, &user(8)),
            entry(2, &result),
            entry(3, &hidden),
            entry(4, &answer(8)),
        ];

        let (selected, _) = plan(entries).summary_input(0).expect("input");

        assert_eq!(selected, vec![user(8), answer(8)]);
    }

    /// Nested kernel-generated branch summaries carry their files forward.
    #[test]
    fn summary_input_absorbs_nested_branch_summary_files() {
        let read_call = assistant(vec![ContentBlock::ToolCall {
            tool_call_id: protocol::ToolCallId::try_from("read")
                .expect("call id"),
            name: "read".to_string(),
            arguments: serde_json::json!({ "path": "input.txt" }),
            invalid_arguments: None,
        }]);
        let nested = |from_hook: bool, file: &str| {
            branch_entry(
                1,
                serde_json::json!({
                    "fromId": "root",
                    "summary": "earlier branch",
                    "details": { "readFiles": [file], "modifiedFiles": [] },
                    "fromHook": from_hook,
                }),
            )
        };

        let (_, generated) =
            plan(vec![nested(false, "a.rs"), entry(2, &read_call)])
                .summary_input(0)
                .expect("input");
        let (_, hooked) =
            plan(vec![nested(true, "b.rs"), entry(2, &read_call)])
                .summary_input(0)
                .expect("input");

        assert_eq!(generated.read_files(), vec!["a.rs", "input.txt"]);
        assert_eq!(hooked.read_files(), vec!["input.txt"]);
    }
}
