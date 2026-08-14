use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use protocol::{CompactionData, EntryId, LaneId, Sequence, SessionId};
use serde::Serialize;

use crate::state::{
    EntryMutation, FactKind, LabelFactMutation, LaneMutation, MutationKind,
    NameFactMutation, RecordMutation, SessionState,
};
use crate::{
    Clock, EntryKind, NewEntry, NewRecord, SessionEntry, SessionRecord,
    SessionStore, StoreError,
};

#[derive(typed_builder::TypedBuilder)]
pub(super) struct JsonlSessionStore {
    session_id: SessionId,
    path: PathBuf,
    clock: Arc<dyn Clock>,
    state: SessionState,
}

impl JsonlSessionStore {
    /// Computes the next shared sequence without mutating durable state early.
    fn candidate_sequence(&self) -> Result<Sequence, StoreError> {
        let next_sequence = self
            .state
            .next_sequence
            .checked_add(1)
            .ok_or(StoreError::SequenceExhausted)?;
        Sequence::try_from(next_sequence)
            .map_err(|_scalar_error| StoreError::SequenceExhausted)
    }

    /// Appends one serialized mutation as a complete JSONL line.
    fn append<T: Serialize>(&self, mutation: &T) -> Result<(), StoreError> {
        let mut file = OpenOptions::new().append(true).open(&self.path)?;
        serde_json::to_writer(&mut file, mutation)?;
        file.write_all(b"\n")?;
        file.sync_data()?;
        Ok(())
    }

    /// Validates a mutation against a candidate state before making it durable.
    fn persist<T: Serialize>(
        &mut self,
        mutation: &T,
    ) -> Result<(), StoreError> {
        let value = serde_json::to_value(mutation)?;
        let mut candidate = self.state.clone();
        Self::apply_loaded_mutation(&mut candidate, value.clone())?;
        self.append(&value)?;
        // State advances only after the complete JSONL line reaches durable storage.
        self.state = candidate;
        Ok(())
    }

    /// Applies one decoded mutation while rebuilding in-memory session state.
    pub(super) fn apply_loaded_mutation(
        state: &mut SessionState,
        value: serde_json::Value,
    ) -> Result<(), StoreError> {
        let kind = value
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                StoreError::InvalidSession("mutation has no kind".to_string())
            })?;
        let sequence = value
            .get("seq")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                StoreError::InvalidSession(
                    "mutation has invalid sequence".to_string(),
                )
            })?;
        if sequence != state.next_sequence.saturating_add(1) {
            return Err(StoreError::InvalidSession(format!(
                "mutation sequence {sequence} is not consecutive after {}",
                state.next_sequence
            )));
        }

        match kind {
            "lane" => {
                let mutation: LaneMutation = serde_json::from_value(value)?;
                if let Some(target) = &mutation.leaf_id
                    && !state.entries.contains_key(target)
                {
                    return Err(StoreError::InvalidSession(format!(
                        "lane {} references missing entry {target}",
                        mutation.lane
                    )));
                }
                state.next_sequence = mutation.sequence.get();
                state.lanes.insert(mutation.lane, mutation.leaf_id);
            }
            "entry" => {
                let mutation: EntryMutation = serde_json::from_value(value)?;
                if mutation.entry.kind == EntryKind::Compaction {
                    let data: CompactionData =
                        serde_json::from_value(serde_json::Value::Object(
                            mutation.entry.payload.clone(),
                        ))
                        .map_err(|error| {
                            StoreError::InvalidSession(format!(
                                "invalid compaction payload: {error}"
                            ))
                        })?;
                    let details = data.details.ok_or_else(|| {
                        StoreError::InvalidSession(
                            "compaction details are required".to_string(),
                        )
                    })?;
                    if data.summary.trim().is_empty() {
                        return Err(StoreError::InvalidSession(
                            "compaction summary must not be empty".to_string(),
                        ));
                    }
                    if details.ended_at_ms < details.started_at_ms {
                        return Err(StoreError::InvalidSession(
                            "compaction ended before it started".to_string(),
                        ));
                    }
                }
                if state.entries.contains_key(&mutation.entry.id) {
                    return Err(StoreError::DuplicateEntry(mutation.entry.id));
                }
                let leaf =
                    state.lanes.get(&mutation.lane).ok_or_else(|| {
                        StoreError::InvalidSession(format!(
                            "entry references missing lane {}",
                            mutation.lane
                        ))
                    })?;
                if leaf != &mutation.entry.parent_id {
                    return Err(StoreError::InvalidSession(
                        "entry does not chain to lane leaf".to_string(),
                    ));
                }
                state.next_sequence = mutation.entry.sequence.get();
                state
                    .lanes
                    .insert(mutation.lane, Some(mutation.entry.id.clone()));
                state.entry_order.push(mutation.entry.id.clone());
                state
                    .entries
                    .insert(mutation.entry.id.clone(), mutation.entry);
            }
            "record" => {
                let mutation: RecordMutation = serde_json::from_value(value)?;
                if !state.lanes.contains_key(&mutation.record.lane) {
                    return Err(StoreError::InvalidSession(format!(
                        "record references missing lane {}",
                        mutation.record.lane
                    )));
                }
                if !state.used_record_ids.insert(mutation.record.id.clone()) {
                    return Err(StoreError::InvalidSession(format!(
                        "duplicate record id {}",
                        mutation.record.id
                    )));
                }
                state.next_sequence = mutation.record.sequence.get();
                state.records.push(mutation.record);
            }
            "fact" => {
                let fact = value
                    .get("fact")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        StoreError::InvalidSession(
                            "fact mutation has no fact type".to_string(),
                        )
                    })?;
                match fact {
                    "name" => {
                        state.name = value
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .map(ToOwned::to_owned);
                    }
                    "label" => {
                        let target = value
                            .get("targetId")
                            .and_then(serde_json::Value::as_str)
                            .ok_or_else(|| {
                                StoreError::InvalidSession(
                                    "label fact has no targetId".to_string(),
                                )
                            })?;
                        let target_id =
                            EntryId::try_from(target).map_err(|error| {
                                StoreError::InvalidSession(error.to_string())
                            })?;
                        if !state.entries.contains_key(&target_id) {
                            return Err(StoreError::EntryNotFound(target_id));
                        }
                        match value
                            .get("label")
                            .and_then(serde_json::Value::as_str)
                        {
                            Some(label) => {
                                state
                                    .labels
                                    .insert(target_id, label.to_string());
                            }
                            None => {
                                state.labels.remove(&target_id);
                            }
                        }
                    }
                    other => {
                        return Err(StoreError::InvalidSession(format!(
                            "unknown fact type {other}"
                        )));
                    }
                }
                state.next_sequence = sequence;
            }
            other => {
                return Err(StoreError::InvalidSession(format!(
                    "unknown mutation kind {other}"
                )));
            }
        }
        Ok(())
    }
}

impl SessionStore for JsonlSessionStore {
    /// Returns the stable session identifier loaded from the header.
    fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    /// Returns the backing JSONL file path.
    fn path(&self) -> &Path {
        &self.path
    }

    /// Creates a lane and persists its initial leaf as a v4 lane mutation.
    fn create_lane(
        &mut self,
        lane: LaneId,
        at: Option<EntryId>,
    ) -> Result<(), StoreError> {
        if self.state.lanes.contains_key(&lane) {
            return Err(StoreError::InvalidLane(lane.to_string()));
        }
        if let Some(entry_id) = &at
            && !self.state.entries.contains_key(entry_id)
        {
            return Err(StoreError::InvalidLane(lane.to_string()));
        }

        let mutation = LaneMutation::builder()
            .kind(MutationKind::Lane)
            .sequence(self.candidate_sequence()?)
            .lane(lane.clone())
            .leaf_id(at.clone())
            .build();
        self.persist(&mutation)?;
        Ok(())
    }

    /// Persists a lane cursor move while retaining every existing entry.
    fn move_lane(
        &mut self,
        lane: &LaneId,
        to: Option<EntryId>,
    ) -> Result<(), StoreError> {
        if !self.state.lanes.contains_key(lane) {
            return Err(StoreError::InvalidLane(lane.to_string()));
        }
        if let Some(entry_id) = &to
            && !self.state.entries.contains_key(entry_id)
        {
            return Err(StoreError::EntryNotFound(entry_id.clone()));
        }
        let mutation = LaneMutation::builder()
            .kind(MutationKind::Lane)
            .sequence(self.candidate_sequence()?)
            .lane(lane.clone())
            .leaf_id(to.clone())
            .build();
        self.persist(&mutation)?;
        Ok(())
    }

    /// Appends an entry using the current lane leaf as its parent.
    fn append_entry(
        &mut self,
        lane: &LaneId,
        entry: NewEntry,
    ) -> Result<SessionEntry, StoreError> {
        if self.state.entries.contains_key(&entry.id) {
            return Err(StoreError::DuplicateEntry(entry.id));
        }
        let parent_id = self
            .state
            .lanes
            .get(lane)
            .cloned()
            .ok_or_else(|| StoreError::InvalidLane(lane.to_string()))?;
        let serde_json::Value::Object(payload) = entry.payload else {
            return Err(StoreError::InvalidEntryPayload);
        };
        let stored = SessionEntry::builder()
            .id(entry.id)
            .sequence(self.candidate_sequence()?)
            .parent_id(parent_id)
            .timestamp_ms(self.clock.now())
            .kind(entry.kind)
            .payload(payload)
            .build();
        let mutation = EntryMutation {
            kind: MutationKind::Entry,
            lane: lane.clone(),
            entry: stored.clone(),
        };
        self.persist(&mutation)?;
        Ok(stored)
    }

    /// Returns the current leaf of a lane.
    fn lane(&self, lane: &LaneId) -> Option<&EntryId> {
        self.state.lanes.get(lane).and_then(Option::as_ref)
    }

    /// Returns one stored entry without cloning its payload.
    fn get_entry(&self, id: &EntryId) -> Option<&SessionEntry> {
        self.state.entries.get(id)
    }

    /// Clones entries in their durable shared-sequence order.
    fn entries(&self) -> Vec<SessionEntry> {
        self.state
            .entry_order
            .iter()
            .filter_map(|id| self.state.entries.get(id).cloned())
            .collect()
    }

    /// Walks parent links from a selected leaf and returns root-to-leaf order.
    fn branch(&self, leaf: &EntryId) -> Result<Vec<SessionEntry>, StoreError> {
        let mut branch = Vec::new();
        let mut visited = HashSet::new();
        let mut cursor = Some(leaf.clone());
        while let Some(entry_id) = cursor {
            if !visited.insert(entry_id.clone()) {
                return Err(StoreError::InvalidSession(format!(
                    "session branch contains a cycle at {entry_id}"
                )));
            }
            let entry =
                self.state.entries.get(&entry_id).cloned().ok_or_else(
                    || StoreError::EntryNotFound(entry_id.clone()),
                )?;
            cursor = entry.parent_id.clone();
            branch.push(entry);
        }
        branch.reverse();
        Ok(branch)
    }

    /// Appends an operation record without changing the lane's current entry leaf.
    fn append_record(
        &mut self,
        record: NewRecord,
    ) -> Result<SessionRecord, StoreError> {
        if !self.state.lanes.contains_key(&record.lane) {
            return Err(StoreError::InvalidLane(record.lane.to_string()));
        }
        if self.state.used_record_ids.contains(&record.id) {
            return Err(StoreError::InvalidSession(format!(
                "duplicate record id {}",
                record.id
            )));
        }
        let serde_json::Value::Object(payload) = record.payload else {
            return Err(StoreError::InvalidEntryPayload);
        };
        let stored = SessionRecord::builder()
            .id(record.id)
            .sequence(self.candidate_sequence()?)
            .lane(record.lane)
            .timestamp_ms(self.clock.now())
            .run_id(record.run_id)
            .kind(record.kind)
            .payload(payload)
            .build();
        self.persist(&RecordMutation {
            kind: MutationKind::Record,
            record: stored.clone(),
        })?;
        Ok(stored)
    }

    /// Clones durable records in their shared-sequence order.
    fn records(&self) -> Vec<SessionRecord> {
        self.state.records.clone()
    }

    /// Persists the current optional session name as a global fact mutation.
    fn set_name(&mut self, name: Option<String>) -> Result<(), StoreError> {
        let mutation = NameFactMutation::builder()
            .kind(MutationKind::Fact)
            .sequence(self.candidate_sequence()?)
            .fact(FactKind::Name)
            .name(name.clone())
            .build();
        self.persist(&mutation)?;
        Ok(())
    }

    /// Returns the latest persisted session name.
    fn name(&self) -> Option<&str> {
        self.state.name.as_deref()
    }

    /// Persists an optional label for an existing entry.
    fn set_label(
        &mut self,
        target_id: EntryId,
        label: Option<String>,
    ) -> Result<(), StoreError> {
        if !self.state.entries.contains_key(&target_id) {
            return Err(StoreError::EntryNotFound(target_id));
        }
        let mutation = LabelFactMutation::builder()
            .kind(MutationKind::Fact)
            .sequence(self.candidate_sequence()?)
            .fact(FactKind::Label)
            .target_id(target_id.clone())
            .label(label.clone())
            .build();
        self.persist(&mutation)?;
        Ok(())
    }

    /// Returns the latest persisted label for one entry.
    fn label(&self, target_id: &EntryId) -> Option<&str> {
        self.state.labels.get(target_id).map(String::as_str)
    }
}
