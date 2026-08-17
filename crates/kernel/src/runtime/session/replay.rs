use super::super::*;

impl Kernel {
    /// Returns every persisted message on the active branch for ACP replay and UI history.
    pub fn session_transcript(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<AgentMessage>, KernelError> {
        let session = self.session(session_id)?;
        let store = session
            .store
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        let Some(leaf) = store.lane(&session.lane) else {
            return Ok(Vec::new());
        };
        store
            .branch(leaf)?
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

    /// Returns durable messages and compaction boundaries in active-branch entry order.
    pub fn session_replay(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<protocol::SessionReplayItem>, KernelError> {
        let session = self.session(session_id)?;
        let store = session
            .store
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        let Some(leaf) = store.lane(&session.lane) else {
            return Ok(Vec::new());
        };
        let mut replay = Vec::new();
        for entry in store.branch(leaf)? {
            match entry.kind {
                EntryKind::Message => {
                    let message = serde_json::from_value(
                        serde_json::Value::Object(entry.payload),
                    )
                    .map_err(|error| {
                        store::StoreError::InvalidSession(format!(
                            "invalid message entry {}: {error}",
                            entry.id
                        ))
                    })?;
                    replay.push(protocol::SessionReplayItem::Message(message));
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
                    replay.push(protocol::SessionReplayItem::Compaction {
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
                    });
                }
                EntryKind::ModelChange
                | EntryKind::ThinkingLevelChange
                | EntryKind::ActiveToolsChange
                | EntryKind::BranchSummary
                | EntryKind::Custom => {}
            }
        }
        Ok(replay)
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
