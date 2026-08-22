use std::path::PathBuf;
use std::sync::Mutex;

use protocol::{
    AgentMessage, EntryId, ExtensionEntryData, LaneId, SessionId,
    SessionTreeEntry, SessionTreeSnapshot,
};
use store::{
    EntryKind, NewEntry, NewRecord, SessionEntry, SessionRecord, SessionStore,
};

use super::KernelError;

/// Owns one lane's durable Store and its compaction-aware model context.
pub(super) struct SessionTranscript {
    lane: LaneId,
    state: Mutex<SessionTranscriptState>,
}

/// State that must change under one lock to keep Store and history consistent.
struct SessionTranscriptState {
    store: Box<dyn SessionStore>,
    history: Vec<AgentMessage>,
}

/// Owned Store topology used to plan tree navigation without retaining a lock.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct TranscriptNavigationSnapshot {
    #[builder(default)]
    pub(super) old_leaf_id: Option<EntryId>,
    pub(super) target: SessionEntry,
    pub(super) old_branch: Vec<SessionEntry>,
    pub(super) target_branch: Vec<SessionEntry>,
}

/// Inputs committed atomically after asynchronous tree hooks and summarization.
#[derive(typed_builder::TypedBuilder)]
pub(super) struct TranscriptNavigationCommit {
    #[builder(default)]
    pub(super) new_leaf_id: Option<EntryId>,
    pub(super) target_id: EntryId,
    #[builder(default)]
    pub(super) summary: Option<NewEntry>,
    #[builder(default)]
    pub(super) label: Option<String>,
}

/// Owned values needed to emit the tree event after its Store commit.
pub(super) struct TranscriptNavigationResult {
    pub(super) summary_entry: Option<SessionEntry>,
    pub(super) new_leaf_id: Option<EntryId>,
}

impl SessionTranscript {
    /// Creates a transcript from one opened Store and its reconstructed context.
    pub(super) fn new(
        store: Box<dyn SessionStore>,
        lane: LaneId,
        history: Vec<AgentMessage>,
    ) -> Self {
        Self {
            lane,
            state: Mutex::new(SessionTranscriptState { store, history }),
        }
    }

    /// Returns the stable Session identifier as an owned value.
    pub(super) fn session_id(&self) -> Result<SessionId, KernelError> {
        self.state
            .lock()
            .map(|state| state.store.session_id().clone())
            .map_err(|_poison_error| KernelError::Poisoned)
    }

    /// Returns the backing Store path as an owned value.
    pub(super) fn path(&self) -> Result<PathBuf, KernelError> {
        self.state
            .lock()
            .map(|state| state.store.path().to_path_buf())
            .map_err(|_poison_error| KernelError::Poisoned)
    }

    /// Returns the active lane identifier as an owned value.
    pub(super) fn lane(&self) -> LaneId {
        self.lane.clone()
    }

    /// Returns the latest optional Session name as an owned value.
    pub(super) fn name(&self) -> Result<Option<String>, KernelError> {
        self.state
            .lock()
            .map(|state| state.store.name().map(ToOwned::to_owned))
            .map_err(|_poison_error| KernelError::Poisoned)
    }

    /// Returns a defensive copy of the model-visible context.
    pub(super) fn history(&self) -> Result<Vec<AgentMessage>, KernelError> {
        self.state
            .lock()
            .map(|state| state.history.clone())
            .map_err(|_poison_error| KernelError::Poisoned)
    }

    /// Inspects model-visible context under the transcript lock without cloning it.
    pub(super) fn inspect_history<R>(
        &self,
        inspect: impl FnOnce(&[AgentMessage]) -> R,
    ) -> Result<R, KernelError> {
        self.state
            .lock()
            .map(|state| inspect(&state.history))
            .map_err(|_poison_error| KernelError::Poisoned)
    }

    /// Returns the current lane leaf as an owned value.
    pub(super) fn leaf_id(&self) -> Result<Option<EntryId>, KernelError> {
        self.state
            .lock()
            .map(|state| state.store.lane(&self.lane).cloned())
            .map_err(|_poison_error| KernelError::Poisoned)
    }

    /// Returns the active branch as owned Store entries.
    pub(super) fn active_branch(
        &self,
    ) -> Result<Vec<SessionEntry>, KernelError> {
        let state = self
            .state
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        let Some(leaf) = state.store.lane(&self.lane) else {
            return Ok(Vec::new());
        };
        Ok(state.store.branch(leaf)?)
    }

    /// Returns durable operation records for replay and recovery inspection.
    pub(super) fn records(&self) -> Result<Vec<SessionRecord>, KernelError> {
        self.state
            .lock()
            .map(|state| state.store.records())
            .map_err(|_poison_error| KernelError::Poisoned)
    }

    /// Builds the public tree snapshot from one consistent Store view.
    pub(super) fn tree_snapshot(
        &self,
    ) -> Result<SessionTreeSnapshot, KernelError> {
        let state = self
            .state
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        let entries = state
            .store
            .entries()
            .into_iter()
            .map(|entry| {
                SessionTreeEntry::builder()
                    .entry_id(entry.id)
                    .parent_id(entry.parent_id)
                    .kind(entry.kind.as_str().to_string())
                    .timestamp_ms(entry.timestamp_ms)
                    .payload(serde_json::Value::Object(entry.payload))
                    .build()
            })
            .collect();
        Ok(SessionTreeSnapshot::builder()
            .session_id(state.store.session_id().clone())
            .lane(self.lane.clone())
            .leaf_id(state.store.lane(&self.lane).cloned())
            .entries(entries)
            .name(state.store.name().map(ToOwned::to_owned))
            .build())
    }

    /// Appends a context message and publishes it to history under one lock.
    pub(super) fn append_context_message(
        &self,
        entry_id: EntryId,
        message: &AgentMessage,
    ) -> Result<(), KernelError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        state.store.append_entry(
            &self.lane,
            NewEntry {
                id: entry_id,
                kind: EntryKind::Message,
                payload: serde_json::to_value(message)?,
            },
        )?;
        state.history.push(message.clone());
        Ok(())
    }

    /// Appends a Store-only entry without changing model-visible history.
    pub(super) fn append_store_only_entry(
        &self,
        entry: NewEntry,
    ) -> Result<SessionEntry, KernelError> {
        Ok(self
            .state
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .store
            .append_entry(&self.lane, entry)?)
    }

    /// Appends one durable operation record without changing history.
    pub(super) fn append_record(
        &self,
        record: NewRecord,
    ) -> Result<SessionRecord, KernelError> {
        Ok(self
            .state
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .store
            .append_record(record)?)
    }

    /// Appends a compaction boundary and replaces context under one lock.
    pub(super) fn commit_compaction(
        &self,
        entry: NewEntry,
        context: Vec<AgentMessage>,
    ) -> Result<(), KernelError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        state.store.append_entry(&self.lane, entry)?;
        state.history = context;
        Ok(())
    }

    /// Flushes all buffered transcript mutations to durable storage.
    pub(super) fn sync(&self) -> Result<(), KernelError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        if let Err(error) = state.store.sync() {
            tracing::error!(
                "failed to sync persistent state for session {}: {}",
                state.store.session_id(),
                error
            );
            return Err(error.into());
        }
        Ok(())
    }

    /// Sets or clears the durable Session name.
    pub(super) fn set_name(
        &self,
        name: Option<String>,
    ) -> Result<(), KernelError> {
        self.state
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .store
            .set_name(name)?;
        Ok(())
    }

    /// Sets the Session name only when no prior explicit or default name exists.
    pub(super) fn set_default_name(
        &self,
        name: String,
    ) -> Result<Option<String>, KernelError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        if state.store.name().is_some() {
            return Ok(None);
        }
        state.store.set_name(Some(name.clone()))?;
        Ok(Some(name))
    }

    /// Sets or clears one durable entry label.
    pub(super) fn set_label(
        &self,
        entry_id: EntryId,
        label: Option<String>,
    ) -> Result<(), KernelError> {
        self.state
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .store
            .set_label(entry_id, label)?;
        Ok(())
    }

    /// Appends one typed extension entry without changing model history.
    pub(super) fn append_extension_entry(
        &self,
        entry_id: EntryId,
        entry: ExtensionEntryData,
    ) -> Result<SessionEntry, KernelError> {
        Ok(self
            .state
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?
            .store
            .append_extension_entry(&self.lane, entry_id, entry)?)
    }

    /// Captures all topology required to plan tree navigation consistently.
    pub(super) fn navigation_snapshot(
        &self,
        target_id: &EntryId,
    ) -> Result<TranscriptNavigationSnapshot, KernelError> {
        let state = self
            .state
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        let target =
            state.store.get_entry(target_id).cloned().ok_or_else(|| {
                store::StoreError::EntryNotFound(target_id.clone())
            })?;
        let old_leaf_id = state.store.lane(&self.lane).cloned();
        let old_branch = old_leaf_id
            .as_ref()
            .map(|leaf| state.store.branch(leaf))
            .transpose()?
            .unwrap_or_default();
        let target_branch = state.store.branch(target_id)?;
        Ok(TranscriptNavigationSnapshot::builder()
            .old_leaf_id(old_leaf_id)
            .target(target)
            .old_branch(old_branch)
            .target_branch(target_branch)
            .build())
    }

    /// Moves the active lane to root and returns the leaf selected before the change.
    pub(super) fn clear_navigation(
        &self,
    ) -> Result<Option<EntryId>, KernelError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        let old_leaf_id = state.store.lane(&self.lane).cloned();
        state.store.move_lane(&self.lane, None)?;
        let old_history = std::mem::take(&mut state.history);
        if let Err(error) = state.store.sync() {
            // Root navigation is still reversible because it appends no entry;
            // restore both observable states before reporting checkpoint failure.
            match state.store.move_lane(&self.lane, old_leaf_id.clone()) {
                Ok(()) => {
                    state.history = old_history;
                    if let Err(rollback_error) = state.store.sync() {
                        tracing::warn!(
                            "failed to sync Session root navigation rollback: {}",
                            rollback_error
                        );
                    }
                }
                Err(rollback_error) => {
                    tracing::warn!(
                        "failed to restore Session lane after root navigation error: {}",
                        rollback_error
                    );
                    // Keep model context aligned with the lane retained by the
                    // Store when restoring the previous leaf also fails.
                    match Self::history_from_store(
                        state.store.as_ref(),
                        &self.lane,
                    ) {
                        Ok(history) => state.history = history,
                        Err(reconcile_error) => tracing::error!(
                            "failed to reconcile Session history after root navigation rollback error: {}",
                            reconcile_error
                        ),
                    }
                }
            }
            return Err(error.into());
        }
        Ok(old_leaf_id)
    }

    /// Commits navigation and compensates only mutations that remain reversible.
    pub(super) fn commit_navigation(
        &self,
        commit: TranscriptNavigationCommit,
    ) -> Result<TranscriptNavigationResult, KernelError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_poison_error| KernelError::Poisoned)?;
        let old_leaf_id = state.store.lane(&self.lane).cloned();
        let TranscriptNavigationCommit {
            new_leaf_id,
            target_id,
            summary,
            label,
        } = commit;
        let mut label_rollback = None;
        let mut appended_summary = None;
        let mut reached_sync = false;
        let mutation = (|| -> Result<_, KernelError> {
            state.store.move_lane(&self.lane, new_leaf_id)?;
            appended_summary = summary
                .map(|entry| state.store.append_entry(&self.lane, entry))
                .transpose()?;
            if let Some(label) = label {
                let label_entry_id = appended_summary
                    .as_ref()
                    .map(|entry| entry.id.clone())
                    .unwrap_or(target_id);
                label_rollback = Some((
                    label_entry_id.clone(),
                    state.store.label(&label_entry_id).map(ToOwned::to_owned),
                ));
                state.store.set_label(label_entry_id, Some(label))?;
            }
            let history =
                Self::history_from_store(state.store.as_ref(), &self.lane)?;
            // Reaching this point means every logical mutation succeeded and
            // only the final durability checkpoint can still fail.
            reached_sync = true;
            state.store.sync()?;
            Ok((state.store.lane(&self.lane).cloned(), history))
        })();
        match mutation {
            Ok((new_leaf_id, history)) => {
                state.history = history;
                Ok(TranscriptNavigationResult {
                    summary_entry: appended_summary,
                    new_leaf_id,
                })
            }
            Err(error) => {
                if appended_summary.is_some() {
                    // Branch summaries are append-only and cannot be removed.
                    // Keep the summary as the active leaf so tree snapshots do
                    // not expose an unreachable entry after a later failure.
                    let history_reconciled = match Self::history_from_store(
                        state.store.as_ref(),
                        &self.lane,
                    ) {
                        Ok(history) => {
                            state.history = history;
                            true
                        }
                        Err(reconcile_error) => {
                            tracing::error!(
                                "failed to reconcile Session history after post-summary navigation error: {}",
                                reconcile_error
                            );
                            false
                        }
                    };
                    let sync_recovered = match state.store.sync() {
                        Ok(()) => true,
                        Err(sync_error) => {
                            tracing::warn!(
                                "failed to sync Session navigation after preserving appended summary: {}",
                                sync_error
                            );
                            false
                        }
                    };
                    if reached_sync && history_reconciled && sync_recovered {
                        // The first sync was the only failed step, so the retry
                        // completes the original commit and callers must emit
                        // its tree event instead of reporting a false failure.
                        return Ok(TranscriptNavigationResult {
                            summary_entry: appended_summary,
                            new_leaf_id: state.store.lane(&self.lane).cloned(),
                        });
                    }
                    return Err(error);
                }
                // Store mutations are append-only, so compensate observable
                // label and lane facts only before a summary has been appended.
                if let Some((entry_id, previous_label)) = label_rollback
                    && let Err(rollback_error) =
                        state.store.set_label(entry_id, previous_label)
                {
                    tracing::warn!(
                        "failed to restore Session navigation label after error: {}",
                        rollback_error
                    );
                }
                match state.store.move_lane(&self.lane, old_leaf_id) {
                    Ok(()) => {
                        if let Err(rollback_error) = state.store.sync() {
                            tracing::warn!(
                                "failed to sync Session navigation rollback: {}",
                                rollback_error
                            );
                        }
                    }
                    Err(rollback_error) => {
                        tracing::warn!(
                            "failed to restore Session lane after navigation error: {}",
                            rollback_error
                        );
                        // If compensation itself fails, align the in-memory
                        // context with whichever lane the Store retained.
                        match Self::history_from_store(
                            state.store.as_ref(),
                            &self.lane,
                        ) {
                            Ok(history) => state.history = history,
                            Err(reconcile_error) => tracing::error!(
                                "failed to reconcile Session history after navigation rollback error: {}",
                                reconcile_error
                            ),
                        }
                    }
                }
                Err(error)
            }
        }
    }

    /// Reconstructs compaction-aware model context from one Store lane.
    pub(super) fn history_from_store(
        store: &dyn SessionStore,
        lane: &LaneId,
    ) -> Result<Vec<AgentMessage>, KernelError> {
        super::Kernel::history_from_store(store, lane)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    use protocol::{
        AgentMessage, ContentBlock, EntryId, MessageContent, MessageId,
        MessageIdentity, MessageTiming, SessionId, TimestampMs, TurnId,
    };
    use store::{
        EntryKind, JsonlStoreFactory, NewEntry, NewRecord,
        SessionCreateOptions, SessionEntry, SessionRecord, SessionStore,
        StoreError, StoreFactory, SystemClock,
    };

    use super::{SessionTranscript, TranscriptNavigationCommit};

    /// Creates one minimal context message with deterministic test identity.
    fn message(message_id: &str, turn_id: &str, text: &str) -> AgentMessage {
        AgentMessage {
            identity: MessageIdentity {
                message_id: MessageId::try_from(message_id)
                    .expect("message id"),
                turn_id: TurnId::try_from(turn_id).expect("turn id"),
            },
            timing: MessageTiming::try_from((
                TimestampMs::from(1),
                TimestampMs::from(1),
                TimestampMs::from(1),
            ))
            .expect("timing"),
            content: MessageContent::User {
                blocks: vec![ContentBlock::Text {
                    text: text.to_string(),
                }],
            },
        }
    }

    /// Keeps the durable entry and model-visible history in one transcript update.
    #[test]
    fn context_message_updates_store_and_history_together() {
        let root = tempfile::tempdir().expect("temporary store root");
        let factory =
            JsonlStoreFactory::new(root.path(), Arc::new(SystemClock));
        let session_id = SessionId::try_from("session-1").expect("session id");
        let store = factory
            .create(SessionCreateOptions {
                session_id,
                cwd: PathBuf::from("/workspace"),
                parent_session_id: None,
            })
            .expect("create store");
        let transcript = SessionTranscript::new(
            store,
            protocol::LaneId::try_from("main").expect("lane id"),
            Vec::new(),
        );
        let message = message("message-1", "turn-1", "hello");

        transcript
            .append_context_message(
                EntryId::try_from("entry-1").expect("entry id"),
                &message,
            )
            .expect("append context message");

        assert_eq!(transcript.history().expect("history"), vec![message]);
        assert_eq!(transcript.tree_snapshot().expect("tree").entries.len(), 1);
    }

    /// Restores the prior observable branch when a post-move append fails.
    #[test]
    fn failed_navigation_restores_the_previous_lane_and_history() {
        let root = tempfile::tempdir().expect("temporary store root");
        let factory =
            JsonlStoreFactory::new(root.path(), Arc::new(SystemClock));
        let session_id = SessionId::try_from("session-1").expect("session id");
        let mut store = factory
            .create(SessionCreateOptions {
                session_id,
                cwd: PathBuf::from("/workspace"),
                parent_session_id: None,
            })
            .expect("create store");
        let lane = protocol::LaneId::try_from("main").expect("lane id");
        let first = message("message-1", "turn-1", "first");
        let second = message("message-2", "turn-2", "second");
        let first_entry_id = EntryId::try_from("entry-1").expect("entry id");
        let second_entry_id = EntryId::try_from("entry-2").expect("entry id");
        store
            .append_entry(
                &lane,
                NewEntry {
                    id: first_entry_id.clone(),
                    kind: EntryKind::Message,
                    payload: serde_json::to_value(&first)
                        .expect("first payload"),
                },
            )
            .expect("append first entry");
        store
            .append_entry(
                &lane,
                NewEntry {
                    id: second_entry_id.clone(),
                    kind: EntryKind::Message,
                    payload: serde_json::to_value(&second)
                        .expect("second payload"),
                },
            )
            .expect("append second entry");
        let transcript = SessionTranscript::new(
            store,
            lane,
            vec![first.clone(), second.clone()],
        );

        let error = match transcript.commit_navigation(
            TranscriptNavigationCommit::builder()
                .new_leaf_id(Some(first_entry_id.clone()))
                .target_id(first_entry_id.clone())
                .summary(Some(NewEntry {
                    // Reusing an existing id makes the append fail after
                    // the lane move, reproducing a partial commit.
                    id: first_entry_id,
                    kind: EntryKind::BranchSummary,
                    payload: serde_json::json!({ "summary": "unused" }),
                }))
                .build(),
        ) {
            Ok(_result) => panic!("duplicate summary id must fail navigation"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            crate::KernelError::Store(store::StoreError::DuplicateEntry(_))
        ));
        assert_eq!(
            transcript.leaf_id().expect("lane leaf"),
            Some(second_entry_id)
        );
        assert_eq!(transcript.history().expect("history"), vec![first, second]);
    }

    /// Store wrapper that injects one sync failure at a selected boundary.
    struct FailingSyncStore {
        inner: Box<dyn SessionStore>,
        fail_sync: bool,
        arm_after_summary: bool,
    }

    impl SessionStore for FailingSyncStore {
        /// Returns the delegated Session identifier.
        fn session_id(&self) -> &SessionId {
            self.inner.session_id()
        }

        /// Returns the delegated persistence path.
        fn path(&self) -> &Path {
            self.inner.path()
        }

        /// Delegates lane creation to the real Store.
        fn create_lane(
            &mut self,
            lane: protocol::LaneId,
            at: Option<EntryId>,
        ) -> Result<(), StoreError> {
            self.inner.create_lane(lane, at)
        }

        /// Delegates lane movement to the real Store.
        fn move_lane(
            &mut self,
            lane: &protocol::LaneId,
            to: Option<EntryId>,
        ) -> Result<(), StoreError> {
            self.inner.move_lane(lane, to)
        }

        /// Arms one sync failure after a durable branch-summary append.
        fn append_entry(
            &mut self,
            lane: &protocol::LaneId,
            entry: NewEntry,
        ) -> Result<SessionEntry, StoreError> {
            let is_summary = entry.kind == EntryKind::BranchSummary;
            let appended = self.inner.append_entry(lane, entry)?;
            if is_summary && self.arm_after_summary {
                self.fail_sync = true;
            }
            Ok(appended)
        }

        /// Returns the delegated lane leaf.
        fn lane(&self, lane: &protocol::LaneId) -> Option<&EntryId> {
            self.inner.lane(lane)
        }

        /// Returns one delegated entry.
        fn get_entry(&self, id: &EntryId) -> Option<&SessionEntry> {
            self.inner.get_entry(id)
        }

        /// Returns all delegated entries.
        fn entries(&self) -> Vec<SessionEntry> {
            self.inner.entries()
        }

        /// Returns one delegated root-to-leaf branch.
        fn branch(
            &self,
            leaf: &EntryId,
        ) -> Result<Vec<SessionEntry>, StoreError> {
            self.inner.branch(leaf)
        }

        /// Delegates durable operation records.
        fn append_record(
            &mut self,
            record: NewRecord,
        ) -> Result<SessionRecord, StoreError> {
            self.inner.append_record(record)
        }

        /// Injects one post-summary durability failure before delegating later syncs.
        fn sync(&mut self) -> Result<(), StoreError> {
            if std::mem::take(&mut self.fail_sync) {
                return Err(std::io::Error::other(
                    "injected post-summary sync failure",
                )
                .into());
            }
            self.inner.sync()
        }

        /// Returns all delegated operation records.
        fn records(&self) -> Vec<SessionRecord> {
            self.inner.records()
        }

        /// Delegates the latest shared mutation sequence.
        fn last_sequence(&self) -> u64 {
            self.inner.last_sequence()
        }

        /// Delegates Session naming.
        fn set_name(&mut self, name: Option<String>) -> Result<(), StoreError> {
            self.inner.set_name(name)
        }

        /// Returns the delegated Session name.
        fn name(&self) -> Option<&str> {
            self.inner.name()
        }

        /// Delegates entry labeling.
        fn set_label(
            &mut self,
            target_id: EntryId,
            label: Option<String>,
        ) -> Result<(), StoreError> {
            self.inner.set_label(target_id, label)
        }

        /// Returns the delegated entry label.
        fn label(&self, target_id: &EntryId) -> Option<&str> {
            self.inner.label(target_id)
        }
    }

    /// Returns a committed navigation after a transient post-summary sync failure.
    #[test]
    fn post_summary_sync_retry_returns_committed_navigation() {
        let root = tempfile::tempdir().expect("temporary store root");
        let factory =
            JsonlStoreFactory::new(root.path(), Arc::new(SystemClock));
        let mut store = factory
            .create(SessionCreateOptions {
                session_id: SessionId::try_from("session-summary-failure")
                    .expect("session id"),
                cwd: PathBuf::from("/workspace"),
                parent_session_id: None,
            })
            .expect("create store");
        let lane = protocol::LaneId::try_from("main").expect("lane id");
        let first = message("message-1", "turn-1", "first");
        let second = message("message-2", "turn-2", "second");
        let first_entry_id = EntryId::try_from("entry-1").expect("entry id");
        let second_entry_id = EntryId::try_from("entry-2").expect("entry id");
        let summary_entry_id =
            EntryId::try_from("entry-summary").expect("summary id");
        for (entry_id, message) in [
            (first_entry_id.clone(), &first),
            (second_entry_id.clone(), &second),
        ] {
            store
                .append_entry(
                    &lane,
                    NewEntry {
                        id: entry_id,
                        kind: EntryKind::Message,
                        payload: serde_json::to_value(message)
                            .expect("message payload"),
                    },
                )
                .expect("append message");
        }
        let transcript = SessionTranscript::new(
            Box::new(FailingSyncStore {
                inner: store,
                fail_sync: false,
                arm_after_summary: true,
            }),
            lane,
            vec![first.clone(), second],
        );

        let result = transcript
            .commit_navigation(
                TranscriptNavigationCommit::builder()
                    .new_leaf_id(Some(first_entry_id))
                    .target_id(EntryId::try_from("entry-1").expect("target id"))
                    .summary(Some(NewEntry {
                        id: summary_entry_id.clone(),
                        kind: EntryKind::BranchSummary,
                        payload: serde_json::json!({ "summary": "branch" }),
                    }))
                    .build(),
            )
            .expect("sync retry must recover the committed navigation");

        assert_eq!(
            result.summary_entry.expect("committed summary entry").id,
            summary_entry_id
        );
        assert_eq!(
            transcript.leaf_id().expect("lane leaf"),
            result.new_leaf_id
        );
        assert_eq!(transcript.history().expect("history"), vec![first]);
        assert_eq!(transcript.tree_snapshot().expect("tree").entries.len(), 3);
    }

    /// Restores the original root navigation state when its first sync fails.
    #[test]
    fn clear_navigation_restores_leaf_and_history_after_sync_failure() {
        let root = tempfile::tempdir().expect("temporary store root");
        let factory =
            JsonlStoreFactory::new(root.path(), Arc::new(SystemClock));
        let mut store = factory
            .create(SessionCreateOptions {
                session_id: SessionId::try_from("session-clear-failure")
                    .expect("session id"),
                cwd: PathBuf::from("/workspace"),
                parent_session_id: None,
            })
            .expect("create store");
        let lane = protocol::LaneId::try_from("main").expect("lane id");
        let first = message("message-1", "turn-1", "first");
        let second = message("message-2", "turn-2", "second");
        let first_entry_id = EntryId::try_from("entry-1").expect("entry id");
        let second_entry_id = EntryId::try_from("entry-2").expect("entry id");
        for (entry_id, message) in
            [(first_entry_id, &first), (second_entry_id.clone(), &second)]
        {
            store
                .append_entry(
                    &lane,
                    NewEntry {
                        id: entry_id,
                        kind: EntryKind::Message,
                        payload: serde_json::to_value(message)
                            .expect("message payload"),
                    },
                )
                .expect("append message");
        }
        let transcript = SessionTranscript::new(
            Box::new(FailingSyncStore {
                inner: store,
                fail_sync: true,
                arm_after_summary: false,
            }),
            lane,
            vec![first.clone(), second.clone()],
        );

        let error = transcript
            .clear_navigation()
            .expect_err("injected sync failure must reject root navigation");

        assert!(matches!(error, crate::KernelError::Store(_)));
        assert_eq!(
            transcript.leaf_id().expect("lane leaf"),
            Some(second_entry_id)
        );
        assert_eq!(transcript.history().expect("history"), vec![first, second]);
    }
}
