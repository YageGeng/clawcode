use super::super::*;

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
                        | MessageContent::CompactionSummary { .. } => None,
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
                EntryKind::ModelChange
                | EntryKind::ThinkingLevelChange
                | EntryKind::ActiveToolsChange
                | EntryKind::BranchSummary
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
    pub(in crate::runtime) fn history_from_store(
        store: &dyn SessionStore,
        lane: &LaneId,
    ) -> Result<Vec<AgentMessage>, KernelError> {
        let Some(leaf) = store.lane(lane) else {
            return Ok(Vec::new());
        };
        let mut history = Vec::new();
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
                    history.push(message);
                }
                EntryKind::Compaction => {
                    let compaction_entry_id = entry.id.clone();
                    let data: CompactionData = serde_json::from_value(
                        serde_json::Value::Object(entry.payload),
                    )?;
                    let details = data.details.ok_or_else(|| {
                        KernelError::Protocol(
                            "compaction details are required".to_string(),
                        )
                    })?;
                    let summary_message = AgentMessage {
                        identity: MessageIdentity {
                            message_id: MessageId::try_from(format!(
                                "compaction-summary-{}",
                                compaction_entry_id
                            ))
                            .map_err(|error| {
                                KernelError::Protocol(error.to_string())
                            })?,
                            turn_id: details.turn_id,
                        },
                        timing: MessageTiming::try_from((
                            details.started_at_ms,
                            details.started_at_ms,
                            details.ended_at_ms,
                        ))
                        .map_err(|error| {
                            KernelError::Protocol(error.to_string())
                        })?,
                        content: MessageContent::CompactionSummary {
                            compaction:
                                protocol::CompactionSummaryMessage::builder()
                                    .entry_id(compaction_entry_id)
                                    .summary(data.summary)
                                    .tokens_before(data.tokens_before)
                                    .read_files(details.read_files)
                                    .modified_files(details.modified_files)
                                    .build(),
                        },
                    };
                    history = Vec::with_capacity(
                        data.retained_tail.len().saturating_add(1),
                    );
                    history.push(summary_message);
                    history.extend(data.retained_tail);
                }
                EntryKind::ModelChange
                | EntryKind::ThinkingLevelChange
                | EntryKind::ActiveToolsChange
                | EntryKind::BranchSummary
                | EntryKind::Custom => {}
            }
        }
        Ok(history)
    }
}
