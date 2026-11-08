use std::collections::{HashMap, HashSet};

use protocol::{
    AgentMessage, AgentOutcome, CompactionData, CompactionResult, LaneId,
    MessageContent, MessageId, MessageIdentity, MessageTiming, SessionId,
    SessionMode, TurnId, TurnRecord,
};
use store::{EntryKind, RecordKind, SessionEntry, SessionStore};

use crate::runtime::plan::{ModeChange, ModeNote, ModeNotes};
use crate::runtime::{Kernel, KernelError};

/// Summary entries projected into the messages they contribute to model context.
pub(in crate::runtime) trait SummaryEntry {
    /// Returns a Compaction entry's checkpoint message and its retained tail.
    fn compaction_checkpoint(
        &self,
    ) -> Result<(AgentMessage, Vec<AgentMessage>), KernelError>;

    /// Returns a BranchSummary entry as the message pi places in context.
    fn branch_summary_message(&self) -> Result<AgentMessage, KernelError>;
}

/// Persisted branch-summary fields read back into model context.
#[derive(serde::Deserialize, typed_builder::TypedBuilder)]
#[serde(rename_all = "camelCase")]
struct BranchSummaryPayload {
    // Tolerated when absent so a session with a hand-written or older entry
    // still loads; the summary alone drives model context.
    #[serde(default)]
    #[builder(default)]
    from_id: String,
    summary: String,
    #[serde(default)]
    #[builder(default)]
    details: Option<serde_json::Value>,
    #[serde(default)]
    from_hook: bool,
}

impl SummaryEntry for SessionEntry {
    fn compaction_checkpoint(
        &self,
    ) -> Result<(AgentMessage, Vec<AgentMessage>), KernelError> {
        let data: CompactionData = serde_json::from_value(
            serde_json::Value::Object(self.payload.clone()),
        )?;
        let details = data.details.ok_or_else(|| {
            KernelError::Protocol("compaction details are required".to_string())
        })?;
        let summary_message = AgentMessage {
            identity: MessageIdentity {
                message_id: MessageId::try_from(format!(
                    "compaction-summary-{}",
                    self.id
                ))
                .map_err(|error| KernelError::Protocol(error.to_string()))?,
                turn_id: details.turn_id,
            },
            timing: MessageTiming::try_from((
                details.started_at_ms,
                details.started_at_ms,
                details.ended_at_ms,
            ))
            .map_err(|error| KernelError::Protocol(error.to_string()))?,
            content: MessageContent::CompactionSummary {
                compaction: protocol::CompactionSummaryMessage::builder()
                    .entry_id(self.id.clone())
                    .summary(data.summary)
                    .tokens_before(data.tokens_before)
                    .read_files(details.read_files)
                    .modified_files(details.modified_files)
                    .build(),
            },
        };
        Ok((summary_message, data.retained_tail))
    }

    fn branch_summary_message(&self) -> Result<AgentMessage, KernelError> {
        let payload: BranchSummaryPayload = serde_json::from_value(
            serde_json::Value::Object(self.payload.clone()),
        )?;
        // Only kernel-generated summaries carry pi's file lists; extension
        // details have an arbitrary shape and older entries have none.
        let files = |field: &str| -> Vec<String> {
            payload
                .details
                .as_ref()
                .filter(|_| !payload.from_hook)
                .and_then(|details| details.get(field))
                .and_then(|value| {
                    serde_json::from_value::<Vec<String>>(value.clone()).ok()
                })
                .unwrap_or_default()
        };
        let read_files = files("readFiles");
        let modified_files = files("modifiedFiles");
        // Branch summaries belong to no Turn, so the entry names both ids.
        let id = format!("branch-summary-{}", self.id);
        Ok(AgentMessage {
            identity: MessageIdentity {
                message_id: MessageId::try_from(id.clone()).map_err(
                    |error| KernelError::Protocol(error.to_string()),
                )?,
                turn_id: TurnId::try_from(id).map_err(|error| {
                    KernelError::Protocol(error.to_string())
                })?,
            },
            timing: MessageTiming::try_from((
                self.timestamp_ms,
                self.timestamp_ms,
                self.timestamp_ms,
            ))
            .map_err(|error| KernelError::Protocol(error.to_string()))?,
            content: MessageContent::BranchSummary {
                branch: protocol::BranchSummaryMessage::builder()
                    .entry_id(self.id.clone())
                    .from_id(payload.from_id)
                    .summary(payload.summary)
                    .read_files(read_files)
                    .modified_files(modified_files)
                    .build(),
            },
        })
    }
}

/// Model context rebuilt from a branch, with notes still waiting for tool results.
pub(in crate::runtime) struct RebuiltHistory {
    pub(in crate::runtime) messages: Vec<AgentMessage>,
    pub(in crate::runtime) notes: ModeNotes,
}

impl Kernel {
    /// Computes a safe live-event floor above every expanded durable replay event.
    pub(in crate::runtime) fn replay_sequence_floor(
        store: &dyn SessionStore,
        lane: &LaneId,
    ) -> Result<u64, KernelError> {
        let Some(leaf) = store.lane(lane) else {
            return Ok(store.last_sequence());
        };
        let mut assistant_count = 0_u64;
        for entry in store.branch(leaf)? {
            if entry.kind != EntryKind::Message {
                continue;
            }
            let message: AgentMessage = serde_json::from_value(
                serde_json::Value::Object(entry.payload),
            )
            .map_err(|error| {
                store::StoreError::InvalidSession(format!(
                    "invalid message entry {}: {error}",
                    entry.id
                ))
            })?;
            // Native provenance does not change the assistant role or its lifecycle.
            if matches!(message.content, MessageContent::Assistant { .. }) {
                // Assistant replay expands one persisted Message into
                // MessageEnd plus a possible UsageUpdated event.
                assistant_count = assistant_count.saturating_add(1);
            }
        }
        Ok(store.last_sequence().saturating_add(assistant_count))
    }

    /// Returns every persisted message on the active branch for ACP replay and UI history.
    pub fn session_transcript(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<AgentMessage>, KernelError> {
        let session = self.session(session_id)?;
        session
            .transcript
            .active_branch()?
            .into_iter()
            .filter(|entry| entry.kind == EntryKind::Message)
            .map(|entry| {
                serde_json::from_value(serde_json::Value::Object(entry.payload))
                    .map_err(|error| {
                        store::StoreError::InvalidSession(format!(
                            "invalid message entry {}: {error}",
                            entry.id
                        ))
                        .into()
                    })
            })
            .collect()
    }

    /// Returns durable messages, Turns, and compactions in shared Store sequence order.
    pub fn session_replay(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<protocol::SessionReplayItem>, KernelError> {
        let session = self.session(session_id)?;
        let mut replay = Vec::new();
        let mut active_turn_ids = HashSet::new();
        for entry in session.transcript.active_branch()? {
            let sequence = entry.sequence;
            match entry.kind {
                EntryKind::Message => {
                    let message: AgentMessage = serde_json::from_value(
                        serde_json::Value::Object(entry.payload),
                    )
                    .map_err(|error| {
                        store::StoreError::InvalidSession(format!(
                            "invalid message entry {}: {error}",
                            entry.id
                        ))
                    })?;
                    // Usage is persisted with Assistant metadata, while its model
                    // window is recovered from the current catalog for ACP replay.
                    let context_window = match &message.content {
                        MessageContent::Assistant { metadata, .. } => {
                            active_turn_ids
                                .insert(message.identity.turn_id.clone());
                            session
                                .model
                                .resolve(
                                    &metadata.provider_id,
                                    &metadata.model_id,
                                )
                                .map(|model| model.profile().context_tokens)
                        }
                        MessageContent::System { .. }
                        | MessageContent::User { .. }
                        | MessageContent::ExpandedUser { .. }
                        | MessageContent::ToolResult { .. }
                        | MessageContent::BashExecution { .. }
                        | MessageContent::Extension { .. }
                        | MessageContent::SlashCommand { .. }
                        | MessageContent::CompactionSummary { .. }
                        | MessageContent::BranchSummary { .. } => None,
                    };
                    replay.push((
                        sequence,
                        protocol::SessionReplayItem::Message {
                            message,
                            context_window,
                        },
                    ));
                }
                EntryKind::Compaction => {
                    let data: CompactionData = serde_json::from_value(
                        serde_json::Value::Object(entry.payload),
                    )?;
                    let details = data.details.ok_or_else(|| {
                        store::StoreError::InvalidSession(
                            "compaction details are required".to_string(),
                        )
                    })?;
                    replay.push((
                        sequence,
                        protocol::SessionReplayItem::Compaction {
                            run_id: details.run_id,
                            reason: details.reason,
                            result: CompactionResult::builder()
                                .entry_id(entry.id)
                                .turn_id(details.turn_id)
                                .summary(data.summary)
                                .tokens_before(data.tokens_before)
                                .usage(data.usage)
                                .started_at_ms(details.started_at_ms)
                                .ended_at_ms(details.ended_at_ms)
                                .build(),
                        },
                    ));
                }
                // Clients show the branch summary where it enters context.
                EntryKind::BranchSummary => {
                    replay.push((
                        sequence,
                        protocol::SessionReplayItem::Message {
                            message: entry.branch_summary_message()?,
                            context_window: None,
                        },
                    ));
                }
                EntryKind::ModelChange
                | EntryKind::ThinkingLevelChange
                | EntryKind::ActiveToolsChange
                | EntryKind::Custom => {}
            }
        }
        let records = session.transcript.records()?;
        let mut active_run_turns = HashMap::new();
        for record in &records {
            if record.lane != session.transcript.lane()
                || record.kind != RecordKind::StepAttempt
                || !record.payload.contains_key("turn_id")
                || !record.payload.contains_key("outcome")
            {
                continue;
            }
            let turn: TurnRecord = serde_json::from_value(
                serde_json::Value::Object(record.payload.clone()),
            )?;
            // Lane records are not branch-linked, so only replay settlements
            // whose Assistant remains on the current active branch.
            if active_turn_ids.contains(&turn.identity.turn_id) {
                active_run_turns.insert(
                    turn.identity.run_id.clone(),
                    turn.identity.turn_id.clone(),
                );
                replay.push((
                    record.sequence,
                    protocol::SessionReplayItem::Turn(turn),
                ));
            }
        }
        for record in records {
            if record.lane != session.transcript.lane()
                || record.kind != RecordKind::OperationFinished
            {
                continue;
            }
            let Some(run_id) = record.run_id else {
                continue;
            };
            let Some(turn_id) = active_run_turns.get(&run_id).cloned() else {
                continue;
            };
            // Agent and compaction operations share OperationFinished records;
            // correlation with an active-branch Turn selects Agent Runs only.
            let outcome = match record
                .payload
                .get("status")
                .and_then(serde_json::Value::as_str)
            {
                Some("completed") => AgentOutcome::Succeeded,
                Some("cancelled") => AgentOutcome::Cancelled,
                Some("failed") => AgentOutcome::Failed {
                    message: record
                        .payload
                        .get("error")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| {
                            store::StoreError::InvalidSession(format!(
                                "failed Run record {} is missing its error",
                                record.id
                            ))
                        })?
                        .to_string(),
                },
                Some(status) => {
                    return Err(store::StoreError::InvalidSession(format!(
                        "Run record {} has unsupported status {status}",
                        record.id
                    ))
                    .into());
                }
                None => {
                    return Err(store::StoreError::InvalidSession(format!(
                        "Run record {} is missing its status",
                        record.id
                    ))
                    .into());
                }
            };
            replay.push((
                record.sequence,
                protocol::SessionReplayItem::RunEnd {
                    run_id,
                    turn_id,
                    outcome,
                    ended_at_ms: record.timestamp_ms,
                },
            ));
        }
        replay.sort_by_key(|(sequence, _item)| *sequence);
        Ok(replay.into_iter().map(|(_sequence, item)| item).collect())
    }

    /// Reconstructs compaction-aware model context from one persisted lane branch.
    ///
    /// Mode switches become the same notes, at the same places, that the live
    /// transcript added; `delegated` selects the subagent wording.
    pub(in crate::runtime) fn history_from_store(
        store: &dyn SessionStore,
        lane: &LaneId,
        delegated: bool,
    ) -> Result<RebuiltHistory, KernelError> {
        let mut notes = ModeNotes::default();
        let Some(leaf) = store.lane(lane) else {
            return Ok(RebuiltHistory {
                messages: Vec::new(),
                notes,
            });
        };
        let mut history = Vec::new();
        let mut mode = SessionMode::Default;
        for entry in store.branch(leaf)? {
            match entry.kind {
                EntryKind::Message => {
                    // Persisted transcript corruption is a session-schema
                    // failure, including missing mandatory Assistant metadata.
                    let message = serde_json::from_value(
                        serde_json::Value::Object(entry.payload),
                    )
                    .map_err(|error| {
                        store::StoreError::InvalidSession(format!(
                            "invalid message entry {}: {error}",
                            entry.id
                        ))
                    })?;
                    notes.push(&mut history, message);
                }
                EntryKind::Compaction => {
                    let compaction_entry_id = entry.id.clone();
                    let (summary_message, retained_tail) =
                        entry.compaction_checkpoint()?;
                    history = Vec::with_capacity(
                        retained_tail.len().saturating_add(2),
                    );
                    history.push(summary_message);
                    history.extend(retained_tail);
                    notes.clear();
                    ModeNote::new(mode, delegated).after_compaction(
                        &mut history,
                        &mut notes,
                        &compaction_entry_id,
                    )?;
                }
                EntryKind::Custom => {
                    if let Some(change) = ModeChange::from_entry(&entry) {
                        mode = change.mode;
                        let note = ModeNote::new(change.mode, delegated)
                            .message(
                                &format!("session-mode-{}", entry.id),
                                change.turn_id,
                                entry.timestamp_ms,
                            )?;
                        notes.place(&mut history, note);
                    }
                }
                // Pi places a branch summary in context where it was attached.
                EntryKind::BranchSummary => {
                    notes.push(&mut history, entry.branch_summary_message()?);
                }
                EntryKind::ModelChange
                | EntryKind::ThinkingLevelChange
                | EntryKind::ActiveToolsChange => {}
            }
        }
        Ok(RebuiltHistory {
            messages: history,
            notes,
        })
    }
}
